//! Webhook notifier: POST a stable JSON envelope to a configured URL, with an
//! optional HMAC-SHA256 signature over the exact body bytes.

use async_trait::async_trait;
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;

use skadi_core::{AppError, NotifierId, Result};
use skadi_http::HttpClient;

use crate::{EventPayload, NotificationEvent, NotificationKind, Notifier};

type HmacSha256 = Hmac<Sha256>;

/// The stable wire contract delivered to a user's webhook receiver. Field names
/// and shape are a public contract — change with care.
#[derive(Serialize)]
struct Envelope<'a> {
    event: NotificationKind,
    payload: &'a EventPayload,
    timestamp: String,
}

/// Delivers events by POSTing a JSON [`Envelope`] to a configured URL.
pub struct WebhookNotifier {
    id: NotifierId,
    url: String,
    /// Optional HMAC-SHA256 secret; when set, every POST carries an
    /// `X-Skadi-Signature: <hex>` header over the body bytes.
    secret: Option<Vec<u8>>,
    channels: Vec<NotificationKind>,
    /// Tag ids scoping this notifier to matching items (SKADI-T-0560); empty
    /// means every item.
    tags: Vec<String>,
    http: HttpClient,
    /// Which body shape to post (SKADI-T-0504, SKADI-T-0538).
    flavour: Flavour,
}

/// The body shape a notifier posts (SKADI-T-0538).
///
/// All four share this one transport — and therefore one retry policy, one
/// timeout and one place to get delivery right. Only the body differs, so a
/// parallel implementation per service would be three more copies of the parts
/// that are easy to get wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Flavour {
    /// Our own `{event, payload, timestamp}` envelope, optionally HMAC-signed.
    Envelope,
    /// Discord's `{ "content": … }`; it renders that and ignores the rest.
    Discord,
    /// Telegram's `sendMessage` — the bot token is already in the URL, the chat
    /// id goes in the body.
    Telegram { chat_id: String },
    /// Pushover's `messages.json`, which wants the app token *and* the user key
    /// in the body rather than a header.
    Pushover { token: String, user_key: String },
}

impl WebhookNotifier {
    /// Construct a webhook notifier delivering `channels` to `url`.
    #[must_use]
    pub fn new(
        id: NotifierId,
        url: impl Into<String>,
        channels: Vec<NotificationKind>,
        http: HttpClient,
    ) -> Self {
        Self {
            id,
            url: url.into(),
            secret: None,
            channels,
            tags: Vec::new(),
            http,
            flavour: Flavour::Envelope,
        }
    }

    /// A notifier posting `flavour`'s body shape instead of our envelope
    /// (SKADI-T-0504, SKADI-T-0538). Same transport, same retry.
    #[must_use]
    pub fn with_flavour(
        id: NotifierId,
        url: impl Into<String>,
        channels: Vec<NotificationKind>,
        http: HttpClient,
        flavour: Flavour,
    ) -> Self {
        Self {
            flavour,
            ..Self::new(id, url, channels, http)
        }
    }

    /// Scope this notifier to items carrying one of `tags` (SKADI-T-0560).
    /// Empty leaves it applying to everything.
    #[must_use]
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Sign every POST with HMAC-SHA256 over the body using `secret`.
    #[must_use]
    pub fn with_secret(mut self, secret: impl Into<Vec<u8>>) -> Self {
        self.secret = Some(secret.into());
        self
    }
}

#[async_trait]
impl Notifier for WebhookNotifier {
    fn id(&self) -> NotifierId {
        self.id
    }

    fn channels(&self) -> &[NotificationKind] {
        &self.channels
    }

    fn applies_to_tags(&self, item_tags: &[String]) -> bool {
        // Untagged notifier ⇒ every item. Tagged ⇒ only items sharing a tag.
        // Same rule as `IndexerFlags::applies_to` (SKADI-T-0556), deliberately:
        // an operator who tags a notifier `4k` and an indexer `4k` expects both
        // to mean the same thing.
        self.tags.is_empty() || self.tags.iter().any(|t| item_tags.contains(t))
    }

