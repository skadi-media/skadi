//! The Cardigann template engine (SKADI-T-0256): a small interpreter for the
//! Go-template **subset** the definitions actually use — variable substitution
//! (`.Keywords`/`.Config.x`/`.Result.x`/`.Categories`/`.Query.x`/`.True`/`.False`),
//! `{{ if … }}…{{ else }}…{{ end }}`, the boolean/`eq`/`ne` builtins, and `join`.
//! Pure + total (errors, never panics). Unsupported constructs surface a
//! [`TemplateError`] so the caller can warn-skip the affected definition.

use std::collections::BTreeMap;

/// A rendered template value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    List(Vec<String>),
    Bool(bool),
}

impl Value {
    /// Go-template truthiness: a non-empty string/list, or `true`.
    fn truthy(&self) -> bool {
        match self {
            Value::Str(s) => !s.is_empty(),
            Value::List(l) => !l.is_empty(),
            Value::Bool(b) => *b,
        }
    }

    /// Rendered into a template's output string.
    fn render(&self) -> String {
        match self {
            Value::Str(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            // Go renders a slice as "[a b c]"; cardigann always wraps lists in
            // `join`, so this is just a sane fallback.
            Value::List(l) => format!("[{}]", l.join(" ")),
        }
    }

    fn str(s: impl Into<String>) -> Value {
        Value::Str(s.into())
    }
}

/// The variable scope a template renders against.
#[derive(Debug, Clone, Default)]
pub struct TemplateContext {
    pub keywords: String,
    pub categories: Vec<String>,
    /// Per-indexer config values (`text` settings are `Str`, `checkbox` are `Bool`).
    pub config: BTreeMap<String, Value>,
    /// Query params (`imdbid`, `season`, …).
    pub query: BTreeMap<String, String>,
    /// Fields already extracted for the current row (`.Result.<name>`).
    pub result: BTreeMap<String, String>,
}

impl TemplateContext {
    /// Look up a `.Query.<key>` value, ignoring case.
    ///
    /// Cardigann definitions in the wild spell these in the Jackett spec's own
    /// casing — `.Query.IMDBID`, `.Query.Season`, `.Query.Ep` — while the adapter
    /// fills the map with lowercase keys. An exact-match lookup therefore rendered
    /// every id and episode variable as the empty string, so id search and
    /// season/episode search never reached the tracker at all (SKADI-T-0498).
    /// Matching case-insensitively accepts both spellings; a definition using the
    /// lowercase form keeps working unchanged.
    fn query_value(&self, key: &str) -> String {
        if let Some(v) = self.query.get(key) {
            return v.clone();
        }
        self.query
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }

    /// Resolve a dotted variable path (`["Config","apiurl"]`) to a value.
    fn lookup(&self, path: &[String]) -> Result<Value, TemplateError> {
        match path {
            [root] => match root.as_str() {
                "Keywords" => Ok(Value::str(&self.keywords)),
                "Categories" => Ok(Value::List(self.categories.clone())),
                "True" => Ok(Value::Bool(true)),
                "False" => Ok(Value::Bool(false)),
                // A bare `.Result`/`.Config` is never used standalone; treat any
                // other single root as an empty string (Go's zero value).
                _ => Ok(Value::str("")),
            },
            [root, key] => match root.as_str() {
                "Config" => Ok(self.config.get(key).cloned().unwrap_or(Value::str(""))),
                "Query" => Ok(Value::str(self.query_value(key))),
                "Result" => Ok(Value::str(
                    self.result.get(key).cloned().unwrap_or_default(),
                )),
                "Today" => Ok(Value::str("")), // date sub-fields: best-effort empty for now
                other => Err(TemplateError(format!("unknown variable root '.{other}'"))),
            },
            _ => Err(TemplateError(format!("unsupported variable path {path:?}"))),
        }
    }
}

/// A template that could not be parsed or rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateError(pub String);

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "template error: {}", self.0)
    }
}
impl std::error::Error for TemplateError {}

/// Render `input` against `ctx`. Plain text passes through; `{{ … }}` actions are
/// evaluated.
///
/// # Errors
/// [`TemplateError`] on a malformed action or an unsupported construct.
pub fn render(input: &str, ctx: &TemplateContext) -> Result<String, TemplateError> {
    let nodes = parse(input)?;
    let mut out = String::new();
    render_nodes(&nodes, ctx, &mut out)?;
    Ok(out)
}

