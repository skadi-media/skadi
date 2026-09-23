//! Typed notifier configuration (SKADI-T-0058).
//!
//! [`NotifierConfig`] is the exact non-secret shape stored in the daemon's
//! settings `body` for `kind = "notifiers"` rows. The optional HMAC secret is
//! **not** part of this config — it lives in the credential store and is passed
//! to [`NotifierConfig::build`].

use serde::{Deserialize, Serialize};

use skadi_core::{AppError, NotifierId, Result};
use skadi_http::HttpClient;

use crate::webhook::WebhookNotifier;
use crate::{NotificationKind, Notifier};

/// Non-secret configuration for one notifier, as stored in settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NotifierConfig {
    /// A JSON webhook receiver.
    Webhook(WebhookConfig),
    /// A Telegram bot (SKADI-T-0538). The **bot token is the secret** and lives
    /// in the credential store, not here — unlike Discord, where the webhook URL
    /// is itself the credential and so belongs in the settings body.
    Telegram(TelegramConfig),
    /// A Pushover application (SKADI-T-0538). The **app token is the secret**;
    /// the user key identifies the recipient and is not itself sufficient to
    /// send, so it is ordinary config.
    Pushover(PushoverConfig),
    /// A Discord channel webhook (SKADI-T-0504).
    ///
    /// Discord's incoming-webhook endpoint accepts a JSON POST, so it reuses the
    /// webhook transport rather than getting a parallel implementation — the only
    /// difference that matters is the body shape, and a Discord webhook URL is
    /// itself the credential, so no HMAC secret applies.
    Discord(DiscordConfig),
}

/// Configuration for a Telegram bot notifier (SKADI-T-0538).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramConfig {
    /// Display name.
    pub name: String,
    /// The chat (or channel) id messages are sent to.
    pub chat_id: String,
    /// Event kinds this notifier wants.
    pub channels: Vec<NotificationKind>,
    /// Tag ids scoping this notifier to matching items (SKADI-T-0560).
    ///
    /// **Empty means "every item"**, not "no items" — the same rule as indexer
    /// scoping (SKADI-T-0556). Every notifier in every existing install is
    /// untagged, so the inverted reading would silence all of them on upgrade,
    /// and the only symptom would be notifications quietly stopping.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Configuration for a Pushover notifier (SKADI-T-0538).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushoverConfig {
    /// Display name.
    pub name: String,
    /// The recipient's user (or group) key.
    pub user_key: String,
    /// Event kinds this notifier wants.
    pub channels: Vec<NotificationKind>,
    /// Tag ids scoping this notifier to matching items (SKADI-T-0560).
    ///
    /// **Empty means "every item"**, not "no items" — the same rule as indexer
    /// scoping (SKADI-T-0556). Every notifier in every existing install is
    /// untagged, so the inverted reading would silence all of them on upgrade,
    /// and the only symptom would be notifications quietly stopping.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Configuration for a Discord channel webhook (SKADI-T-0504).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordConfig {
    /// Display name.
    pub name: String,
    /// The `https://discord.com/api/webhooks/...` URL from the channel settings.
    pub url: String,
    /// Event kinds this notifier wants.
    pub channels: Vec<NotificationKind>,
    /// Tag ids scoping this notifier to matching items (SKADI-T-0560).
    ///
    /// **Empty means "every item"**, not "no items" — the same rule as indexer
    /// scoping (SKADI-T-0556). Every notifier in every existing install is
    /// untagged, so the inverted reading would silence all of them on upgrade,
    /// and the only symptom would be notifications quietly stopping.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Configuration for a [`WebhookNotifier`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookConfig {
    /// Display name.
    pub name: String,
    /// Receiver URL the JSON envelope is POSTed to.
    pub url: String,
    /// Event kinds this notifier wants.
    pub channels: Vec<NotificationKind>,
    /// Tag ids scoping this notifier to matching items (SKADI-T-0560).
    ///
    /// **Empty means "every item"**, not "no items" — the same rule as indexer
    /// scoping (SKADI-T-0556). Every notifier in every existing install is
    /// untagged, so the inverted reading would silence all of them on upgrade,
    /// and the only symptom would be notifications quietly stopping.
    #[serde(default)]
    pub tags: Vec<String>,
}

