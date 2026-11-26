//! The piece probe of the storage host as a health check (SKADI-T-0775).
//!
//! The probe stops itself when it cannot work and waits for an operator. This
//! check is where the operator learns of it: the reason, and what to do.

use async_trait::async_trait;

use skadi_library_scan::probe;
use skadi_store::Store;

use super::{HealthCheck, Outcome};

/// The state of the piece probe.
pub struct PieceProbeCheck {
    store: Store,
}

impl PieceProbeCheck {
    pub fn new(store: Store) -> Self {
        PieceProbeCheck { store }
    }
}

const FAILED_REMEDIATION: &str = "Fix the cause, then press Clear in Settings → Storage → Piece probe. The probe stays off until then. If the storage host has a new key on purpose, empty storage.probe.host_key first.";

const SETUP_REMEDIATION: &str = "Set the storage.probe settings in Settings → Library and the credential in Settings → Storage, or turn storage.probe.pieces.enabled off.";

#[async_trait]
impl HealthCheck for PieceProbeCheck {
    fn id(&self) -> String {
        "storage-piece-probe".into()
    }
    fn label(&self) -> String {
        "Storage piece probe".into()
    }
    async fn run(&self) -> Outcome {
        let o = match probe::overview(&self.store).await {
            Ok(o) => o,
            Err(e) => {
                return Outcome::error(
                    format!("the probe's state is unreadable: {e}"),
                    "Check the database check and the daemon log.",
                );
            }
        };
        if o.failed {
            return Outcome::error(
                format!(
                    "the piece probe has stopped: {}",
                    o.reason.as_deref().unwrap_or("no reason was recorded")
                ),
                FAILED_REMEDIATION,
            );
        }
        if !o.enabled {
            return Outcome::ok("off (storage.probe.pieces.enabled)");
        }
        if !o.missing.is_empty() {
            return Outcome::warn(
                format!("on, but not set up: {} missing", o.missing.join(", ")),
                SETUP_REMEDIATION,
            );
        }
        Outcome::ok("on")
    }
}