/// Evaluate a template that is expected to yield a single [`Value`] (e.g. a
/// condition or a `join`), rather than rendered text — used by selector logic.
///
/// # Errors
/// [`TemplateError`] if the input is not a single `{{ expr }}` action.
pub fn eval_expr(action: &str, ctx: &TemplateContext) -> Result<Value, TemplateError> {
    let expr = parse_expr(&tokenize_action(action)?)?;
    eval(&expr, ctx)
}

// --- AST ---------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Node {
    Text(String),
    Subst(Expr),
    If {
        cond: Expr,
        then: Vec<Node>,
        els: Vec<Node>,
    },
}

#[derive(Debug, Clone)]
enum Expr {
    Var(Vec<String>),
    Str(String),
    Call(String, Vec<Expr>),
}

// --- Parser ------------------------------------------------------------------

/// Split a template into text + `{{ action }}` actions, then parse control flow.
fn parse(input: &str) -> Result<Vec<Node>, TemplateError> {
    let actions = lex(input)?;
    let mut pos = 0;
    parse_block(&actions, &mut pos, &[])
}

/// One lexed chunk: literal text, or the inner string of a `{{ … }}` action.
enum Lex {
    Text(String),
    Action(String),
}

fn lex(input: &str) -> Result<Vec<Lex>, TemplateError> {
    let mut out = Vec::new();
    let mut rest = input;
    while let Some(start) = rest.find("{{") {
        if start > 0 {
            out.push(Lex::Text(rest[..start].to_string()));
        }
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .ok_or_else(|| TemplateError("unterminated '{{'".into()))?;
        let mut action = after[..end].trim();
        // Whitespace-trim markers `{{- … -}}` — accepted, no surrounding text here.
        action = action.trim_start_matches('-').trim_end_matches('-').trim();
        out.push(Lex::Action(action.to_string()));
        rest = &after[end + 2..];
    }
    if !rest.is_empty() {
        out.push(Lex::Text(rest.to_string()));
    }
    Ok(out)
}

/// Parse a run of lexed chunks into nodes, stopping at any terminator in `stop`
/// (`else`/`end`). Returns the nodes; `pos` is left pointing at the terminator.
fn parse_block(
    actions: &[Lex],
    pos: &mut usize,
    stop: &[&str],
) -> Result<Vec<Node>, TemplateError> {
    let mut nodes = Vec::new();
    while *pos < actions.len() {
        match &actions[*pos] {
            Lex::Text(t) => {
                nodes.push(Node::Text(t.clone()));
                *pos += 1;
            }
            Lex::Action(a) => {
                let head = a.split_whitespace().next().unwrap_or("");
                if stop.contains(&head) {
                    return Ok(nodes);
                }
                match head {
                    "if" => {
                        *pos += 1;
                        let cond = parse_expr(&tokenize_action(a[2..].trim())?)?;
                        let then = parse_block(actions, pos, &["else", "end"])?;
                        let mut els = Vec::new();
                        if matches!(actions.get(*pos), Some(Lex::Action(x)) if x.starts_with("else"))
                        {
                            *pos += 1;
                            els = parse_block(actions, pos, &["end"])?;
                        }
                        // consume the matching `end`
                        if !matches!(actions.get(*pos), Some(Lex::Action(x)) if x.trim() == "end") {
                            return Err(TemplateError("missing '{{ end }}'".into()));
                        }
                        *pos += 1;
                        nodes.push(Node::If { cond, then, els });
                    }
                    "range" | "with" | "template" | "block" => {
                        return Err(TemplateError(format!("unsupported action '{head}'")));
                    }
                    _ => {
                        *pos += 1;
                        nodes.push(Node::Subst(parse_expr(&tokenize_action(a)?)?));
                    }
                }
            }
        }
    }
    Ok(nodes)
}

/// Tokens of a single action's expression.
#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Var(Vec<String>),
    Str(String),
    Ident(String),
    LParen,
    RParen,
}

