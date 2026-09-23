//! HTTP byte-range file serving, shared by every domain that has bytes to hand
//! out (SKADI-T-0574).
//!
//! Extracted from the audiobook audio route, which had the only correct Range
//! implementation in the codebase. That route's own comment framed it as
//! "resumable transfers, NOT streaming" — but byte-range serving *is* what a
//! video player seeks with. The behaviour was already right; only the framing
//! was audio-shaped.
//!
//! It lives here rather than being copied into movies and television because the
//! subtleties below are the kind that get fixed in one copy and not the others:
//! `If-Range` validation, suffix ranges, the zero-length guard, and what counts
//! as a *syntactically* invalid range (ignore, serve 200) versus an
//! *unsatisfiable* one (416).

use std::path::Path;

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use skadi_core::AppError;

use crate::error::ApiError;

/// Parse a single byte range against a known length.
///
/// * `None` — no range, or one that is syntactically invalid or unsupported.
///   RFC 7233 says an unparsable `Range` is ignored, so the caller serves 200.
/// * `Some(Err(()))` — well-formed but **unsatisfiable**: 416.
/// * `Some(Ok((start, end)))` — inclusive, clamped to the file.
#[must_use]
pub fn parse_byte_range(header: Option<&str>, len: u64) -> Option<Result<(u64, u64), ()>> {
    let spec = header?.strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None; // multi-range unsupported → ignore, serve 200
    }
    let (a, b) = spec.split_once('-')?;
    if a.is_empty() {
        // Suffix form `-N`: the last N bytes. `-0` is unsatisfiable (416).
        let n: u64 = b.trim().parse().ok()?;
        if n == 0 {
            return Some(Err(()));
        }
        if len == 0 {
            return Some(Err(()));
        }
        return Some(Ok((len.saturating_sub(n), len - 1)));
    }
    let start: u64 = a.trim().parse().ok()?;
    let end: u64 = match b.trim() {
        "" => len.saturating_sub(1),
        e => e.parse().ok()?,
    };
    // start past EOF → 416. Inverted/backwards range (e.g. 50-40) is invalid
    // syntax per RFC → ignore (200).
    if start >= len {
        return Some(Err(()));
    }
    if start > end {
        return None;
    }
    Some(Ok((start, end.min(len.saturating_sub(1)))))
}

/// RFC 1123 date string for `Last-Modified`/`If-Range`.
#[must_use]
pub fn httpdate_from_systemtime(t: std::time::SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Utc> = t.into();
    dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

/// Content type by container extension.
///
/// Video containers matter for playback, not just tidiness: a player handed
/// `application/octet-stream` may refuse to probe it at all.
#[must_use]
pub fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        // audio
        Some("m4b" | "m4a" | "aac") => "audio/mp4",
        Some("mp3") => "audio/mpeg",
        Some("flac") => "audio/flac",
        Some("ogg" | "opus") => "audio/ogg",
        Some("wav") => "audio/wav",
        // video
        Some("mkv") => "video/x-matroska",
        Some("webm") => "video/webm",
        Some("mp4" | "m4v" | "mov") => "video/mp4",
        Some("avi") => "video/x-msvideo",
        Some("ts" | "m2ts" | "mts") => "video/mp2t",
        Some("mpg" | "mpeg") => "video/mpeg",
        _ => "application/octet-stream",
    }
}

