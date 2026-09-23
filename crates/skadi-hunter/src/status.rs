//! The status-persistence boundary.
//!
//! As tasks complete, the hunter records the acquirable's [`AcquisitionStatus`]
//! so the database reflects live progress independent of Cloacina's internal
//! task tables. *Where* that status lives is a **domain** concern — the
//! acquirable (e.g. a movie edition) is owned by the domain, and there is no
//! generic acquirable table in `skadi-store` (only `domains`). So the hunter
//! depends on a [`StatusSink`] trait that the domain implements against its own
//! table; the hunter stays edition-agnostic. [`InMemoryStatusSink`] is provided
//! for tests and as a default no-persistence option.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use skadi_core::{AcquisitionStatus, Result};
use skadi_importer::AcquirableRef;

/// Sink the hunter writes acquisition status into at task boundaries. The
/// domain supplies the real implementation (writing to its acquirable table);
/// the hunter never interprets the [`AcquirableRef`].
#[async_trait]
pub trait StatusSink: Send + Sync {
    /// Persist `status` for `acquirable`. Must be idempotent — a resumed run may
    /// re-write the same status.
    async fn set_status(&self, acquirable: &AcquirableRef, status: AcquisitionStatus)
    -> Result<()>;

    /// The last persisted status for `acquirable`, if any.
    async fn get_status(&self, acquirable: &AcquirableRef) -> Result<Option<AcquisitionStatus>>;

    /// Persist probed [`MediaInfo`](skadi_core::MediaInfo) for `acquirable`
    /// (SKADI-T-0237) — the post-import probe step's sink. Default is a no-op (the
    /// in-memory / no-persistence sinks don't store it); domains override to write it.
    async fn set_media_info(
        &self,
        _acquirable: &AcquirableRef,
        _media_info: skadi_core::MediaInfo,
    ) -> Result<()> {
        Ok(())
    }
}

/// An in-memory [`StatusSink`] for tests and as a no-database default. Records
/// the latest status per acquirable.
#[derive(Default)]
pub struct InMemoryStatusSink {
    states: Mutex<HashMap<String, AcquisitionStatus>>,
}

impl InMemoryStatusSink {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl StatusSink for InMemoryStatusSink {
    async fn set_status(
        &self,
        acquirable: &AcquirableRef,
        status: AcquisitionStatus,
    ) -> Result<()> {
        self.states
            .lock()
            .unwrap()
            .insert(acquirable.0.clone(), status);
        Ok(())
    }

    async fn get_status(&self, acquirable: &AcquirableRef) -> Result<Option<AcquisitionStatus>> {
        Ok(self.states.lock().unwrap().get(&acquirable.0).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use skadi_core::{DownloaderId, FailureReason, ReleaseId};

    #[tokio::test]
    async fn records_and_reads_back_each_variant() {
        let sink = InMemoryStatusSink::new();
        let a = AcquirableRef("ed-1".into());

        assert!(sink.get_status(&a).await.unwrap().is_none());

        for status in [
            AcquisitionStatus::Searching {
                since: Utc::now(),
                attempts: 1,
            },
            AcquisitionStatus::Snatched {
                release: ReleaseId::new(),
                downloader: DownloaderId::new(),
                at: Utc::now(),
            },
            AcquisitionStatus::Downloading {
                release: ReleaseId::new(),
                progress: 0.5,
            },
            AcquisitionStatus::Failed {
                reason: FailureReason::NoSuitableRelease,
                retry_at: None,
                attempts: 0,
            },
        ] {
            sink.set_status(&a, status.clone()).await.unwrap();
            assert_eq!(sink.get_status(&a).await.unwrap().as_ref(), Some(&status));
        }
    }

    #[tokio::test]
    async fn set_is_idempotent_overwrite() {
        let sink = InMemoryStatusSink::new();
        let a = AcquirableRef("ed-2".into());
        let s = AcquisitionStatus::Cutoff;
        sink.set_status(&a, s.clone()).await.unwrap();
        sink.set_status(&a, s.clone()).await.unwrap();
        assert_eq!(sink.get_status(&a).await.unwrap(), Some(s));
    }
}
