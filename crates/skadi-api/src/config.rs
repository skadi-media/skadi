//! Daemon configuration loaded from the environment.
//!
//! The bootstrap surface is intentionally tiny — three env vars plus the
//! credential-encryption key that `skadi-store` reads independently:
//!
//! | Env var               | Required | Default               | Used for                              |
//! |-----------------------|----------|-----------------------|---------------------------------------|
//! | [`DATABASE_URL_ENV`]  | no       | [`DEFAULT_DATABASE_URL`] | Backing store URL (sqlite:// or postgres://) |
//! | [`BIND_ADDR_ENV`]     | no       | `127.0.0.1:8080`      | axum listen address                   |
//! | [`BEARER_ENV`]        | no       | (open mode + warn)    | Bearer token required on every protected route |
//!
//! `SKADI_SECRET_KEY` is *not* read here — that env var belongs to
//! `skadi_store::crypto` for credential encryption, and reusing it for the API
//! bearer token would conflate two unrelated secrets.

use std::net::SocketAddr;

use skadi_core::{AppError, Result};
use skadi_store::DEFAULT_DATABASE_URL;

use crate::auth::BEARER_ENV;

/// Env var holding the backing-store URL.
pub const DATABASE_URL_ENV: &str = "SKADI_DATABASE_URL";

/// Env var holding the axum listen address (`host:port`).
pub const BIND_ADDR_ENV: &str = "SKADI_BIND_ADDR";

/// Default listen address when [`BIND_ADDR_ENV`] is unset.
pub const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8080";

/// Runtime configuration for the daemon. Built by [`Config::from_env`] in the
/// binary, or directly in tests.
#[derive(Clone, Debug)]
pub struct Config {
    /// `sqlite://…` or `postgres://…`. Defaulted to [`DEFAULT_DATABASE_URL`].
    pub database_url: String,
    /// Address axum binds to.
    pub bind_addr: SocketAddr,
    /// Bearer token required on every protected route. `None` puts the API in
    /// **open mode**; [`auth::bearer_auth`](crate::auth::bearer_auth) skips the
    /// check entirely and [`serve`](crate::serve::serve) logs a warning at
    /// startup.
    pub bearer_token: Option<String>,
}

impl Config {
    /// Load from env, applying defaults. The only failure mode is an
    /// unparseable [`BIND_ADDR_ENV`].
    pub fn from_env() -> Result<Self> {
        let database_url =
            std::env::var(DATABASE_URL_ENV).unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string());
        let bind_addr_raw =
            std::env::var(BIND_ADDR_ENV).unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_string());
        let bind_addr: SocketAddr = bind_addr_raw.parse().map_err(|e| {
            AppError::Config(format!("invalid {BIND_ADDR_ENV} {bind_addr_raw:?}: {e}"))
        })?;
        let bearer_token = std::env::var(BEARER_ENV).ok().filter(|s| !s.is_empty());
        Ok(Self {
            database_url,
            bind_addr,
            bearer_token,
        })
    }

    /// Re-resolve the **table-backed** runtime fields (`bind_addr`, `api_token`)
    /// from a [`ConfigView`](skadi_config::ConfigView), replacing the
    /// env-derived values built by [`from_env`](Self::from_env). Called after
    /// boot seeding (SKADI-I-0014), so the running daemon reads these from the
    /// `config` table — env was seeded into it, so values are unchanged. The
    /// Tier-0 `database_url` is left untouched (it is how we reached the table).
    pub fn resolve(&mut self, view: &skadi_config::ConfigView) -> Result<()> {
        let cfg_err = |e: skadi_config::ConfigError| AppError::Config(e.to_string());
        let bind = view.get_string("bind_addr").map_err(cfg_err)?;
        self.bind_addr = bind
            .parse()
            .map_err(|e| AppError::Config(format!("invalid bind_addr {bind:?}: {e}")))?;
        self.bearer_token = view.get_opt_string("api_token").map_err(cfg_err)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_valid_bind_addr() {
        let cfg = Config {
            database_url: DEFAULT_DATABASE_URL.into(),
            bind_addr: "0.0.0.0:9000".parse().unwrap(),
            bearer_token: None,
        };
        assert_eq!(cfg.bind_addr.port(), 9000);
    }
}
