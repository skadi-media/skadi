//! The `vpn` check (SKADI-T-0683): the tunnel is up, gluetun reports an exit IP,
//! and the download worker egresses from that same IP.
//!
//! The worker shares gluetun's network namespace, so in a correct deploy the
//! comparison holds by construction. What it catches is a misconfigured deploy:
//! a worker outside the namespace (it reports no egress IP, because gluetun is
//! not on its loopback) or behind another tunnel (it reports another IP). The
//! worker observes its egress by asking gluetun on loopback, at most once a
//! minute (`skadi-downloader-worker/src/egress.rs`), and writes it on its
//! heartbeat row.
//!
//! A deploy without a VPN (`SKADI_GLUETUN_CONTROL_URL` unset or empty: the lab, a
//! dev run) has no `vpn` check at all.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use skadi_store::{Store, WorkerStatus, WorkerStatusRepo};

use super::builtin::WORKER_SILENT_AFTER;
use super::{CheckContext, CheckSource, HealthCheck, Outcome};
use crate::vpn::{VpnStatus, configured_control_url, status_at};

/// Two sequential control-server requests of 4 s each, plus a heartbeat read.
const VPN_CHECK_TIMEOUT: Duration = Duration::from_secs(15);

/// gluetun's exit IP changes only on a reconnect, and the worker refreshes its
/// observation once a minute, so a fresher result would not say more.
const VPN_CHECK_TTL: Duration = Duration::from_secs(60);

/// Lists the `vpn` check when the deploy configures gluetun, else nothing.
pub struct VpnChecks;

#[async_trait]
impl CheckSource for VpnChecks {
    async fn checks(&self, ctx: &CheckContext) -> Vec<Arc<dyn HealthCheck>> {
        match configured_control_url() {
            Some(url) => vec![Arc::new(VpnCheck {
                store: ctx.store.clone(),
                url,
            })],
            None => Vec::new(),
        }
    }
}

/// The tunnel, its exit IP, and the worker's egress against it.
pub struct VpnCheck {
    store: Store,
    /// gluetun's control server, as the daemon reaches it.
    url: String,
}

#[async_trait]
impl HealthCheck for VpnCheck {
    fn id(&self) -> String {
        "vpn".into()
    }
    fn label(&self) -> String {
        "VPN".into()
    }
    fn timeout(&self) -> Duration {
        VPN_CHECK_TIMEOUT
    }
    fn ttl(&self) -> Duration {
        VPN_CHECK_TTL
    }
    async fn run(&self) -> Outcome {
        let vpn = status_at(&self.url).await;
        // An unreadable heartbeat leaves the egress uncompared; the worker
        // check reports the read error itself.
        let worker = self.store.latest_worker_status().await.ok().flatten();
        let fresh = worker.filter(|w| w.is_fresh(WORKER_SILENT_AFTER));
        VpnCheck::outcome(&self.url, &vpn, fresh.as_ref())
    }
}

