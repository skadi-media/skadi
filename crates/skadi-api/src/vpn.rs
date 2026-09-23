//! VPN status + kill-switch via gluetun's control API (SKADI-T-0292).
//!
//! gluetun is a required component of the deploy stack (the download worker shares
//! its network namespace + kill-switched firewall), so skadi ties directly to its
//! control server (`http://gluetun:8000` by default). We read the tunnel state +
//! exit IP/location to show prominently on Downloads, and (with the control-server
//! API key) can stop/start the tunnel — a real kill-switch that structurally drops
//! the worker's egress with no leak.
//!
//! Read routes (`/v1/publicip/ip`, `/v1/vpn/status`) are open on current gluetun;
//! the write route (`PUT /v1/vpn/status`) wants the API key. Everything degrades
//! gracefully when gluetun is unreachable (e.g. running outside the stack).

use std::time::Duration;

use serde::{Deserialize, Serialize};

const DEFAULT_CONTROL_URL: &str = "http://gluetun:8000";

/// gluetun control-server base URL (env override for non-default deploys).
fn control_url() -> String {
    std::env::var("SKADI_GLUETUN_CONTROL_URL").unwrap_or_else(|_| DEFAULT_CONTROL_URL.to_string())
}

/// Optional control-server API key (`X-API-Key`) — required for the kill-switch
/// once gluetun's auth is enabled.
fn api_key() -> Option<String> {
    std::env::var("SKADI_GLUETUN_CONTROL_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .build()
        .unwrap_or_default()
}

fn with_key(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    match api_key() {
        Some(k) => req.header("X-API-Key", k),
        None => req,
    }
}

#[derive(Deserialize)]
struct PublicIp {
    public_ip: Option<String>,
    country: Option<String>,
    city: Option<String>,
    region: Option<String>,
}

#[derive(Deserialize)]
struct VpnStatusResp {
    status: Option<String>,
}

/// The VPN panel payload.
#[derive(Debug, Clone, Serialize, Default)]
pub struct VpnStatus {
    /// Could we reach gluetun's control server at all?
    pub reachable: bool,
    /// Tunnel up (gluetun reports `running`).
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}

/// Query gluetun for the current VPN state + exit identity.
pub async fn status() -> VpnStatus {
    let base = control_url();
    let http = client();

    let vpn = with_key(http.get(format!("{base}/v1/vpn/status")))
        .send()
        .await
        .ok();
    let reachable = vpn.is_some();
    let connected = match vpn {
        Some(resp) => resp
            .json::<VpnStatusResp>()
            .await
            .ok()
            .and_then(|s| s.status)
            .map(|s| s.eq_ignore_ascii_case("running"))
            .unwrap_or(false),
        None => false,
    };

    let ip = with_key(http.get(format!("{base}/v1/publicip/ip")))
        .send()
        .await
        .ok();
    let pub_ip = match ip {
        Some(resp) => resp.json::<PublicIp>().await.ok(),
        None => None,
    };

    VpnStatus {
        reachable,
        connected,
        exit_ip: pub_ip.as_ref().and_then(|p| p.public_ip.clone()),
        country: pub_ip.as_ref().and_then(|p| p.country.clone()),
        city: pub_ip.as_ref().and_then(|p| p.city.clone()),
        region: pub_ip.as_ref().and_then(|p| p.region.clone()),
    }
}