    /// Deliver a real `test` event and report what the receiver said
    /// (SKADI-T-0501).
    ///
    /// This used to return `Ok(())` without contacting anything, so the test
    /// button passed against a typo'd URL, a receiver that was down, and a wrong
    /// signing secret alike — the one moment an operator is actually asking
    /// "does this work?". A webhook has no health endpoint to probe, so the only
    /// honest test is to deliver something, which is what Sonarr does too.
    async fn test(&self) -> Result<()> {
        let payload = EventPayload {
            title: "skadi test notification".into(),
            ..Default::default()
        };
        self.post(NotificationKind::Test, &payload).await
    }

    async fn notify(&self, event: &NotificationEvent) -> Result<()> {
        self.post(event.kind(), event.payload()).await
    }
}

impl WebhookNotifier {
    /// POST one envelope, signed when a secret is configured.
    ///
    /// Shared by `notify` and `test` so the signature header, the error mapping
    /// and the non-2xx check cannot drift apart between them.
    async fn post(&self, event: NotificationKind, payload: &EventPayload) -> Result<()> {
        let envelope = Envelope {
            event,
            payload,
            timestamp: Utc::now().to_rfc3339(),
        };
        // A plain one-line summary, for every flavour that renders text rather
        // than consuming our envelope.
        let text = match &payload.message {
            Some(m) => format!("{event:?} — {} — {m}", payload.title),
            None => format!("{event:?} — {}", payload.title),
        };
        let body = match &self.flavour {
            Flavour::Envelope => serde_json::to_vec(&envelope),
            Flavour::Discord => serde_json::to_vec(&serde_json::json!({
                "content": format!("**{event:?}** — {}", text_tail(&text)),
            })),
            Flavour::Telegram { chat_id } => serde_json::to_vec(&serde_json::json!({
                "chat_id": chat_id,
                "text": text,
            })),
            Flavour::Pushover { token, user_key } => serde_json::to_vec(&serde_json::json!({
                "token": token,
                "user": user_key,
                "title": payload.title,
                "message": text,
            })),
        }
        .map_err(|e| AppError::Internal(format!("serializing webhook envelope: {e}")))?;

        // Retry through `send_idempotent` (SKADI-T-0504) rather than `raw()`,
        // which bypassed the retry policy entirely — a receiver that blipped a
        // 503 dropped the notification silently and permanently.
        //
        // A POST is not idempotent in general, but *this* one is safe to repeat:
        // webhook delivery is an at-least-once contract (Sonarr retries too), and
        // a duplicate "imported" is a far better failure than a missing one. The
        // envelope carries a timestamp and the signature covers the body, so a
        // receiver that cares can de-duplicate.
        // Only our own envelope is signed: the other services authenticate by
        // their URL or a token in the body and would ignore the header.
        let signature = match self.flavour {
            Flavour::Envelope => self.secret.as_ref().map(|secret| sign(secret, &body)),
            _ => None,
        };
        self.http
            .send_idempotent(|c| {
                let mut req = c.post(&self.url).header("content-type", "application/json");
                if let Some(sig) = &signature {
                    req = req.header("x-skadi-signature", sig);
                }
                // Fresh body per attempt: a `RequestBuilder` is single-use.
                req.body(body.clone())
            })
            .await
            .map_err(|e| AppError::Network(format!("posting webhook: {e}")))?;
        Ok(())
    }
}

/// The summary minus its leading `Kind — `, so Discord's bolded kind is not
/// repeated in the text after it.
fn text_tail(text: &str) -> &str {
    text.split_once(" — ").map_or(text, |(_, rest)| rest)
}

