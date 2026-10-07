//! Embeds the build commit for `GET /system/status` (SKADI-T-0684).
//!
//! Order of trust:
//! 1. `SKADI_BUILD_COMMIT` from the environment. The Docker build sets it from
//!    a build arg, because `.git/` is not in the build context.
//! 2. `git rev-parse HEAD` when the source is a git checkout.
//! 3. `unknown`. A build with neither still compiles and does not invent a
//!    commit.

use std::path::PathBuf;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SKADI_BUILD_COMMIT");

    let from_env = std::env::var("SKADI_BUILD_COMMIT")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let commit = from_env.or_else(|| {
        let sha = git(&["rev-parse", "--short=12", "HEAD"])?;
        // Rebuild when HEAD moves: HEAD itself (a checkout) and the ref it
        // points at (a commit). `--git-path` resolves both correctly inside a
        // linked worktree, where the refs live in the common dir. A path that
        // does not exist is not emitted, since cargo would then rerun this on
        // every build.
        let mut watch = vec![git(&["rev-parse", "--git-path", "HEAD"])];
        if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
            watch.push(git(&["rev-parse", "--git-path", &r]));
        }
        watch.push(git(&["rev-parse", "--git-path", "packed-refs"]));
        for p in watch.into_iter().flatten().map(PathBuf::from) {
            if p.exists() {
                println!("cargo:rerun-if-changed={}", p.display());
            }
        }
        Some(sha)
    });

    println!(
        "cargo:rustc-env=SKADI_BUILD_COMMIT={}",
        commit.as_deref().unwrap_or("unknown")
    );
}
