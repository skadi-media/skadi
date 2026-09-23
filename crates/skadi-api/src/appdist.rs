//! Native-app distribution + zero-config discovery (SKADI-T-0340 / I-0049).
//!
//! Three pieces, all modeled on squire:
//!
//! 1. **APK serving** — `GET /app/{file}` streams files from `SKADI_APK_DIR`
//!    (a manifest.json + the published APK, written by
//!    `deploy/publish-apk.sh`). Mounted on the OUTER router, deliberately
//!    **unauthenticated** (squire SQUIRE-T-0051): a fresh phone downloads the
//!    app before it has any token, and the APK is not a secret on a home LAN.
//!    Path traversal is blocked (single flat component, no dot-prefixed names).
//!
//! 2. **Install QR** — `GET /api/v1/pair/apk` (authenticated, desktop UI):
//!    the LAN download URL for the current APK as a QR, host taken from the
//!    request's Host header exactly like `pair::pair_qr`.
//!
//! 3. **In-daemon mDNS advertisement** — [`advertise_mdns`] registers
//!    `_skadi._tcp` when `SKADI_MDNS_ADVERTISE=1`. Default OFF: in the
//!    Docker-for-Mac deploy, container multicast can never reach the LAN
//!    (deploy/advertise-mdns.sh does it host-side instead, SKADI-T-0339);
//!    this flag is for Linux/bare-metal daemons.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use qrcode::QrCode;
use qrcode::render::svg;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// The APK directory, when configured and present.
fn apk_dir() -> Option<PathBuf> {
    std::env::var_os("SKADI_APK_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

/// `manifest.json` written by the publish script beside the APK.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApkManifest {
    pub file: String,
    #[serde(default)]
    pub version_name: Option<String>,
    #[serde(default)]
    pub version_code: Option<u64>,
}

fn read_manifest(dir: &FsPath) -> Option<ApkManifest> {
    let raw = std::fs::read_to_string(dir.join("manifest.json")).ok()?;
    serde_json::from_str(&raw).ok()
}

/// A published filename is a flat `[A-Za-z0-9._-]+` — rejects traversal,
/// dotfiles, AND CR/LF or quotes that would corrupt the Content-Disposition
/// header (review pass 2, server finding 9).
fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// `GET /app/{file}` — STREAM a published file (never buffer the whole APK
/// into memory: it's the unauthenticated endpoint and N phones = N copies;
/// review pass 2, server finding 4). Flat safe names only.
async fn serve_app_file(Path(file): Path<String>, headers: axum::http::HeaderMap) -> Response {
    if !is_safe_name(&file) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let Some(dir) = apk_dir() else {
        return (StatusCode::NOT_FOUND, "no app published").into_response();
    };
    let path = dir.join(&file);
    if tokio::fs::metadata(&path).await.is_err() {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let mime = if file.ends_with(".apk") {
        "application/vnd.android.package-archive"
    } else if file.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    };

    // Range support (SKADI-T-0576). This route used to answer **every** request
    // with 200 from byte 0, `Range` header or not, and advertised no
    // `Accept-Ranges`.
    //
    // Android's DownloadManager — which Chrome uses for an APK — retries an
    // interrupted download with `Range: bytes=N-`. Served the whole file from
    // the start instead, it appends those bytes to the N it already holds, so
    // the file outgrows its own `Content-Length` and the download never
    // completes. Reported from a real phone as "the download isn't finishing",
    // and reproduced here: `curl -r 0-1023` returned 200 with all 3,842,405
    // bytes rather than 206 with 1024.
    //
    // `serve_file_range` is the shared helper the video routes use
    // (SKADI-T-0574); the content type is overridden afterwards because an APK
    // is not something it should have to know about.
    let mut resp = match crate::ranged::serve_file_range(&path, &headers, "app").await {
        Ok(r) => r,
        Err(_) => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(mime),
    );
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!("attachment; filename=\"{file}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    resp
}

/// Unauthenticated outer-router routes (the phone has no token yet).
pub fn app_dist_router() -> Router {
    Router::new().route("/app/{file}", get(serve_app_file))
}

/// The current installable build: LAN URL + QR (`available:false` when nothing
/// is published). Mirrors squire's `app_install.rs`.
#[derive(Debug, Default, Serialize)]
pub struct ApkInstallView {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qr_svg: Option<String>,
}

/// `GET /api/v1/pair/apk` — install QR for the published APK.
async fn pair_apk(headers: HeaderMap) -> Json<ApkInstallView> {
    let Some(dir) = apk_dir() else {
        return Json(ApkInstallView::default());
    };
    let Some(m) = read_manifest(&dir) else {
        return Json(ApkInstallView::default());
    };
    if !dir.join(&m.file).is_file() {
        return Json(ApkInstallView::default());
    }
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1:8080");
    let url = format!("http://{host}/app/{}", m.file);
    let qr_svg = QrCode::new(url.as_bytes()).ok().map(|c| {
        c.render::<svg::Color>()
            .min_dimensions(220, 220)
            .dark_color(svg::Color("#e6edf3"))
            .light_color(svg::Color("#00000000"))
            .build()
    });
    Json(ApkInstallView {
        available: true,
        version_name: m.version_name,
        url: Some(url),
        qr_svg,
    })
}

/// Authenticated `/api/v1` routes.
pub fn pair_apk_router() -> Router<Arc<AppState>> {
    Router::new().route("/pair/apk", get(pair_apk))
}

/// Register `_skadi._tcp` via mDNS when `SKADI_MDNS_ADVERTISE=1`.
/// Returns whether advertisement started (the daemon logs the outcome).
pub fn advertise_mdns(port: u16) -> bool {
    if std::env::var("SKADI_MDNS_ADVERTISE").as_deref() != Ok("1") {
        return false;
    }
    let Ok(daemon) = mdns_sd::ServiceDaemon::new() else {
        tracing::warn!("mDNS advertisement requested but the responder failed to start");
        return false;
    };
    let host = hostname();
    let info = match mdns_sd::ServiceInfo::new(
        "_skadi._tcp.local.",
        "skadi",
        &format!("{host}.local."),
        (),
        port,
        None,
    ) {
        Ok(i) => i.enable_addr_auto(),
        Err(e) => {
            tracing::warn!("mDNS service info invalid: {e}");
            return false;
        }
    };
    match daemon.register(info) {
        Ok(()) => {
            tracing::info!(port, "advertising _skadi._tcp over mDNS");
            // Stash the handle (was mem::forget) so graceful shutdown can send
            // goodbye packets instead of letting the record linger to TTL
            // (SKADI-T-0346). ServiceDaemon is a cheap cloneable handle.
            let _ = MDNS.set(daemon);
            true
        }
        Err(e) => {
            tracing::warn!("mDNS register failed: {e}");
            false
        }
    }
}

/// The registered mDNS responder handle, kept for lifetime + graceful shutdown.
static MDNS: std::sync::OnceLock<mdns_sd::ServiceDaemon> = std::sync::OnceLock::new();

/// Unregister `_skadi._tcp` (sends mDNS goodbye/TTL-0 packets) and stop the
/// responder, so clients drop the entry immediately instead of waiting out the
/// record TTL (SKADI-T-0346). Blocks briefly for the goodbye to flush; a no-op
/// when advertisement never started. Call from the daemon's graceful-shutdown path.
pub fn shutdown_mdns() {
    let Some(daemon) = MDNS.get() else { return };
    // Full service name = "<instance>.<type>" (instance "skadi").
    if let Ok(recv) = daemon.unregister("skadi._skadi._tcp.local.") {
        // Wait (bounded) for the goodbye to actually go out before we stop.
        let _ = recv.recv_timeout(std::time::Duration::from_secs(2));
    }
    let _ = daemon.shutdown();
    tracing::info!("mDNS responder shut down (goodbye sent)");
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().trim_end_matches(".local").to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "skadi-host".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn app_file_rejects_traversal_and_dotfiles() {
        for bad in ["../secret", ".hidden", "a/b.apk"] {
            let resp = serve_app_file(Path(bad.to_string()), axum::http::HeaderMap::new()).await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{bad}");
        }
    }

    #[tokio::test]
    async fn pair_apk_serves_manifest_backed_qr() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("skadi-1.apk"), b"fake apk").unwrap();
        std::fs::write(
            dir.path().join("manifest.json"),
            r#"{"file":"skadi-1.apk","version_name":"0.1.0","version_code":1}"#,
        )
        .unwrap();
        // SAFETY: test-local env mutation.
        unsafe { std::env::set_var("SKADI_APK_DIR", dir.path()) };

        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "203.0.113.27:8090".parse().unwrap());
        let Json(v) = pair_apk(headers).await;
        assert!(v.available);
        assert_eq!(
            v.url.as_deref(),
            Some("http://203.0.113.27:8090/app/skadi-1.apk")
        );
        assert!(v.qr_svg.is_some());
        unsafe { std::env::remove_var("SKADI_APK_DIR") };
    }
}
