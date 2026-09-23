//! The crate-wide error type and [`Result`] alias.
//!
//! `skadi-core` owns the canonical error enum so every other crate can return
//! `skadi_core::Result<T>`. Variants are intentionally broad; richer,
//! domain-specific context is layered on by wrapping into [`AppError::Other`]
//! (via [`anyhow`]) or by adding variants as concrete needs appear.

use thiserror::Error;

/// The crate-wide error type.
#[derive(Error, Debug)]
pub enum AppError {
    /// A requested entity does not exist.
    #[error("Not found: {0}")]
    NotFound(String),

    /// Input failed validation.
    #[error("Validation error: {0}")]
    Validation(String),

    /// Input failed validation **for a named field** (SKADI-T-0525).
    ///
    /// Sonarr answers a bad settings save with per-field
    /// `{propertyName, errorMessage}` so its UI can put the message next to the
    /// offending input. With only [`Validation`](Self::Validation)'s free text, a
    /// form could show the reason but not *where*, so the operator had to work out
    /// which of a dozen inputs was wrong from prose.
    ///
    /// Kept as a separate variant rather than adding a field to `Validation`:
    /// there are ~150 call sites, most of which genuinely have no single
    /// offending field, and forcing them all to invent one would make the field
    /// meaningless exactly where a UI would want to trust it.
    #[error("{field}: {message}")]
    ValidationField { field: String, message: String },

    /// An illegal acquisition-status transition was attempted.
    #[error("Invalid status transition: {from} -> {to}")]
    InvalidTransition { from: String, to: String },

    /// Configuration was missing or malformed.
    #[error("Configuration error: {0}")]
    Config(String),

    /// An underlying I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A network / external-service failure (HTTP transport, timeout, bad status).
    #[error("Network error: {0}")]
    Network(String),

    /// An unexpected internal failure.
    #[error("Internal error: {0}")]
    Internal(String),

    /// Any other error, carrying its source chain.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl AppError {
    /// A validation failure attributed to `field` (SKADI-T-0525), e.g.
    /// `AppError::field("api_key", "must be a non-empty string")`.
    #[must_use]
    pub fn field(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::ValidationField {
            field: field.into(),
            message: message.into(),
        }
    }

    /// The field this error blames, when it blames one.
    #[must_use]
    pub fn field_name(&self) -> Option<&str> {
        match self {
            Self::ValidationField { field, .. } => Some(field),
            _ => None,
        }
    }
}

/// Convenience alias used throughout the workspace.
pub type Result<T> = std::result::Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_messages_are_descriptive() {
        assert_eq!(
            AppError::NotFound("movie 7".into()).to_string(),
            "Not found: movie 7"
        );
        assert_eq!(
            AppError::InvalidTransition {
                from: "Missing".into(),
                to: "Imported".into(),
            }
            .to_string(),
            "Invalid status transition: Missing -> Imported"
        );
    }

    #[test]
    fn io_errors_convert_via_from() {
        fn fails() -> Result<()> {
            std::fs::File::open("/definitely/not/here/skadi-test")?;
            Ok(())
        }
        let err = fails().unwrap_err();
        assert!(matches!(err, AppError::Io(_)));
    }
}
