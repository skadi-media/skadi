//! FlareSolverr client (SKADI-T-0259): solve a CloudFlare/DDoS-Guard challenge by
//! proxying the request through a headless-Chromium [FlareSolverr] instance, which
//! returns the solved page HTML plus the `cf_clearance` cookie + user-agent. The
//! one external helper Skadi keeps after dropping Prowlarr — opt-in via the
//! `flaresolverr_url` config (empty ⇒ disabled).
//!
//! [FlareSolverr]: https://github.com/FlareSolverr/FlareSolverr

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::json;
use tokio::sync::Semaphore;

/// Process-wide cap on concurrent solves (SKADI-T-0488).
///
/// **Process-wide, not per-indexer**: every `FlareSolverr` here points at the
/// same container, which runs a single Chromium. A per-instance limit would let
/// nineteen configured indexers queue nineteen solves and cap nothing that
/// matters.
///
/// On 2026-09-06 the hunter queued twelve at once; each took tens of seconds,
/// the healthcheck declared the solver dead, and autoheal killed Chromium
/// mid-solve — the restart then failed and left it `Exited (128)`. The
/// healthcheck fix gives it room to answer; this stops the burst arriving.
static SOLVE_SLOTS: OnceLock<Semaphore> = OnceLock::new();

/// Default concurrent solves. Deliberately small: FlareSolverr drives one
/// browser, so parallel challenges contend for the same Chromium and each one
/// gets *slower*, pushing the whole batch toward the timeout that started this.
/// Two keeps a second request warm without that.
const DEFAULT_MAX_CONCURRENT_SOLVES: usize = 2;

/// Set the concurrent-solve cap. Call once, before any solve.
///
/// Later calls are ignored rather than panicking: the value comes from config
/// that a reload may re-read, and a settings save must not take down the daemon
/// over a number that has not changed. `0` is treated as `1` — a cap of zero
/// would deadlock every search rather than disabling the solver, which is what
/// clearing `flaresolverr_url` is for.
pub fn set_max_concurrent(n: usize) {
    let _ = SOLVE_SLOTS.set(Semaphore::new(n.max(1)));
}

fn slots() -> &'static Semaphore {
    SOLVE_SLOTS.get_or_init(|| Semaphore::new(DEFAULT_MAX_CONCURRENT_SOLVES))
}

/// A solved response from FlareSolverr.
#[derive(Debug, Clone)]
pub struct Solved {
    pub status: u16,
    /// The fully-rendered page HTML.
    pub body: String,
    /// The browser user-agent the clearance cookie is bound to (must be replayed
    /// on later direct requests for the `cf_clearance` cookie to be accepted).
    pub user_agent: String,
    /// Cookies set by the solve (`cf_clearance`, …) — `(name, value)`.
    pub cookies: Vec<(String, String)>,
}

/// A configured FlareSolverr endpoint.
#[derive(Clone)]
pub struct FlareSolverr {
    endpoint: String,
    client: reqwest::Client,
}

