//! Mapping [`skadi_core::AppError`] to HTTP responses.
//!
//! Handlers return `Result<T, ApiError>`; `ApiError` wraps the workspace
//! [`AppError`] and implements [`IntoResponse`] so the status-code mapping
//! lives in exactly one place.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use skadi_core::AppError;

/// The JSON body returned for every error response.
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    /// A stable machine-readable kind (`not_found`, `validation`, …).
    pub error: &'static str,
    /// The human-readable message. For a validation error this is the reason
    /// alone — `error` already names the kind, so repeating it would render as
    /// "validation: Validation error: …" (SKADI-T-0525).
    pub message: String,
    /// The offending field, when the error names one — Sonarr's `propertyName`,
    /// so a settings form can put the message beside the right input rather than
    /// at the top of the page. Omitted entirely when there is no single field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

/// HTTP-layer error wrapper around [`AppError`].
#[derive(Debug)]
pub struct ApiError(pub AppError);

impl From<AppError> for ApiError {
    fn from(e: AppError) -> Self {
        ApiError(e)
    }
}

impl ApiError {
    /// The HTTP status + stable kind string for this error.
    fn parts(&self) -> (StatusCode, &'static str) {
        match &self.0 {
            AppError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            AppError::Validation(_) | AppError::ValidationField { .. } => {
                (StatusCode::BAD_REQUEST, "validation")
            }
            AppError::InvalidTransition { .. } => (StatusCode::CONFLICT, "invalid_transition"),
            AppError::Config(_) => (StatusCode::INTERNAL_SERVER_ERROR, "config"),
            AppError::Io(_) => (StatusCode::INTERNAL_SERVER_ERROR, "io"),
            AppError::Network(_) => (StatusCode::BAD_GATEWAY, "network"),
            AppError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
            AppError::Other(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error) = self.parts();
        let body = ErrorBody {
            error,
            // For a validation error the envelope's `error` field already says
            // "validation", so `AppError`'s own "Validation error: " prefix just
            // doubled it up — a client rendering `{error}: {message}` showed
            // "validation: Validation error: …" (SKADI-T-0525). Use the inner
            // reason directly.
            message: match &self.0 {
                AppError::Validation(m) => m.clone(),
                AppError::ValidationField { message, .. } => message.clone(),
                other => other.to_string(),
            },
            // Sonarr's per-field `propertyName`, so a settings form can put the
            // message next to the offending input instead of at the top.
            field: self.0.field_name().map(str::to_string),
        };
        (status, Json(body)).into_response()
    }
}

/// A `Json<T>` extractor whose rejections use this module's envelope
/// (SKADI-T-0458).
///
/// Axum's own `Json` rejection is a bare status plus plain text, so a malformed
/// body or a wrongly-typed field came back in a completely different shape from
/// every error the handlers produce — a client parsing `{error, message}` got
/// something it could not read, at exactly the moment it needed to know what it
/// had done wrong.
pub struct ApiJson<T>(pub T);

impl<S, T> axum::extract::FromRequest<S> for ApiJson<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(
        req: axum::extract::Request,
        state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(ApiJson(v)),
            // Both a syntax error and a type error are the caller's mistake, so
            // both are `validation` — the same kind a handler would return for a
            // body it could parse but not accept.
            Err(rejection) => Err(ApiError(AppError::Validation(rejection.body_text()))),
        }
    }
}

/// A `Query<T>` extractor whose rejections use the envelope (SKADI-T-0458).
pub struct ApiQuery<T>(pub T);

impl<S, T> axum::extract::FromRequestParts<S> for ApiQuery<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Query(v)) => Ok(ApiQuery(v)),
            Err(rejection) => Err(ApiError(AppError::Validation(rejection.body_text()))),
        }
    }
}

/// The envelope for a route that does not exist, so an unknown API path reads
/// like every other error (SKADI-T-0458).
pub async fn not_found_fallback() -> Response {
    ApiError(AppError::NotFound("no such route".into())).into_response()
}

/// The envelope for a known path with the wrong method.
pub async fn method_not_allowed_fallback() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(serde_json::json!({
            "error": "method_not_allowed",
            "message": "the route exists but does not accept this method",
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_status_codes() {
        assert_eq!(
            ApiError(AppError::NotFound("x".into())).parts().0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ApiError(AppError::Validation("x".into())).parts().0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ApiError(AppError::InvalidTransition {
                from: "a".into(),
                to: "b".into()
            })
            .parts()
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            ApiError(AppError::Internal("x".into())).parts().0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
