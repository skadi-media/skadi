//! `skadi-notify` — the notification abstraction.
//!
//! Defines the [`Notifier`] trait, the [`NotificationEvent`] values the rest of
//! the system emits, and the day-one [`WebhookNotifier`]. The daemon holds a
//! `Vec<Box<dyn Notifier>>` and fans each event out to the notifiers whose
//! [`Notifier::channels`] include the event's [`NotificationKind`]. A failed
//! notify is non-fatal to the grab/import that triggered it — the daemon logs
//! and continues; this crate only surfaces the error.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use skadi_core::{NotifierId, Result};

/// The kind of a notification, used for per-notifier channel filtering.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    Grabbed,
    Imported,
    Upgraded,
    Failed,
    Health,
    /// A library file was renamed (Sonarr's "On Rename"). Separate from
    /// `Imported`: the file was already ours, and a receiver that mirrors paths
    /// needs to know the name moved without thinking a new item arrived.
    Renamed,
    /// A library file was deleted (Sonarr's "On Delete"). The one event a mirror
    /// or an external index cannot infer from anything else.
    Deleted,
    /// A previously-reported health problem cleared (Sonarr's "On Health
    /// Restored"). Without it a receiver that raised an alert on `Health` has no
    /// way to lower it again, so an operator sees a stale alarm for a problem
    /// that fixed itself — which teaches them to ignore the alarms.
    HealthRestored,
    /// Emitted only by a notifier's "test" button (SKADI-T-0501). Deliberately
    /// not offered as a subscribable channel: it is a connectivity probe, not
    /// something an operator wants delivered on an ongoing basis.
    Test,
}

/// A small, serde-stable payload describing the item an event concerns.
#[derive(Clone, Eq, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct EventPayload {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    /// Human-readable detail (e.g. a failure reason or health message).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// An event emitted by the system for delivery to notifiers.
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum NotificationEvent {
    Grabbed(EventPayload),
    Imported(EventPayload),
    Upgraded(EventPayload),
    Failed(EventPayload),
    Health(EventPayload),
    Renamed(EventPayload),
    Deleted(EventPayload),
    HealthRestored(EventPayload),
}

impl NotificationEvent {
    /// The [`NotificationKind`] this event belongs to.
    #[must_use]
    pub fn kind(&self) -> NotificationKind {
        match self {
            Self::Grabbed(_) => NotificationKind::Grabbed,
            Self::Imported(_) => NotificationKind::Imported,
            Self::Upgraded(_) => NotificationKind::Upgraded,
            Self::Failed(_) => NotificationKind::Failed,
            Self::Health(_) => NotificationKind::Health,
            Self::Renamed(_) => NotificationKind::Renamed,
            Self::Deleted(_) => NotificationKind::Deleted,
            Self::HealthRestored(_) => NotificationKind::HealthRestored,
        }
    }

    /// The payload carried by this event.
    #[must_use]
    pub fn payload(&self) -> &EventPayload {
        match self {
            Self::Grabbed(p)
            | Self::Imported(p)
            | Self::Upgraded(p)
            | Self::Failed(p)
            | Self::Health(p)
            | Self::Renamed(p)
            | Self::Deleted(p)
            | Self::HealthRestored(p) => p,
        }
    }
}

/// A delivery target for notification events. Object-safe so the daemon can hold
/// `Vec<Box<dyn Notifier>>`.
#[async_trait]
pub trait Notifier: Send + Sync {
    /// Stable identifier for this configured notifier.
    fn id(&self) -> NotifierId;
    /// Which kinds of events this notifier wants delivered.
    fn channels(&self) -> &[NotificationKind];

    /// Whether this notifier applies to an item carrying `item_tags`
    /// (SKADI-T-0560).
    ///
    /// The default is `true` for **every** item, for the same reason the indexer
    /// side defaults that way (SKADI-T-0556): every notifier in every existing
    /// install is untagged, and returning `false` by default would silence all of
    /// them on upgrade — a failure whose only symptom is notifications quietly
    /// stopping.
    fn applies_to_tags(&self, _item_tags: &[String]) -> bool {
        true
    }
    /// Deliver one event. Errors are returned but treated as non-fatal upstream.
    async fn notify(&self, event: &NotificationEvent) -> Result<()>;
    /// Verify reachability/credentials (SKADI-T-0061).
    ///
    /// **Required, no default** — every implementor (mocks included) declares
    /// its own. Webhooks aren't reliably pingable, so [`WebhookNotifier`]
    /// returns `Ok(())` explicitly; a HEAD/health probe can come later.
    async fn test(&self) -> Result<()>;

    /// Whether this notifier wants `event` (default: membership in `channels()`).
    fn wants(&self, event: &NotificationEvent) -> bool {
        self.channels().contains(&event.kind())
    }
}

pub mod config;
mod webhook;
pub use config::{NotifierConfig, WebhookConfig};
pub use webhook::WebhookNotifier;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_round_trips_and_tags() {
        let ev = NotificationEvent::Imported(EventPayload {
            title: "The Matrix".into(),
            year: Some(1999),
            quality: Some("Bluray-1080p".into()),
            message: None,
        });
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["event"], "imported");
        assert_eq!(json["title"], "The Matrix");
        // None fields are omitted.
        assert!(json.get("message").is_none());

        let back: NotificationEvent = serde_json::from_value(json).unwrap();
        assert_eq!(ev, back);
        assert_eq!(back.kind(), NotificationKind::Imported);
    }
}