/// Serve `path` with single-range support.
///
/// `what` names the thing in error messages ("audio", "video") so a 404 says
/// which kind of file is missing rather than a generic "not found".
pub async fn serve_file_range(
    path: &Path,
    headers: &HeaderMap,
    what: &str,
) -> Result<Response, ApiError> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| ApiError(AppError::NotFound(format!("{what} file missing: {e}"))))?;
    let len = meta.len();
    let content_type = content_type_for(path);
    // Weak validator for If-Range resume correctness: a quality upgrade
    // rewriting this path between a client's pause and resume must invalidate
    // the range, not append mismatched bytes to a half-downloaded file.
    let last_modified: Option<String> = meta.modified().ok().map(httpdate_from_systemtime);

    // Honor If-Range: if the client's validator does not match, ignore its
    // Range and serve the whole file (RFC 7233 §3.2).
    let if_range_ok = headers
        .get(axum::http::header::IF_RANGE)
        .and_then(|v| v.to_str().ok())
        .zip(last_modified.as_deref())
        .map(|(want, have)| want == have)
        .unwrap_or(true);

    let range_header = if_range_ok
        .then(|| {
            headers
                .get(axum::http::header::RANGE)
                .and_then(|v| v.to_str().ok())
        })
        .flatten();

    let (status, start, end) = match parse_byte_range(range_header, len) {
        None => (StatusCode::OK, 0, len.saturating_sub(1)),
        Some(Ok((s, e))) => (StatusCode::PARTIAL_CONTENT, s, e),
        Some(Err(())) => {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(axum::http::header::CONTENT_RANGE, format!("bytes */{len}"))],
            )
                .into_response());
        }
    };

    let mut f = tokio::fs::File::open(path)
        .await
        .map_err(|e| ApiError(AppError::NotFound(format!("open {what} file: {e}"))))?;
    if start > 0 {
        use tokio::io::AsyncSeekExt;
        f.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|e| ApiError(AppError::Internal(format!("seek {what} file: {e}"))))?;
    }
    // Bytes actually available in [start, end] — 0 for an empty file, so we
    // never declare `Content-Length: 1` over a 0-byte body and hang the client.
    let window = if len == 0 {
        0
    } else {
        (end + 1).saturating_sub(start).min(len - start)
    };
    let reader = tokio::io::AsyncReadExt::take(f, window);
    let stream = tokio_util::io::ReaderStream::new(reader);
    let body = axum::body::Body::from_stream(stream);

    let mut resp = Response::builder()
        .status(status)
        .header(axum::http::header::CONTENT_TYPE, content_type)
        .header(axum::http::header::ACCEPT_RANGES, "bytes")
        .header(axum::http::header::CONTENT_LENGTH, window.to_string());
    if let Some(lm) = &last_modified {
        resp = resp.header(axum::http::header::LAST_MODIFIED, lm);
    }
    if status == StatusCode::PARTIAL_CONTENT {
        resp = resp.header(
            axum::http::header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{len}"),
        );
    }
    resp.body(body)
        .map_err(|e| ApiError(AppError::Internal(format!("build {what} response: {e}"))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_range_is_inclusive_and_clamped() {
        assert_eq!(
            parse_byte_range(Some("bytes=0-99"), 1000),
            Some(Ok((0, 99)))
        );
        // An end past EOF clamps rather than failing — clients routinely ask
        // for more than exists.
        assert_eq!(
            parse_byte_range(Some("bytes=900-9999"), 1000),
            Some(Ok((900, 999)))
        );
        // Open-ended.
        assert_eq!(
            parse_byte_range(Some("bytes=500-"), 1000),
            Some(Ok((500, 999)))
        );
    }

    #[test]
    fn a_suffix_range_takes_the_tail() {
        assert_eq!(
            parse_byte_range(Some("bytes=-100"), 1000),
            Some(Ok((900, 999)))
        );
        // Longer than the file is the whole file, not an error.
        assert_eq!(
            parse_byte_range(Some("bytes=-5000"), 1000),
            Some(Ok((0, 999)))
        );
    }

    #[test]
    fn unsatisfiable_and_invalid_are_different_answers() {
        // Well-formed but unsatisfiable → 416. RFC 7233 §4.4.
        assert_eq!(parse_byte_range(Some("bytes=1000-"), 1000), Some(Err(())));
        assert_eq!(parse_byte_range(Some("bytes=-0"), 1000), Some(Err(())));
        assert_eq!(parse_byte_range(Some("bytes=-1"), 0), Some(Err(())));
        // Syntactically invalid → ignore the header and serve 200, NOT 416.
        // Conflating the two is the classic bug: a client sending junk gets a
        // hard failure instead of the file.
        assert_eq!(parse_byte_range(Some("bytes=50-40"), 1000), None);
        assert_eq!(parse_byte_range(Some("bytes=abc"), 1000), None);
        assert_eq!(parse_byte_range(Some("chunks=0-9"), 1000), None);
        assert_eq!(parse_byte_range(None, 1000), None);
        // Multi-range is unsupported, not invalid: ignore and serve whole.
        assert_eq!(parse_byte_range(Some("bytes=0-9,20-29"), 1000), None);
    }

    #[test]
    fn video_containers_get_real_content_types() {
        // A player handed application/octet-stream may refuse to probe at all,
        // so the MKV case in particular is load-bearing: it is ~78 % of this
        // library.
        assert_eq!(content_type_for(Path::new("/x/a.mkv")), "video/x-matroska");
        assert_eq!(content_type_for(Path::new("/x/a.mp4")), "video/mp4");
        assert_eq!(content_type_for(Path::new("/x/a.m4v")), "video/mp4");
        assert_eq!(content_type_for(Path::new("/x/a.m2ts")), "video/mp2t");
        assert_eq!(content_type_for(Path::new("/x/A.MKV")), "video/x-matroska");
    }

    #[test]
    fn audio_content_types_are_unchanged() {
        // The audiobook route now shares this; its behaviour must not move.
        assert_eq!(content_type_for(Path::new("/x/a.m4b")), "audio/mp4");
        assert_eq!(content_type_for(Path::new("/x/a.mp3")), "audio/mpeg");
        assert_eq!(content_type_for(Path::new("/x/a.flac")), "audio/flac");
        assert_eq!(content_type_for(Path::new("/x/a.ogg")), "audio/ogg");
        assert_eq!(content_type_for(Path::new("/x/a.wav")), "audio/wav");
        assert_eq!(
            content_type_for(Path::new("/x/a.bin")),
            "application/octet-stream"
        );
    }
}