/// HMAC-SHA256 of `body` under `secret`, lowercase hex.
fn sign(secret: &[u8], body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use wiremock::matchers::{header, header_exists, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    fn http() -> HttpClient {
        HttpClient::new(Duration::from_secs(5)).unwrap()
    }

    fn event() -> NotificationEvent {
        NotificationEvent::Grabbed(EventPayload {
            title: "Heat".into(),
            year: Some(1995),
            quality: Some("Bluray-1080p".into()),
            message: None,
        })
    }

    #[tokio::test]
    async fn posts_signed_envelope() {
        let server = MockServer::start().await;
        let secret = b"topsecret".to_vec();

        // Assert the matched request body and that a signature header is present;
        // also verify the signature value matches HMAC over the received body.
        Mock::given(method("POST"))
            .and(path("/hook"))
            .and(header("content-type", "application/json"))
            .and(header_exists("x-skadi-signature"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let n = WebhookNotifier::new(
            NotifierId::new(),
            format!("{}/hook", server.uri()),
            vec![NotificationKind::Grabbed],
            http(),
        )
        .with_secret(secret.clone());
        n.notify(&event()).await.unwrap();

        // Pull the recorded request and check the signature is correct for its body.
        let reqs = server.received_requests().await.unwrap();
        let req: &Request = &reqs[0];
        let got = req
            .headers
            .get("x-skadi-signature")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(got, sign(&secret, &req.body));
        // Envelope shape.
        let v: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(v["event"], "grabbed");
        assert_eq!(v["payload"]["title"], "Heat");
        assert!(v["timestamp"].is_string());
    }

    #[tokio::test]
    async fn posts_unsigned_when_no_secret() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .and(header("content-type", "application/json"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let n = WebhookNotifier::new(
            NotifierId::new(),
            format!("{}/hook", server.uri()),
            vec![NotificationKind::Grabbed],
            http(),
        );
        n.notify(&event()).await.unwrap();

        let reqs = server.received_requests().await.unwrap();
        assert!(reqs[0].headers.get("x-skadi-signature").is_none());
    }

    #[tokio::test]
    async fn non_2xx_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let n = WebhookNotifier::new(
            NotifierId::new(),
            format!("{}/hook", server.uri()),
            vec![NotificationKind::Grabbed],
            http(),
        );
        let err = n.notify(&event()).await.unwrap_err();
        assert!(matches!(err, AppError::Network(_)));
    }

    #[test]
    fn wants_filters_by_channel() {
        let n = WebhookNotifier::new(
            NotifierId::new(),
            "http://unused",
            vec![NotificationKind::Imported],
            http(),
        );
        assert!(n.wants(&NotificationEvent::Imported(EventPayload::default())));
        assert!(!n.wants(&event()));
    }
}

#[cfg(test)]
mod tag_scope_tests {
    use super::*;

    fn n(tags: &[&str]) -> WebhookNotifier {
        WebhookNotifier::new(
            NotifierId::new(),
            "http://example.invalid/hook",
            vec![NotificationKind::Imported],
            HttpClient::new(std::time::Duration::from_secs(5)).unwrap(),
        )
        .with_tags(tags.iter().map(|s| (*s).to_string()).collect())
    }

    /// The regression that would silence every existing install (SKADI-T-0560).
    #[test]
    fn an_untagged_notifier_fires_for_everything() {
        let notifier = n(&[]);
        assert!(notifier.applies_to_tags(&[]));
        assert!(
            notifier.applies_to_tags(&["anime".into()]),
            "an untagged notifier must keep firing for tagged items too — every \
             notifier out there today is untagged, and the inverted reading would \
             silence all of them with no error anywhere"
        );
    }

    #[test]
    fn a_tagged_notifier_fires_only_where_a_tag_is_shared() {
        let notifier = n(&["4k", "anime"]);
        assert!(notifier.applies_to_tags(&["4k".into()]));
        assert!(notifier.applies_to_tags(&["anime".into(), "other".into()]));
        assert!(!notifier.applies_to_tags(&["other".into()]));
        // An untagged *item* against a tagged notifier: no overlap, so no. The
        // safe default belongs on the notifier's empty set, not the item's.
        assert!(!notifier.applies_to_tags(&[]));
    }

    #[test]
    fn the_rule_matches_the_indexer_side_exactly() {
        // An operator who tags a notifier `4k` and an indexer `4k` expects both
        // to mean the same thing. If these two ever diverge, one of them is
        // surprising and there is no way to tell which from the UI.
        for (notifier_tags, item_tags, want) in [
            (vec![], vec!["a"], true),
            (vec!["a"], vec!["a"], true),
            (vec!["a"], vec!["b"], false),
            (vec!["a"], vec![], false),
        ] {
            let flags = skadi_indexers::IndexerFlags {
                tags: notifier_tags.iter().map(|s| (*s).to_string()).collect(),
                ..Default::default()
            };
            let items: Vec<String> = item_tags.iter().map(|s| (*s).to_string()).collect();
            assert_eq!(
                n(&notifier_tags).applies_to_tags(&items),
                want,
                "notifier {notifier_tags:?} vs item {item_tags:?}"
            );
            assert_eq!(
                flags.applies_to(&items),
                want,
                "indexer {notifier_tags:?} vs item {item_tags:?} must agree"
            );
        }
    }
}
