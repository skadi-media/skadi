//! `skadi-aurora-css <dir>`: writes Aurora's `aurora.css` and `hyper.css`
//! into `<dir>` (SKADI-T-0717). A file whose text has not changed is not
//! written again, so `trunk serve` does not see a change and build again.

use std::path::Path;

fn write_if_changed(dir: &Path, name: &str, css: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    if std::fs::read_to_string(&path).is_ok_and(|old| old == css) {
        return Ok(());
    }
    std::fs::write(&path, css)
}

fn main() -> std::io::Result<()> {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "style".to_string());
    let dir = Path::new(&dir);
    write_if_changed(dir, "aurora.css", aurora_leptos::AURORA_CSS)?;
    write_if_changed(dir, "hyper.css", aurora_leptos::HYPER_CSS)?;
    Ok(())
}