fn tokenize_action(s: &str) -> Result<Vec<Tok>, TemplateError> {
    let mut toks = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '(' => {
                chars.next();
                toks.push(Tok::LParen);
            }
            ')' => {
                chars.next();
                toks.push(Tok::RParen);
            }
            '"' | '\'' => {
                let quote = c;
                chars.next();
                let mut lit = String::new();
                let mut escaped = false;
                for ch in chars.by_ref() {
                    if escaped {
                        lit.push(ch);
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == quote {
                        break;
                    } else {
                        lit.push(ch);
                    }
                }
                toks.push(Tok::Str(lit));
            }
            '.' => {
                chars.next();
                let mut path = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_alphanumeric() || ch == '_' || ch == '.' {
                        path.push(ch);
                        chars.next();
                    } else {
                        break;
                    }
                }
                toks.push(Tok::Var(path.split('.').map(str::to_string).collect()));
            }
            _ => {
                let mut ident = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_whitespace() || ch == '(' || ch == ')' {
                        break;
                    }
                    ident.push(ch);
                    chars.next();
                }
                toks.push(Tok::Ident(ident));
            }
        }
    }
    Ok(toks)
}

/// Parse a whole token stream as one expression (a function applies to the rest).
fn parse_expr(toks: &[Tok]) -> Result<Expr, TemplateError> {
    let mut pos = 0;
    let e = parse_expr_at(toks, &mut pos)?;
    if pos != toks.len() {
        return Err(TemplateError("trailing tokens in expression".into()));
    }
    Ok(e)
}

/// Parse one expression: a primary, or `funcname arg arg …` (greedy to the end of
/// the current group).
fn parse_expr_at(toks: &[Tok], pos: &mut usize) -> Result<Expr, TemplateError> {
    let first = parse_primary(toks, pos)?;
    // A leading identifier is a function call over the remaining args at this level.
    if let Expr::Call(name, _) = &first
        && is_builtin(name)
    {
        let mut args = Vec::new();
        while *pos < toks.len() && !matches!(toks[*pos], Tok::RParen) {
            args.push(parse_primary(toks, pos)?);
        }
        return Ok(Expr::Call(name.clone(), args));
    }
    Ok(first)
}

fn parse_primary(toks: &[Tok], pos: &mut usize) -> Result<Expr, TemplateError> {
    let tok = toks
        .get(*pos)
        .ok_or_else(|| TemplateError("unexpected end of expression".into()))?;
    match tok {
        Tok::Var(p) => {
            *pos += 1;
            Ok(Expr::Var(p.clone()))
        }
        Tok::Str(s) => {
            *pos += 1;
            Ok(Expr::Str(s.clone()))
        }
        Tok::Ident(name) => {
            *pos += 1;
            // Represent as an (as-yet-argless) call; parse_expr_at fills the args.
            Ok(Expr::Call(name.clone(), Vec::new()))
        }
        Tok::LParen => {
            *pos += 1;
            let inner = parse_expr_at(toks, pos)?;
            if !matches!(toks.get(*pos), Some(Tok::RParen)) {
                return Err(TemplateError("missing ')'".into()));
            }
            *pos += 1;
            Ok(inner)
        }
        Tok::RParen => Err(TemplateError("unexpected ')'".into())),
    }
}

fn is_builtin(name: &str) -> bool {
    matches!(name, "and" | "or" | "not" | "eq" | "ne" | "join")
}

// --- Eval --------------------------------------------------------------------

fn render_nodes(
    nodes: &[Node],
    ctx: &TemplateContext,
    out: &mut String,
) -> Result<(), TemplateError> {
    for n in nodes {
        match n {
            Node::Text(t) => out.push_str(t),
            Node::Subst(e) => out.push_str(&eval(e, ctx)?.render()),
            Node::If { cond, then, els } => {
                if eval(cond, ctx)?.truthy() {
                    render_nodes(then, ctx, out)?;
                } else {
                    render_nodes(els, ctx, out)?;
                }
            }
        }
    }
    Ok(())
}

fn eval(expr: &Expr, ctx: &TemplateContext) -> Result<Value, TemplateError> {
    match expr {
        Expr::Var(path) => ctx.lookup(path),
        Expr::Str(s) => Ok(Value::str(s)),
        Expr::Call(name, args) => eval_call(name, args, ctx),
    }
}

