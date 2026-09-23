//! Cardigann login-flow executor (SKADI-T-0258): authenticate a private tracker
//! before searching it. Supports the `form` / `post` / `get` / `oneurl` methods,
//! renders the `login.inputs` from config, checks the `login.error` selectors,
//! and verifies the session with `login.test`. Cookies are carried by the
//! injected [`Fetcher`] (its impl owns a per-indexer jar), so a successful login
//! authenticates every subsequent request.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use scraper::{Html, Selector};

use crate::engine::{EngineError, FetchReq, Fetcher, Method};
use crate::model::{Definition, Login};
use crate::template::{self, TemplateContext, Value as TVal};

/// The result of attempting a login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginOutcome {
    /// The definition needs no login (public).
    NotRequired,
    /// Authenticated (the session cookie is now in the fetcher's jar).
    Ok,
    /// Login ran but failed; carries the tracker's reason if one was matched.
    Failed(String),
}

/// Authenticate `def` against the tracker using `config` (username/password/…).
///
/// # Errors
/// [`EngineError`] on a template/selector/fetch failure (a *rejected* login is
/// `Ok(LoginOutcome::Failed)`, not an `Err`).
pub async fn login<F: Fetcher + ?Sized>(
    def: &Definition,
    config: &BTreeMap<String, TVal>,
    query: &BTreeMap<String, String>,
    base_url: &str,
    fetcher: &F,
    now: DateTime<Utc>,
) -> Result<LoginOutcome, EngineError> {
    let Some(lg) = &def.login else {
        return Ok(LoginOutcome::NotRequired);
    };
    let ctx = TemplateContext {
        keywords: String::new(),
        categories: Vec::new(),
        config: config.clone(),
        query: query.clone(),
        result: BTreeMap::new(),
    };

    let method = lg.method.as_deref().unwrap_or("form");
    let login_url = abs(base_url, &render(lg.path.as_deref().unwrap_or(""), &ctx)?);

    let response_html = match method {
        "form" => submit_form(lg, &login_url, base_url, &ctx, fetcher).await?,
        "post" => {
            let body = encode_inputs(lg, &ctx)?;
            let resp = fetcher
                .fetch(FetchReq {
                    method: Method::Post,
                    url: login_url.clone(),
                    headers: Vec::new(),
                    body: Some(body),
                })
                .await
                .map_err(|e| EngineError::Fetch(e.0))?;
            resp.body
        }
        "get" => {
            let qs = encode_inputs(lg, &ctx)?;
            let url = if qs.is_empty() {
                login_url.clone()
            } else {
                format!("{login_url}?{qs}")
            };
            get(fetcher, &url).await?
        }
        // `oneurl`: a single GET establishes the session; nothing to submit.
        "oneurl" => get(fetcher, &login_url).await?,
        other => {
            return Err(EngineError::Response(format!(
                "unsupported login method '{other}'"
            )));
        }
    };

    // A matched error selector means the tracker rejected us.
    if let Some(reason) = matched_error(lg, &response_html, &ctx)? {
        return Ok(LoginOutcome::Failed(reason));
    }

    // Verify with the `test` block, if present.
    if let Some(test) = &lg.test
        && !verify_test(test, base_url, fetcher, now).await?
    {
        return Ok(LoginOutcome::Failed(
            "login test selector not found (credentials likely wrong)".into(),
        ));
    }
    Ok(LoginOutcome::Ok)
}

/// `method: form` — GET the login page, read the form's action + hidden inputs,
/// merge the definition's inputs over them, and POST.
async fn submit_form<F: Fetcher + ?Sized>(
    lg: &Login,
    login_url: &str,
    base_url: &str,
    ctx: &TemplateContext,
    fetcher: &F,
) -> Result<String, EngineError> {
    let page = get(fetcher, login_url).await?;
    let (action, mut fields) = parse_form(&page, lg.form.as_deref(), base_url, login_url)?;
    // Definition inputs override the scraped defaults.
    for (k, v) in rendered_inputs(lg, ctx)? {
        fields.insert(k, v);
    }
    let body = form_urlencode(&fields);
    let resp = fetcher
        .fetch(FetchReq {
            method: Method::Post,
            url: action,
            headers: Vec::new(),
            body: Some(body),
        })
        .await
        .map_err(|e| EngineError::Fetch(e.0))?;
    Ok(resp.body)
}