impl VpnCheck {
    /// The verdict over gluetun's answer and the latest **fresh** worker
    /// heartbeat (`None` = no worker heard from recently). Pure.
    pub fn outcome(url: &str, vpn: &VpnStatus, worker: Option<&WorkerStatus>) -> Outcome {
        if !vpn.reachable {
            return Outcome::error(
                format!(
                    "gluetun's control server at {url} did not answer — the VPN state is unknown"
                ),
                "Check that the gluetun container is running and that SKADI_GLUETUN_CONTROL_URL points at its control server (port 8000).",
            );
        }
        if !vpn.connected {
            return Outcome::error(
                format!(
                    "the VPN tunnel is {} — downloads have no route out",
                    vpn.tunnel.as_deref().unwrap_or("not running")
                ),
                "Read gluetun's log (docker compose logs gluetun) for the provider error and fix the VPN settings in deploy/.env; the vpn-watchdog restarts gluetun and its namespace-mates after a few unhealthy minutes.",
            );
        }
        let Some(exit) = vpn.exit_ip.as_deref() else {
            return Outcome::warn(
                "the VPN tunnel is up, but gluetun reports no exit IP yet",
                "gluetun looks up its public IP once the tunnel connects; if this lasts, read gluetun's log for the public-IP lookup.",
            );
        };
        let place = vpn
            .country
            .as_deref()
            .map(|c| format!(" ({c})"))
            .unwrap_or_default();
        let Some(w) = worker else {
            return Outcome::ok(format!(
                "tunnel up, exit IP {exit}{place}; the worker's egress is not compared (no recent worker heartbeat)"
            ));
        };
        match w.egress_ip.as_deref() {
            None => Outcome::warn(
                format!(
                    "tunnel up, exit IP {exit}{place}, but the download worker {} reported no egress IP: it could not reach gluetun on its own loopback",
                    w.worker_id
                ),
                "Run the download worker in gluetun's network namespace (network_mode: service:gluetun in deploy/docker-compose.yml); a worker outside it downloads without the VPN. A worker build older than this check reports no egress IP: update it.",
            ),
            Some(egress) if egress != exit => Outcome::error(
                format!(
                    "the download worker {} egresses from {egress}, not from the VPN exit {exit} — its traffic is not going through this tunnel",
                    w.worker_id
                ),
                "Stop the download worker, then recreate it inside gluetun's network namespace (network_mode: service:gluetun) together with gluetun. If gluetun reconnected in the last minute, run the check again first: the worker refreshes its egress IP once a minute.",
            ),
            Some(_) => Outcome::ok(format!(
                "tunnel up, exit IP {exit}{place}; the download worker {} egresses from the same IP",
                w.worker_id
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health_checks::Severity;

    const URL: &str = "http://gluetun:8000";

    fn up(exit: Option<&str>) -> VpnStatus {
        VpnStatus {
            reachable: true,
            connected: true,
            exit_ip: exit.map(str::to_string),
            country: Some("Testland".into()),
            tunnel: Some("running".into()),
            ..VpnStatus::default()
        }
    }

    fn worker(egress: Option<&str>) -> WorkerStatus {
        WorkerStatus {
            worker_id: "worker-a".into(),
            last_seen_at: chrono::Utc::now(),
            version: "0.0.0".into(),
            egress_ip: egress.map(str::to_string),
        }
    }

    #[test]
    fn unreachable_gluetun_is_an_error() {
        let o = VpnCheck::outcome(URL, &VpnStatus::default(), None);
        assert_eq!(o.severity, Severity::Error);
        assert!(o.message.contains(URL), "{}", o.message);
    }

    #[test]
    fn a_stopped_tunnel_is_an_error_naming_the_state() {
        let vpn = VpnStatus {
            reachable: true,
            tunnel: Some("stopped".into()),
            ..VpnStatus::default()
        };
        let o = VpnCheck::outcome(URL, &vpn, Some(&worker(Some("203.0.113.7"))));
        assert_eq!(o.severity, Severity::Error);
        assert!(o.message.contains("stopped"), "{}", o.message);
    }

    #[test]
    fn no_exit_ip_is_a_warning() {
        let o = VpnCheck::outcome(URL, &up(None), None);
        assert_eq!(o.severity, Severity::Warn);
    }

    #[test]
    fn without_a_recent_worker_the_tunnel_alone_is_ok() {
        let o = VpnCheck::outcome(URL, &up(Some("203.0.113.7")), None);
        assert_eq!(o.severity, Severity::Ok);
        assert!(o.message.contains("not compared"), "{}", o.message);
    }

    #[test]
    fn a_worker_that_sees_no_egress_is_a_warning() {
        let o = VpnCheck::outcome(URL, &up(Some("203.0.113.7")), Some(&worker(None)));
        assert_eq!(o.severity, Severity::Warn);
        assert!(o.message.contains("worker-a"), "{}", o.message);
    }

    #[test]
    fn a_different_egress_is_an_error_naming_both_ips() {
        let o = VpnCheck::outcome(
            URL,
            &up(Some("203.0.113.7")),
            Some(&worker(Some("198.51.100.9"))),
        );
        assert_eq!(o.severity, Severity::Error);
        assert!(o.message.contains("198.51.100.9"), "{}", o.message);
        assert!(o.message.contains("203.0.113.7"), "{}", o.message);
    }

    #[test]
    fn the_same_egress_is_ok() {
        let o = VpnCheck::outcome(
            URL,
            &up(Some("203.0.113.7")),
            Some(&worker(Some("203.0.113.7"))),
        );
        assert_eq!(o.severity, Severity::Ok);
    }
}