fn eval_call(name: &str, args: &[Expr], ctx: &TemplateContext) -> Result<Value, TemplateError> {
    let val = |e: &Expr| eval(e, ctx);
    match name {
        "and" => {
            let mut last = Value::Bool(true);
            for a in args {
                last = val(a)?;
                if !last.truthy() {
                    return Ok(last);
                }
            }
            Ok(last)
        }
        "or" => {
            let mut last = Value::Bool(false);
            for a in args {
                last = val(a)?;
                if last.truthy() {
                    return Ok(last);
                }
            }
            Ok(last)
        }
        "not" => {
            let a = args
                .first()
                .ok_or_else(|| TemplateError("not: missing arg".into()))?;
            Ok(Value::Bool(!val(a)?.truthy()))
        }
        "eq" | "ne" => {
            if args.len() != 2 {
                return Err(TemplateError(format!("{name}: needs 2 args")));
            }
            let equal = values_eq(&val(&args[0])?, &val(&args[1])?);
            Ok(Value::Bool(if name == "eq" { equal } else { !equal }))
        }
        "join" => {
            if args.len() != 2 {
                return Err(TemplateError("join: needs a list and a separator".into()));
            }
            let list = match val(&args[0])? {
                Value::List(l) => l,
                Value::Str(s) => vec![s],
                Value::Bool(b) => vec![b.to_string()],
            };
            let sep = val(&args[1])?.render();
            Ok(Value::str(list.join(&sep)))
        }
        other => Err(TemplateError(format!("unsupported function '{other}'"))),
    }
}

/// Go-template `eq`: same type + same value (a `Str` and a `Bool` are never equal,
/// which is what `eq .Config.flag .False` relies on).
fn values_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::List(x), Value::List(y)) => x == y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> TemplateContext {
        let mut config = BTreeMap::new();
        config.insert("apiurl".into(), Value::str("apibay.org"));
        config.insert("disablesort".into(), Value::Bool(false));
        config.insert("sort".into(), Value::str("time"));
        TemplateContext {
            keywords: "the matrix".into(),
            categories: vec!["2000".into(), "5000".into()],
            config,
            ..Default::default()
        }
    }

    #[test]
    fn substitutes_variables_and_join() {
        let t =
            "https://{{ .Config.apiurl }}/q.php?q={{ .Keywords }}&cat={{ join .Categories \",\" }}";
        assert_eq!(
            render(t, &ctx()).unwrap(),
            "https://apibay.org/q.php?q=the matrix&cat=2000,5000"
        );
    }

    #[test]
    fn if_else_with_keywords() {
        let t = "{{ if .Keywords }}search/{{ .Keywords }}{{ else }}cat/Movies{{ end }}/1/";
        assert_eq!(render(t, &ctx()).unwrap(), "search/the matrix/1/");
        let empty = TemplateContext::default();
        assert_eq!(render(t, &empty).unwrap(), "cat/Movies/1/");
    }

    #[test]
    fn nested_and_eq_false_constant() {
        // The 1337x pattern: render "sort-" only when keywords present AND sort enabled.
        let t =
            "{{ if and (.Keywords) (eq .Config.disablesort .False) }}sort-{{ else }}{{ end }}go";
        assert_eq!(render(t, &ctx()).unwrap(), "sort-go");
        // Disable sort → the `eq … .False` is false → branch skipped.
        let mut c = ctx();
        c.config.insert("disablesort".into(), Value::Bool(true));
        assert_eq!(render(t, &c).unwrap(), "go");
    }

    #[test]
    fn result_field_reference() {
        let mut c = TemplateContext::default();
        c.config.insert("sitelink".into(), Value::str("https://x/"));
        c.result.insert("_id".into(), "42".into());
        let t = "{{ .Config.sitelink }}description.php?id={{ .Result._id }}";
        assert_eq!(render(t, &c).unwrap(), "https://x/description.php?id=42");
    }

    #[test]
    fn unsupported_construct_errs_not_panics() {
        assert!(render("{{ range .Categories }}x{{ end }}", &ctx()).is_err());
        assert!(render("{{ .Keywords ", &ctx()).is_err());
    }
}