/// Extract a form's submit URL + its current `name=value` inputs.
fn parse_form(
    html: &str,
    form_selector: Option<&str>,
    base_url: &str,
    page_url: &str,
) -> Result<(String, BTreeMap<String, String>), EngineError> {
    let doc = Html::parse_document(html);
    let sel = form_selector.unwrap_or("form");
    let selector = Selector::parse(sel)
        .map_err(|e| EngineError::Selector(format!("bad form selector: {e}")))?;
    let form = doc
        .select(&selector)
        .next()
        .ok_or_else(|| EngineError::Response(format!("login form '{sel}' not found")))?;
    let action_attr = form.value().attr("action").unwrap_or("");
    let action = if action_attr.is_empty() {
        page_url.to_string()
    } else {
        abs(base_url, action_attr)
    };
    let input_sel = Selector::parse("input").unwrap();
    let mut fields = BTreeMap::new();
    for input in form.select(&input_sel) {
        if let Some(name) = input.value().attr("name") {
            fields.insert(
                name.to_string(),
                input.value().attr("value").unwrap_or("").to_string(),
            );
        }
    }
    Ok((action, fields))
}

/// Render the definition's `login.inputs` as `(name, value)` pairs.
fn rendered_inputs(
    lg: &Login,
    ctx: &TemplateContext,
) -> Result<Vec<(String, String)>, EngineError> {
    let mut out = Vec::new();
    for (k, v) in &lg.inputs {
        let tpl = v.as_str().unwrap_or_default();
        out.push((k.clone(), render(tpl, ctx)?));
    }
    Ok(out)
}

fn encode_inputs(lg: &Login, ctx: &TemplateContext) -> Result<String, EngineError> {
    let fields: BTreeMap<String, String> = rendered_inputs(lg, ctx)?.into_iter().collect();
    Ok(form_urlencode(&fields))
}

/// Whether any `login.error` selector matches → the matched message (or a default).
fn matched_error(
    lg: &Login,
    html: &str,
    ctx: &TemplateContext,
) -> Result<Option<String>, EngineError> {
    let doc = Html::parse_document(html);
    for err in &lg.error {
        let Some(sel_str) = err.get("selector").and_then(|v| v.as_str()) else {
            continue;
        };
        // `:contains(...)` is a jQuery-ism scraper can't parse — strip it and
        // match the base selector (best-effort, never abort).
        let base = sel_str.split(":contains").next().unwrap_or(sel_str).trim();
        let Ok(selector) = Selector::parse(base) else {
            continue;
        };
        if doc.select(&selector).next().is_some() {
            let msg = err
                .get("message")
                .and_then(|m| m.get("text"))
                .and_then(|t| t.as_str())
                .map(|t| render(t, ctx))
                .transpose()?
                .unwrap_or_else(|| "login rejected".into());
            return Ok(Some(msg));
        }
    }
    Ok(None)
}

/// Run `login.test`: GET its path and confirm the selector is present.
async fn verify_test<F: Fetcher + ?Sized>(
    test: &crate::model::Yaml,
    base_url: &str,
    fetcher: &F,
    _now: DateTime<Utc>,
) -> Result<bool, EngineError> {
    let path = test.get("path").and_then(|v| v.as_str()).unwrap_or("/");
    let url = abs(base_url, path);
    let html = get(fetcher, &url).await?;
    let Some(sel_str) = test.get("selector").and_then(|v| v.as_str()) else {
        // No selector ⇒ a 2xx fetch is enough.
        return Ok(true);
    };
    let base = sel_str.split(":contains").next().unwrap_or(sel_str).trim();
    let doc = Html::parse_document(&html);
    match Selector::parse(base) {
        Ok(selector) => Ok(doc.select(&selector).next().is_some()),
        Err(_) => Ok(true),
    }
}

async fn get<F: Fetcher + ?Sized>(fetcher: &F, url: &str) -> Result<String, EngineError> {
    let resp = fetcher
        .fetch(FetchReq {
            method: Method::Get,
            url: url.to_string(),
            headers: Vec::new(),
            body: None,
        })
        .await
        .map_err(|e| EngineError::Fetch(e.0))?;
    Ok(resp.body)
}

fn render(tpl: &str, ctx: &TemplateContext) -> Result<String, EngineError> {
    template::render(tpl, ctx).map_err(|e| EngineError::Template(e.to_string()))
}