impl NotifierConfig {
    /// Validate and build the live notifier. `secret` (optional HMAC signing
    /// key) comes from the credential store.
    pub fn build(
        self,
        id: NotifierId,
        secret: Option<String>,
        http: HttpClient,
    ) -> Result<Box<dyn Notifier>> {
        match self {
            NotifierConfig::Webhook(cfg) => {
                if cfg.url.trim().is_empty()
                    || !(cfg.url.starts_with("http://") || cfg.url.starts_with("https://"))
                {
                    return Err(AppError::Validation(format!(
                        "webhook notifier {:?}: url must be an http(s) URL",
                        cfg.name
                    )));
                }
                if cfg.channels.is_empty() {
                    return Err(AppError::Validation(format!(
                        "webhook notifier {:?}: channels must be non-empty",
                        cfg.name
                    )));
                }
                let mut notifier =
                    WebhookNotifier::new(id, cfg.url, cfg.channels, http).with_tags(cfg.tags);
                if let Some(secret) = secret {
                    notifier = notifier.with_secret(secret.into_bytes());
                }
                Ok(Box::new(notifier))
            }
            NotifierConfig::Telegram(cfg) => {
                let token = secret.filter(|s| !s.trim().is_empty()).ok_or_else(|| {
                    AppError::field(
                        "secret",
                        format!("telegram notifier {:?}: a bot token is required", cfg.name),
                    )
                })?;
                if cfg.chat_id.trim().is_empty() {
                    return Err(AppError::field(
                        "chat_id",
                        format!("telegram notifier {:?}: chat_id is required", cfg.name),
                    ));
                }
                if cfg.channels.is_empty() {
                    return Err(AppError::field(
                        "channels",
                        format!(
                            "telegram notifier {:?}: channels must be non-empty",
                            cfg.name
                        ),
                    ));
                }
                // The token goes in the path, which is Telegram's own scheme —
                // so the URL must never be logged or echoed back.
                let url = format!("https://api.telegram.org/bot{}/sendMessage", token.trim());
                Ok(Box::new(
                    WebhookNotifier::with_flavour(
                        id,
                        url,
                        cfg.channels,
                        http,
                        crate::webhook::Flavour::Telegram {
                            chat_id: cfg.chat_id,
                        },
                    )
                    .with_tags(cfg.tags),
                ))
            }
            NotifierConfig::Pushover(cfg) => {
                let token = secret.filter(|s| !s.trim().is_empty()).ok_or_else(|| {
                    AppError::field(
                        "secret",
                        format!("pushover notifier {:?}: an app token is required", cfg.name),
                    )
                })?;
                if cfg.user_key.trim().is_empty() {
                    return Err(AppError::field(
                        "user_key",
                        format!("pushover notifier {:?}: user_key is required", cfg.name),
                    ));
                }
                if cfg.channels.is_empty() {
                    return Err(AppError::field(
                        "channels",
                        format!(
                            "pushover notifier {:?}: channels must be non-empty",
                            cfg.name
                        ),
                    ));
                }
                Ok(Box::new(
                    WebhookNotifier::with_flavour(
                        id,
                        "https://api.pushover.net/1/messages.json",
                        cfg.channels,
                        http,
                        crate::webhook::Flavour::Pushover {
                            token: token.trim().to_string(),
                            user_key: cfg.user_key,
                        },
                    )
                    .with_tags(cfg.tags),
                ))
            }
            NotifierConfig::Discord(cfg) => {
                if !cfg.url.starts_with("https://") {
                    // https only: the URL *is* the credential, so sending it over
                    // plaintext http would hand it to anyone on the path.
                    return Err(AppError::field(
                        "url",
                        format!(
                            "discord notifier {:?}: url must be an https webhook URL",
                            cfg.name
                        ),
                    ));
                }
                if cfg.channels.is_empty() {
                    return Err(AppError::field(
                        "channels",
                        format!(
                            "discord notifier {:?}: channels must be non-empty",
                            cfg.name
                        ),
                    ));
                }
                // No secret: Discord authenticates by the URL itself, so an HMAC
                // header would be noise it ignores.
                Ok(Box::new(
                    WebhookNotifier::with_flavour(
                        id,
                        cfg.url,
                        cfg.channels,
                        http,
                        crate::webhook::Flavour::Discord,
                    )
                    .with_tags(cfg.tags),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http() -> HttpClient {
        HttpClient::new(std::time::Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn deserializes_and_builds_webhook() {
        let json = serde_json::json!({
            "kind": "webhook",
            "name": "ping",
            "url": "https://example.com/hook",
            "channels": ["imported"]
        });
        let cfg: NotifierConfig = serde_json::from_value(json).unwrap();
        assert!(cfg.build(NotifierId::new(), None, http()).is_ok());
        // And with a secret.
        let json2 = serde_json::json!({
            "kind": "webhook",
            "name": "ping",
            "url": "https://example.com/hook",
            "channels": ["imported", "failed"]
        });
        let cfg2: NotifierConfig = serde_json::from_value(json2).unwrap();
        assert!(
            cfg2.build(NotifierId::new(), Some("s3cret".into()), http())
                .is_ok()
        );
    }

    #[test]
    fn config_round_trips() {
        let cfg = NotifierConfig::Webhook(WebhookConfig {
            tags: Vec::new(),
            name: "ping".into(),
            url: "https://x".into(),
            channels: vec![NotificationKind::Imported],
        });
        let json = serde_json::to_value(&cfg).unwrap();
        assert_eq!(json["kind"], "webhook");
        let back: NotifierConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn rejects_bad_url_and_empty_channels() {
        let bad = NotifierConfig::Webhook(WebhookConfig {
            tags: Vec::new(),
            name: "x".into(),
            url: "nope".into(),
            channels: vec![NotificationKind::Imported],
        });
        assert!(matches!(
            bad.build(NotifierId::new(), None, http()),
            Err(AppError::Validation(_))
        ));
        let empty = NotifierConfig::Webhook(WebhookConfig {
            tags: Vec::new(),
            name: "x".into(),
            url: "https://ok".into(),
            channels: vec![],
        });
        assert!(matches!(
            empty.build(NotifierId::new(), None, http()),
            Err(AppError::Validation(_))
        ));
    }
}