impl FlareSolverr {
    /// Build a client for `base_url` (its `/v1` endpoint is used).
    #[must_use]
    pub fn new(base_url: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(75))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            endpoint: format!("{}/v1", base_url.trim_end_matches('/')),
            client,
        }
    }

    /// Solve `url` (GET, or POST with `post_data`) and return the rendered page.
    ///
    /// # Errors
    /// A human-readable string if the request or the solve fails.
    pub async fn solve(
        &self,
        url: &str,
        post_data: Option<&str>,
    ) -> std::result::Result<Solved, String> {
        // Wait for a slot before building the request (SKADI-T-0488). Held for
        // the whole round trip, so the cap bounds solves *in flight* at the
        // solver rather than requests issued from here.
        let _permit = slots()
            .acquire()
            .await
            .map_err(|_| "flaresolverr: solve slots closed".to_string())?;
        let mut req = json!({
            "cmd": if post_data.is_some() { "request.post" } else { "request.get" },
            "url": url,
            "maxTimeout": 60_000,
        });
        if let Some(pd) = post_data {
            req["postData"] = json!(pd);
        }
        let resp = self
            .client
            .post(&self.endpoint)
            .json(&req)
            .send()
            .await
            .map_err(|e| format!("flaresolverr request: {e}"))?;
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("flaresolverr decode: {e}"))?;
        if v.get("status").and_then(|s| s.as_str()) != Some("ok") {
            return Err(format!(
                "flaresolverr: {}",
                v.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("not ok")
            ));
        }
        let sol = &v["solution"];
        let cookies = sol["cookies"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| {
                        Some((
                            c["name"].as_str()?.to_string(),
                            c["value"].as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Solved {
            status: u16::try_from(sol["status"].as_u64().unwrap_or(0)).unwrap_or(0),
            body: sol["response"].as_str().unwrap_or("").to_string(),
            user_agent: sol["userAgent"].as_str().unwrap_or("").to_string(),
            cookies,
        })
    }
}

/// Heuristic: does this response look like an unsolved CloudFlare / DDoS-Guard
/// challenge that FlareSolverr should handle?
#[must_use]
pub fn is_challenge(status: u16, body: &str) -> bool {
    if !matches!(status, 403 | 429 | 503) {
        return false;
    }
    // A **block** is not a challenge (SKADI-T-0567). Cloudflare's "Sorry, you
    // have been blocked" page is a firewall decision — an IP reputation or WAF
    // rule — with nothing to solve, and it *also* loads
    // `/cdn-cgi/challenge-platform/` telemetry, so the marker below matches it.
    //
    // That misclassification was expensive in production: Torrent[CORE] blocks
    // this deployment's VPN exit, and skadi invoked the solver on all 110 of its
    // fetches, every one of which could not succeed. With SKADI-T-0488's
    // concurrency cap those wasted solves occupy slots that working trackers
    // need, so a permanently-blocked tracker degrades the ones that do work.
    //
    // Checked first, so it wins over any challenge marker the page happens to
    // carry.
    const BLOCK_MARKERS: &[&str] = &[
        "Sorry, you have been blocked",
        "Attention Required! | Cloudflare",
        "block_headline",
        "Error 1020",
    ];
    if BLOCK_MARKERS.iter().any(|m| body.contains(m)) {
        return false;
    }
    const MARKERS: &[&str] = &[
        "Just a moment",
        "cf-browser-verification",
        "_cf_chl_opt",
        "challenge-platform",
        "Checking your browser",
        "DDoS-Guard",
        "ddos-guard",
        "cf_chl_",
    ];
    MARKERS.iter().any(|m| body.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_detection() {
        assert!(is_challenge(
            503,
            "<title>Just a moment...</title><div id=\"challenge-platform\">"
        ));
        assert!(is_challenge(403, "cf-browser-verification"));
        // A normal 200 page is never a challenge.
        assert!(!is_challenge(200, "Just a moment"));
        // A 403 without CF markers is a plain forbidden, not a challenge.
        assert!(!is_challenge(403, "<h1>Access denied</h1>"));
    }
}

#[cfg(test)]
mod block_vs_challenge_tests {
    use super::*;

    /// A Cloudflare **block** page, shaped like the one `torrentcore.xyz` served
    /// this deployment on 2026-09-10 — note it carries `challenge-platform`,
    /// which is exactly why it used to be misread as solvable (SKADI-T-0567).
    const BLOCK_PAGE: &str = r#"<!DOCTYPE html><html><head>
        <title>Attention Required! | Cloudflare</title></head><body>
        <h1 data-translate="block_headline">Sorry, you have been blocked</h1>
        <h2 data-translate="blocked_why_headline">Why have I been blocked?</h2>
        <script src="/cdn-cgi/challenge-platform/scripts/jsd/main.js"></script>
        </body></html>"#;

    /// The real thing: the interstitial EZTV and 1337x serve.
    const CHALLENGE_PAGE: &str = r#"<!DOCTYPE html><html><head>
        <title>Just a moment...</title></head><body>
        <script src="https://challenges.cloudflare.com/turnstile/v0/api.js"></script>
        <div class="challenge-platform"></div></body></html>"#;

    #[test]
    fn a_firewall_block_is_not_a_challenge() {
        // The whole point: there is nothing to solve, so invoking the solver
        // burns a slot that a working tracker needs (SKADI-T-0488's cap makes
        // those slots scarce). 110 fetches did exactly this in production.
        assert!(
            !is_challenge(403, BLOCK_PAGE),
            "a block page carries `challenge-platform` telemetry but has no challenge"
        );
    }

    #[test]
    fn a_real_challenge_is_still_a_challenge() {
        // The guard must not cost us the trackers the solver genuinely rescues —
        // EZTV serves 1013 of its fetches this way and has no direct path.
        assert!(is_challenge(403, CHALLENGE_PAGE));
        assert!(is_challenge(503, "Checking your browser before accessing"));
        assert!(is_challenge(429, "DDoS-Guard"));
    }

    #[test]
    fn block_markers_win_over_challenge_markers() {
        // Order matters: a page carrying both must read as a block, because
        // treating it as a challenge is the failure that wastes the solver.
        let both = format!("{BLOCK_PAGE}{CHALLENGE_PAGE}");
        assert!(!is_challenge(403, &both));
    }

    #[test]
    fn a_plain_forbidden_is_neither() {
        assert!(!is_challenge(403, "<h1>Access denied</h1>"));
        // And a 200 is never a challenge whatever it says.
        assert!(!is_challenge(200, CHALLENGE_PAGE));
    }
}

#[cfg(test)]
mod concurrency_cap_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The cap must bound solves **in flight**, not merely exist
    /// (SKADI-T-0488).
    ///
    /// Exercises the same semaphore `solve` holds for its whole round trip. The
    /// burst that started this was twelve at once against a single Chromium; the
    /// property that matters is that the peak never exceeds the cap, so that is
    /// what is asserted rather than a final count.
    #[tokio::test]
    async fn concurrent_solves_never_exceed_the_cap() {
        const CAP: usize = 2;
        const BURST: usize = 12;
        let sem = Arc::new(Semaphore::new(CAP));
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..BURST {
            let (sem, live, peak) = (sem.clone(), live.clone(), peak.clone());
            tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.expect("slots stay open");
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::task::yield_now().await;
                live.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for t in tasks {
            t.await.expect("no task panics");
        }

        assert_eq!(
            peak.load(Ordering::SeqCst),
            CAP,
            "a burst of {BURST} must never put more than {CAP} solves in flight"
        );
        assert_eq!(live.load(Ordering::SeqCst), 0, "every permit is released");
    }

    /// A cap of zero would deadlock every search rather than disable the solver.
    /// Disabling is what clearing `flaresolverr_url` is for.
    #[test]
    fn a_zero_cap_is_clamped_to_one() {
        set_max_concurrent(0);
        assert!(
            slots().available_permits() >= 1,
            "zero must not mean 'no searches ever complete'"
        );
    }

    /// Config reloads re-read the value; a second call must not panic the daemon
    /// over a number that has usually not changed.
    #[test]
    fn setting_it_twice_is_not_fatal() {
        set_max_concurrent(3);
        set_max_concurrent(4);
    }
}