/// Resolve a possibly-relative URL against the site base.
fn abs(base_url: &str, url: &str) -> String {
    if url.starts_with("http") || base_url.is_empty() {
        url.to_string()
    } else {
        format!(
            "{}/{}",
            base_url.trim_end_matches('/'),
            url.trim_start_matches('/')
        )
    }
}

/// `application/x-www-form-urlencoded` body from sorted fields.
fn form_urlencode(fields: &BTreeMap<String, String>) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for (k, v) in fields {
        if !s.is_empty() {
            s.push('&');
        }
        let _ = write!(s, "{}={}", enc(k), enc(v));
    }
    s
}

fn enc(s: &str) -> String {
    use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{FetchError, FetchResp};
    use std::sync::Mutex;

    /// A scripted fetcher: returns canned bodies by URL substring + records calls.
    struct Scripted {
        routes: Vec<(&'static str, u16, &'static str)>,
        calls: Mutex<Vec<(String, bool)>>, // (url, is_post)
    }

    #[async_trait::async_trait]
    impl Fetcher for Scripted {
        async fn fetch(&self, req: FetchReq) -> Result<FetchResp, FetchError> {
            self.calls
                .lock()
                .unwrap()
                .push((req.url.clone(), req.method == Method::Post));
            for (frag, status, body) in &self.routes {
                if req.url.contains(frag) {
                    return Ok(FetchResp {
                        status: *status,
                        final_url: req.url,
                        body: (*body).to_string(),
                    });
                }
            }
            Ok(FetchResp {
                status: 404,
                final_url: req.url,
                body: String::new(),
            })
        }
    }

    fn def_with_login() -> Definition {
        let yaml = r#"
id: priv
name: Priv
type: private
links: [https://priv.test/]
login:
  path: login.php
  method: form
  form: form#login
  inputs:
    username: "{{ .Config.username }}"
    password: "{{ .Config.password }}"
  error:
    - selector: p.error
  test:
    path: /
    selector: a.logout
caps:
  modes: {search: [q]}
search:
  rows:
    selector: tr
"#;
        crate::parse_definition(yaml).unwrap()
    }

    fn config() -> BTreeMap<String, TVal> {
        let mut c = BTreeMap::new();
        c.insert("username".into(), TVal::Str("alice".into()));
        c.insert("password".into(), TVal::Str("hunter2".into()));
        c
    }

    #[tokio::test]
    async fn form_login_succeeds_when_test_selector_present() {
        let def = def_with_login();
        let f = Scripted {
            routes: vec![
                (
                    "login.php",
                    200,
                    r#"<form id="login" action="/takelogin.php"><input name="token" value="abc"/></form>"#,
                ),
                ("takelogin.php", 200, "<html>ok</html>"),
                // test path "/" → logged-in marker present
                ("priv.test/", 200, r#"<a class="logout">logout</a>"#),
            ],
            calls: Mutex::new(Vec::new()),
        };
        let out = login(
            &def,
            &config(),
            &BTreeMap::new(),
            "https://priv.test",
            &f,
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(out, LoginOutcome::Ok);
        // It POSTed the credentials to the form action.
        let calls = f.calls.lock().unwrap();
        assert!(
            calls
                .iter()
                .any(|(u, post)| u.contains("takelogin.php") && *post)
        );
    }

    #[tokio::test]
    async fn form_login_fails_on_error_selector() {
        let def = def_with_login();
        let f = Scripted {
            routes: vec![
                (
                    "login.php",
                    200,
                    r#"<form id="login" action="/takelogin.php"></form>"#,
                ),
                ("takelogin.php", 200, r#"<p class="error">bad password</p>"#),
            ],
            calls: Mutex::new(Vec::new()),
        };
        let out = login(
            &def,
            &config(),
            &BTreeMap::new(),
            "https://priv.test",
            &f,
            Utc::now(),
        )
        .await
        .unwrap();
        assert!(matches!(out, LoginOutcome::Failed(_)));
    }

    #[tokio::test]
    async fn public_definition_needs_no_login() {
        let def = crate::parse_definition("id: x\nname: X\ntype: public\n").unwrap();
        let out = login(
            &def,
            &config(),
            &BTreeMap::new(),
            "https://x",
            &NoFetch,
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(out, LoginOutcome::NotRequired);
    }

    struct NoFetch;
    #[async_trait::async_trait]
    impl Fetcher for NoFetch {
        async fn fetch(&self, _req: FetchReq) -> Result<FetchResp, FetchError> {
            panic!("should not fetch for a public definition");
        }
    }
}
