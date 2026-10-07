//! An in-memory ring of recent log lines, for `GET /api/v1/log` (SKADI-T-0467).
//!
//! Sonarr's System → Logs reads log *files*. Skadi logs to stdout, which is
//! where a container's logs belong, so there is no file to serve — but an
//! operator still needs to see what the daemon has been saying without shelling
//! into the host. This keeps the last few hundred formatted lines in memory and
//! serves them.
//!
//! Deliberately bounded and deliberately not persistent: it is a diagnostic
//! convenience, not a log store. `docker logs` and the host's journal remain the
//! real record, and this ring is empty after a restart.
//!
//! Each line is redacted ([`crate::redact`]) before it is stored: a provider
//! error can name a URL with its `apikey=`, and this ring is served over HTTP.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tracing::Subscriber;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// How many lines the ring holds. A few hundred covers "what just happened"
/// without letting a chatty debug level grow the process.
pub const CAPACITY: usize = 500;

/// One captured log line.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub time: String,
    pub level: String,
    pub target: String,
    pub message: String,
}

/// The shared ring. Cloneable; every clone sees the same buffer.
#[derive(Clone, Default)]
pub struct LogBuffer(Arc<Mutex<VecDeque<LogLine>>>);

impl LogBuffer {
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(VecDeque::with_capacity(CAPACITY))))
    }

    fn push(&self, line: LogLine) {
        // A poisoned lock must not take the daemon down over logging.
        if let Ok(mut buf) = self.0.lock() {
            if buf.len() == CAPACITY {
                buf.pop_front();
            }
            buf.push_back(line);
        }
    }

    /// The most recent `limit` lines, newest first.
    #[must_use]
    pub fn recent(&self, limit: usize) -> Vec<LogLine> {
        match self.0.lock() {
            Ok(buf) => buf.iter().rev().take(limit).cloned().collect(),
            Err(_) => Vec::new(),
        }
    }

    /// A `tracing` layer that feeds this buffer. Compose it alongside the stdout
    /// layer — capturing here never replaces logging to stdout.
    #[must_use]
    pub fn layer(&self) -> LogBufferLayer {
        LogBufferLayer(self.clone())
    }
}

/// The [`Layer`] half of [`LogBuffer`].
pub struct LogBufferLayer(LogBuffer);

impl<S> Layer<S> for LogBufferLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut message = String::new();
        event.record(&mut MessageVisitor(&mut message));
        // Redacted on the way in (SKADI-T-0685): the ring never holds a
        // secret, so nothing that reads it — `/log` today — can serve one.
        self.0.push(LogLine {
            time: chrono::Utc::now().to_rfc3339(),
            level: event.metadata().level().to_string(),
            target: event.metadata().target().to_string(),
            message: crate::redact::redact_string(message),
        });
    }
}

/// Pulls the `message` field out of an event, appending any other fields so a
/// structured log line is still readable as text.
struct MessageVisitor<'a>(&'a mut String);

impl tracing::field::Visit for MessageVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            if !self.0.is_empty() {
                self.0.push(' ');
            }
            let _ = write!(self.0, "{}={value:?}", field.name());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_keeps_the_newest_lines_and_bounds_itself() {
        let buf = LogBuffer::new();
        for i in 0..(CAPACITY + 10) {
            buf.push(LogLine {
                time: "t".into(),
                level: "INFO".into(),
                target: "test".into(),
                message: format!("line {i}"),
            });
        }
        let recent = buf.recent(3);
        assert_eq!(recent.len(), 3, "limit is honoured");
        // Newest first.
        assert_eq!(recent[0].message, format!("line {}", CAPACITY + 9));
        assert_eq!(recent[1].message, format!("line {}", CAPACITY + 8));
        // And the oldest were dropped rather than growing forever.
        assert_eq!(buf.recent(usize::MAX).len(), CAPACITY);
    }

    #[test]
    fn a_captured_line_is_redacted_before_it_is_stored() {
        use tracing_subscriber::layer::SubscriberExt as _;
        let buf = LogBuffer::new();
        let subscriber = tracing_subscriber::registry().with(buf.layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(
                error = "error sending request for url (http://idx/api?t=caps&apikey=SEKRIT)",
                "indexer test failed for http://u:hunter2@idx"
            );
        });
        let line = &buf.recent(1)[0];
        assert!(!line.message.contains("SEKRIT"), "{}", line.message);
        assert!(!line.message.contains("hunter2"), "{}", line.message);
        assert!(line.message.contains("apikey=***"), "{}", line.message);
        assert!(
            line.message.contains("http://u:***@idx"),
            "{}",
            line.message
        );
    }

    #[test]
    fn asking_for_more_than_exists_returns_what_there_is() {
        let buf = LogBuffer::new();
        buf.push(LogLine {
            time: "t".into(),
            level: "WARN".into(),
            target: "test".into(),
            message: "only one".into(),
        });
        assert_eq!(buf.recent(50).len(), 1);
    }
}
