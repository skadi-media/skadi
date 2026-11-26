//! The piece probe: how many pieces (extents) each library file is in, asked
//! of the storage host over SSH (SKADI-T-0775, design SKADI-D-0040).
//!
//! The mount cannot say: an NFS client has no extent map. The storage host
//! can, so the probe opens an SSH connection, runs one fixed command per file
//! and writes the count to `library_files.pieces`.
//!
//! * **Off by default** (`storage.probe.pieces.enabled`). While it is off, or
//!   not set up, or failed, no connection is opened.
//! * **A trickle**: at most `storage.probe.pieces.per_hour` files (120).
//! * **Read only**: the command opens the file to read its extent map. No
//!   sudo, no write, no helper installed on the host.
//! * **It stops when it cannot work.** A reason that will not fix itself (the
//!   login is refused, the host key changed, the host has no tool, the path
//!   map is wrong) is written to `storage_probe_state`, and the probe stays
//!   off until an operator clears it. Only this probe stops.
//!
//! The credential lives in the vault ([`skadi_store::CredentialRepo`]) and is
//! never logged: [`ProbeCredential`] has a `Debug` that prints no secret, and
//! every error text is built here from fixed words and the host's own output.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use skadi_core::Result;
use skadi_store::{
    ConfigRepo, ConfigSource, CredentialRepo, LibraryFile, LibraryFileRepo, ProbeState,
    StorageProbeRepo, Store,
};
use tokio_util::sync::CancellationToken;

/// The name of this probe in `storage_probe_state`.
pub const PROBE: &str = "pieces";

pub const ENABLED_KEY: &str = "storage.probe.pieces.enabled";
pub const PER_HOUR_KEY: &str = "storage.probe.pieces.per_hour";
/// A measured file is measured again after this many days: a defragment
/// changes the count and nothing the mount shows. 0 = never.
pub const REMEASURE_DAYS_KEY: &str = "storage.probe.pieces.remeasure_days";
pub const HOST_KEY: &str = "storage.probe.host";
pub const PORT_KEY: &str = "storage.probe.port";
pub const USER_KEY: &str = "storage.probe.user";
/// `<the library root as Skadi sees it>=<the same folder on the host>`.
pub const PATH_MAP_KEY: &str = "storage.probe.path_map";
/// The host's public key, written at the first connection and checked at
/// every later one. Not a secret.
pub const PINNED_KEY: &str = "storage.probe.host_key";

/// Where the credential is in the vault.
pub const CREDENTIAL_KIND: &str = "storage_probe";
pub const CREDENTIAL_ID: &str = "ssh";

/// How often the worker looks for files to measure.
pub const TICK: Duration = Duration::from_secs(5 * 60);
/// The wait before the first look after the daemon starts.
const START_DELAY: Duration = Duration::from_secs(2 * 60);
/// Longest one step of the SSH conversation may take.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);
/// This many failures in a row that could have passed by themselves (the host
/// did not answer) become a recorded failure.
pub const TRANSIENT_LIMIT: u32 = 5;
/// This many files in a row that the mount has and the host does not mean the
/// path map is wrong.
pub const MISSING_LIMIT: u32 = 3;

/// How the probe logs in: a password or a private key. Kept in the vault as
/// JSON.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ProbeCredential {
    Password {
        secret: String,
    },
    Key {
        /// An OpenSSH or PEM private key.
        secret: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        passphrase: Option<String>,
    },
}

impl ProbeCredential {
    /// `password` or `key`: safe to show.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            ProbeCredential::Password { .. } => "password",
            ProbeCredential::Key { .. } => "key",
        }
    }
}

/// Never prints the secret.
impl std::fmt::Debug for ProbeCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ProbeCredential({})", self.kind())
    }
}

/// Read the credential from the vault.
pub async fn load_credential(store: &Store) -> Result<Option<ProbeCredential>> {
    let Some(raw) = store.get_secret(CREDENTIAL_KIND, CREDENTIAL_ID).await? else {
        return Ok(None);
    };
    // A stored value this version cannot read counts as none; the text of
    // the parse error could quote the secret, so it is dropped.
    Ok(serde_json::from_str(&raw).ok())
}

/// Write the credential to the vault.
pub async fn save_credential(store: &Store, credential: &ProbeCredential) -> Result<()> {
    let raw = serde_json::to_string(credential)
        .map_err(|_| skadi_core::AppError::Internal("encoding the probe credential".into()))?;
    store.set_secret(CREDENTIAL_KIND, CREDENTIAL_ID, &raw).await
}

/// Remove the credential from the vault.
pub async fn delete_credential(store: &Store) -> Result<()> {
    store.delete_secret(CREDENTIAL_KIND, CREDENTIAL_ID).await
}

/// The probe's settings, as the config plane holds them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeSettings {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// `(root as Skadi sees it, root on the host)`.
    pub path_map: Option<(PathBuf, String)>,
    pub per_hour: u32,
    pub remeasure_days: u32,
    /// The pinned host key, when a connection has been made.
    pub pinned: Option<String>,
}

async fn config(store: &Store, key: &str) -> String {
    let stored = store.get_config(key).await.ok().flatten();
    stored
        .map(|e| e.value)
        .filter(|v| !v.trim().is_empty())
        .or_else(|| skadi_config::default_for(key).ok().map(str::to_string))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// `<skadi root>=<host root>` → the two roots. `None` when it is not that.
#[must_use]
pub fn parse_path_map(raw: &str) -> Option<(PathBuf, String)> {
    let (local, remote) = raw.trim().split_once('=')?;
    let (local, remote) = (local.trim(), remote.trim());
    (local.starts_with('/') && remote.starts_with('/')).then(|| {
        (
            PathBuf::from(local),
            remote.trim_end_matches('/').to_string(),
        )
    })
}

impl ProbeSettings {
    pub async fn load(store: &Store) -> Self {
        let pinned = config(store, PINNED_KEY).await;
        Self {
            enabled: config(store, ENABLED_KEY)
                .await
                .eq_ignore_ascii_case("true"),
            host: config(store, HOST_KEY).await,
            port: config(store, PORT_KEY).await.parse().unwrap_or(22),
            user: config(store, USER_KEY).await,
            path_map: parse_path_map(&config(store, PATH_MAP_KEY).await),
            per_hour: config(store, PER_HOUR_KEY).await.parse().unwrap_or(120),
            remeasure_days: config(store, REMEASURE_DAYS_KEY)
                .await
                .parse()
                .unwrap_or(30),
            pinned: (!pinned.is_empty()).then_some(pinned),
        }
    }

    /// The settings the probe cannot run without and does not have, by the
    /// name of their config key.
    #[must_use]
    pub fn missing(&self, has_credential: bool) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.host.is_empty() {
            missing.push(HOST_KEY);
        }
        if self.user.is_empty() {
            missing.push(USER_KEY);
        }
        if self.path_map.is_none() {
            missing.push(PATH_MAP_KEY);
        }
        if !has_credential {
            missing.push("the credential");
        }
        missing
    }

    /// The path of a library file on the host, or `None` when the file is
    /// not under the mapped root.
    #[must_use]
    pub fn host_path(&self, local: &Path) -> Option<String> {
        let (root, remote) = self.path_map.as_ref()?;
        let rest = local.strip_prefix(root).ok()?;
        Some(format!("{remote}/{}", rest.to_string_lossy()))
    }
}

/// Files to measure in one tick, so that ticks add up to `per_hour`. At least
/// one, so a low rate still moves.
#[must_use]
pub fn batch_size(per_hour: u32, tick: Duration) -> usize {
    let per_tick = u64::from(per_hour) * tick.as_secs() / 3600;
    per_tick.max(1) as usize
}

/// Why a count was not had.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeError {
    /// The host has no such file.
    Missing,
    /// It may pass by itself: the host did not answer, the connection broke.
    Transient(String),
    /// It will not pass by itself. The text is shown to the operator.
    Permanent(String),
}

/// An open conversation with the storage host.
#[async_trait]
pub trait PieceCounter: Send {
    /// The piece count of the file at `host_path`.
    async fn count(&mut self, host_path: &str) -> std::result::Result<i64, ProbeError>;
}

/// Opens conversations with the storage host. Abstracted so the rules of the
/// worker are tested without a network.
#[async_trait]
pub trait Connector: Send + Sync {
    /// Connect and log in. Gives the counter and the host key the host
    /// showed, to pin at the first connection.
    async fn connect(
        &self,
        settings: &ProbeSettings,
        credential: &ProbeCredential,
    ) -> std::result::Result<(Box<dyn PieceCounter>, String), ProbeError>;
}

// The command is one fixed text; the path is its only variable and goes in
// single quotes. The script has no single quote in it.
const FIEMAP_PY: &str = r#"import fcntl,os,struct,sys
p=sys.argv[1]
try:
    fd=os.open(p,os.O_RDONLY)
except FileNotFoundError:
    print("skadi-probe missing"); sys.exit(44)
except OSError as e:
    print("skadi-probe denied %s"%e.strerror); sys.exit(46)
n=0;s=0
try:
    while True:
        b=bytearray(struct.pack("=QQLLLL",s,0xFFFFFFFFFFFFFFFF-s,0,0,512,0)+bytes(56*512))
        fcntl.ioctl(fd,0xC020660B,b)
        m=struct.unpack_from("=L",b,20)[0]
        if not m: break
        n+=m
        lo,ph,ln=struct.unpack_from("=QQQ",b,32+56*(m-1))
        if struct.unpack_from("=L",b,32+56*(m-1)+40)[0]&1: break
        s=lo+ln
except OSError as e:
    print("skadi-probe unsupported %s"%e.strerror); sys.exit(48)
print("skadi-pieces %d"%n)"#;

/// A path in single quotes for a POSIX shell.
#[must_use]
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The command the probe runs on the host for one file: `filefrag` when the
/// host has it, else a short `python3` read of the extent map.
#[must_use]
pub fn remote_command(host_path: &str) -> String {
    let path = shell_quote(host_path);
    format!(
        "if [ ! -e {path} ]; then echo 'skadi-probe missing'; exit 44; \
         elif command -v filefrag >/dev/null 2>&1; then filefrag -- {path} 2>&1; \
         elif command -v python3 >/dev/null 2>&1; then python3 -c '{FIEMAP_PY}' {path} 2>&1; \
         else echo 'skadi-probe no-tool'; exit 47; fi"
    )
}

/// The host's answer, cut to what an operator needs and a log can hold.
fn excerpt(output: &str) -> String {
    let line = output
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("");
    line.chars().take(200).collect()
}

/// Read the answer of [`remote_command`].
pub fn parse_answer(exit: Option<u32>, output: &str) -> std::result::Result<i64, ProbeError> {
    for line in output.lines().map(str::trim) {
        if let Some(n) = line.strip_prefix("skadi-pieces ") {
            return n.trim().parse().map_err(|_| {
                ProbeError::Permanent(format!("the host gave no number: {}", excerpt(line)))
            });
        }
        // filefrag: `<path>: 12 extents found`.
        if let Some(head) = line
            .strip_suffix(" extents found")
            .or_else(|| line.strip_suffix(" extent found"))
            && let Some(n) = head.rsplit(' ').next().and_then(|n| n.parse().ok())
        {
            return Ok(n);
        }
    }
    let said = excerpt(output);
    if said.starts_with("skadi-probe missing") || said.contains("No such file or directory") {
        return Err(ProbeError::Missing);
    }
    if said.starts_with("skadi-probe no-tool") {
        return Err(ProbeError::Permanent(
            "the storage host has neither filefrag nor python3".into(),
        ));
    }
    if said.starts_with("skadi-probe denied") {
        return Err(ProbeError::Permanent(format!(
            "the probe's user may not read the library on the host: {said}"
        )));
    }
    if said.starts_with("skadi-probe unsupported") {
        return Err(ProbeError::Permanent(format!(
            "the host's filesystem does not give an extent map: {said}"
        )));
    }
    match exit {
        // The channel closed with no exit status and no answer: the
        // connection broke.
        None if said.is_empty() => Err(ProbeError::Transient(
            "the host closed the connection with no answer".into(),
        )),
        _ => Err(ProbeError::Permanent(format!(
            "the host's answer was not understood (exit {}): {said}",
            exit.map_or_else(|| "none".to_string(), |c| c.to_string())
        ))),
    }
}

/// The real connector: SSH with `russh`.
pub struct SshConnector;

struct HostKeyCheck {
    pinned: Option<String>,
    seen: Arc<std::sync::Mutex<Option<String>>>,
}

impl russh::client::Handler for HostKeyCheck {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        // A host certificate is not a key that can be pinned: refused.
        let russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Ok(false);
        };
        let Ok(shown) = key.to_openssh() else {
            return Ok(false);
        };
        let ok = self.pinned.as_deref().is_none_or(|p| p == shown);
        if let Ok(mut seen) = self.seen.lock() {
            *seen = Some(shown);
        }
        Ok(ok)
    }
}

async fn step<T>(
    what: &str,
    fut: impl std::future::Future<Output = std::result::Result<T, russh::Error>>,
) -> std::result::Result<T, ProbeError> {
    match tokio::time::timeout(STEP_TIMEOUT, fut).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(ProbeError::Transient(format!("{what}: {e}"))),
        Err(_) => Err(ProbeError::Transient(format!(
            "{what}: no answer in {} s",
            STEP_TIMEOUT.as_secs()
        ))),
    }
}

#[async_trait]
impl Connector for SshConnector {
    async fn connect(
        &self,
        settings: &ProbeSettings,
        credential: &ProbeCredential,
    ) -> std::result::Result<(Box<dyn PieceCounter>, String), ProbeError> {
        let seen = Arc::new(std::sync::Mutex::new(None));
        let handler = HostKeyCheck {
            pinned: settings.pinned.clone(),
            seen: Arc::clone(&seen),
        };
        let config = Arc::new(russh::client::Config {
            inactivity_timeout: Some(STEP_TIMEOUT * 2),
            ..Default::default()
        });
        let addr = (settings.host.clone(), settings.port);
        let connected = step("connecting", russh::client::connect(config, addr, handler)).await;
        let shown = seen.lock().ok().and_then(|s| s.clone());
        let mut handle = match connected {
            Ok(handle) => handle,
            Err(e) => {
                // The handshake fails when the key check says no. Tell that
                // apart from a host that is down: it will not pass by itself.
                return Err(match (&settings.pinned, &shown) {
                    (Some(pinned), Some(shown)) if pinned != shown => ProbeError::Permanent(
                        "the storage host's key is not the one that was pinned".into(),
                    ),
                    (_, None) | (None, _) | (Some(_), Some(_)) => e,
                });
            }
        };
        let Some(shown) = shown else {
            return Err(ProbeError::Transient("the host showed no key".into()));
        };
        let user = settings.user.clone();
        let auth = match credential {
            ProbeCredential::Password { secret } => {
                step("logging in", handle.authenticate_password(user, secret)).await?
            }
            ProbeCredential::Key { secret, passphrase } => {
                // The parse error could quote the key: only fixed words go out.
                let key = russh::keys::decode_secret_key(secret, passphrase.as_deref()).map_err(
                    |_| {
                        ProbeError::Permanent(
                            "the private key could not be read (wrong format or passphrase)".into(),
                        )
                    },
                )?;
                let hash = step("logging in", handle.best_supported_rsa_hash())
                    .await?
                    .flatten();
                let key = russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), hash);
                step("logging in", handle.authenticate_publickey(user, key)).await?
            }
        };
        if !auth.success() {
            return Err(ProbeError::Permanent(format!(
                "the storage host refused the {} of user {}",
                credential.kind(),
                settings.user
            )));
        }
        Ok((Box::new(SshCounter { handle }), shown))
    }
}

struct SshCounter {
    handle: russh::client::Handle<HostKeyCheck>,
}

#[async_trait]
impl PieceCounter for SshCounter {
    async fn count(&mut self, host_path: &str) -> std::result::Result<i64, ProbeError> {
        let mut channel = step("opening a channel", self.handle.channel_open_session()).await?;
        step(
            "sending the command",
            channel.exec(true, remote_command(host_path)),
        )
        .await?;
        let mut output = Vec::new();
        let mut exit = None;
        let read = async {
            while let Some(msg) = channel.wait().await {
                match msg {
                    russh::ChannelMsg::Data { data }
                    | russh::ChannelMsg::ExtendedData { data, .. } => {
                        // A count is one short line; a host that floods is cut.
                        if output.len() < 16 * 1024 {
                            output.extend_from_slice(&data);
                        }
                    }
                    russh::ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
                    _ => {}
                }
            }
        };
        if tokio::time::timeout(STEP_TIMEOUT * 4, read).await.is_err() {
            return Err(ProbeError::Transient("the count took too long".into()));
        }
        parse_answer(exit, &String::from_utf8_lossy(&output))
    }
}

/// What one tick of the worker did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tick {
    /// The flag is off.
    Off,
    /// A setting or the credential is not there yet.
    NotSetUp(Vec<&'static str>),
    /// The probe failed earlier and waits for an operator.
    Stopped,
    /// No file waits.
    Idle,
    /// This many files were measured.
    Measured(usize),
    /// The host did not answer; the probe tries again at the next tick.
    Retrying(String),
    /// The probe failed now, with this reason, and has stopped.
    Failed(String),
}

/// Measures the pieces of the library's files, a few at a time.
pub struct PieceProbeWorker {
    store: Store,
    connector: Arc<dyn Connector>,
    tick: Duration,
    start_delay: Duration,
    /// Failures in a row that could have passed by themselves.
    transient: AtomicU32,
    /// Files in a row the mount has and the host does not.
    missing: AtomicU32,
}

impl PieceProbeWorker {
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self::with_connector(store, Arc::new(SshConnector))
    }

    #[must_use]
    pub fn with_connector(store: Store, connector: Arc<dyn Connector>) -> Self {
        Self {
            store,
            connector,
            tick: TICK,
            start_delay: START_DELAY,
            transient: AtomicU32::new(0),
            missing: AtomicU32::new(0),
        }
    }

    /// Shorten the waits. Tests only.
    #[must_use]
    pub fn with_pauses(mut self, start_delay: Duration, tick: Duration) -> Self {
        self.start_delay = start_delay;
        self.tick = tick;
        self
    }

    async fn fail(&self, reason: String) -> Tick {
        tracing::error!(reason, "piece probe: stopped; it runs again when cleared");
        if let Err(e) = self
            .store
            .fail_storage_probe(PROBE, &reason, Utc::now())
            .await
        {
            tracing::warn!(error = %e, "piece probe: could not record the failure");
        }
        self.transient.store(0, Ordering::Relaxed);
        self.missing.store(0, Ordering::Relaxed);
        Tick::Failed(reason)
    }

    /// A failure that could pass by itself: try again later, but not for
    /// ever.
    async fn transient(&self, why: String) -> Tick {
        let n = self.transient.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= TRANSIENT_LIMIT {
            return self
                .fail(format!("the storage host did not answer {n} times: {why}"))
                .await;
        }
        tracing::warn!(why, attempt = n, "piece probe: will try again");
        Tick::Retrying(why)
    }

    /// The files of this tick: the ones that wait, then the ones measured
    /// longest ago when their count is older than the re-measure age.
    async fn batch(
        &self,
        settings: &ProbeSettings,
        now: DateTime<Utc>,
    ) -> Result<Vec<LibraryFile>> {
        let want = batch_size(settings.per_hour, self.tick);
        let mut files = self
            .store
            .library_files_awaiting_pieces(want as i64)
            .await?;
        if files.len() < want && settings.remeasure_days > 0 {
            let cutoff = now - chrono::Duration::days(i64::from(settings.remeasure_days));
            let old = self
                .store
                .library_files_oldest_pieces((want - files.len()) as i64)
                .await?;
            files.extend(
                old.into_iter()
                    .filter(|f| f.pieces_at.is_some_and(|at| at < cutoff)),
            );
        }
        Ok(files)
    }

    /// One look for work. Never fails: what went wrong is in the answer, the
    /// probe state and the log.
    pub async fn tick_once(&self) -> Tick {
        let settings = ProbeSettings::load(&self.store).await;
        if !settings.enabled {
            return Tick::Off;
        }
        match self.store.storage_probe_status(PROBE).await {
            Ok(Some(s)) if s.state == ProbeState::Failed => return Tick::Stopped,
            Ok(_) => {}
            Err(e) => return Tick::Retrying(format!("reading the probe state: {e}")),
        }
        let credential = match load_credential(&self.store).await {
            Ok(c) => c,
            Err(e) => return Tick::Retrying(format!("reading the credential: {e}")),
        };
        let missing = settings.missing(credential.is_some());
        let Some(credential) = credential.filter(|_| missing.is_empty()) else {
            return Tick::NotSetUp(missing);
        };
        let files = match self.batch(&settings, Utc::now()).await {
            Ok(files) => files,
            Err(e) => return Tick::Retrying(format!("listing the files: {e}")),
        };
        if files.is_empty() {
            return Tick::Idle;
        }

        let (mut counter, shown) = match self.connector.connect(&settings, &credential).await {
            Ok(open) => open,
            Err(ProbeError::Permanent(why)) => return self.fail(why).await,
            Err(ProbeError::Transient(why)) => return self.transient(why).await,
            Err(ProbeError::Missing) => return self.transient("no such host".into()).await,
        };
        if settings.pinned.is_none() {
            // The first connection: trust this key from now on.
            if let Err(e) = self
                .store
                .set_config(PINNED_KEY, &shown, ConfigSource::Runtime)
                .await
            {
                return Tick::Retrying(format!("pinning the host key: {e}"));
            }
            tracing::info!("piece probe: pinned the storage host's key");
        }
        self.transient.store(0, Ordering::Relaxed);

        let mut measured = 0;
        for file in files {
            let Some(host_path) = settings.host_path(Path::new(&file.path)) else {
                return self
                    .fail(format!(
                        "{PATH_MAP_KEY} does not cover the library file {}",
                        file.path
                    ))
                    .await;
            };
            match counter.count(&host_path).await {
                Ok(pieces) => {
                    self.missing.store(0, Ordering::Relaxed);
                    match self
                        .store
                        .record_library_file_pieces(&file.id, pieces, Utc::now())
                        .await
                    {
                        Ok(_) => measured += 1,
                        Err(e) => tracing::warn!(error = %e, "piece probe: could not store"),
                    }
                }
                Err(ProbeError::Missing) => {
                    // The host has no such file. If the mount has it, the two
                    // do not look at the same folder.
                    let here = tokio::time::timeout(
                        STEP_TIMEOUT,
                        tokio::fs::try_exists(file.path.clone()),
                    )
                    .await;
                    if matches!(here, Ok(Ok(true))) {
                        let n = self.missing.fetch_add(1, Ordering::Relaxed) + 1;
                        if n >= MISSING_LIMIT {
                            return self
                                .fail(format!(
                                    "{PATH_MAP_KEY} looks wrong: the host has no {host_path}, \
                                     and {n} files in a row were not found there"
                                ))
                                .await;
                        }
                    }
                }
                Err(ProbeError::Permanent(why)) => return self.fail(why).await,
                Err(ProbeError::Transient(why)) => {
                    let tick = self.transient(why).await;
                    return if measured > 0 && !matches!(tick, Tick::Failed(_)) {
                        Tick::Measured(measured)
                    } else {
                        tick
                    };
                }
            }
        }
        Tick::Measured(measured)
    }
}

impl skadi_core::module::Worker for PieceProbeWorker {
    fn name(&self) -> &str {
        "storage-piece-probe"
    }

    fn run(
        self: Box<Self>,
        cancel: CancellationToken,
    ) -> skadi_core::module::BoxFuture<'static, ()> {
        Box::pin(async move {
            let mut wait = self.start_delay;
            loop {
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = tokio::time::sleep(wait) => {}
                }
                wait = self.tick;
                match self.tick_once().await {
                    Tick::Measured(n) => tracing::debug!(files = n, "piece probe: measured"),
                    Tick::Off | Tick::Stopped | Tick::Idle | Tick::NotSetUp(_) => {}
                    Tick::Retrying(_) | Tick::Failed(_) => {}
                }
            }
        })
    }
}

/// What the page and the health check show of the probe. No secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeOverview {
    pub enabled: bool,
    /// What is not set, by config key; empty when the probe can run.
    pub missing: Vec<&'static str>,
    pub failed: bool,
    pub reason: Option<String>,
    pub failed_at: Option<DateTime<Utc>>,
    pub cleared_at: Option<DateTime<Utc>>,
    pub cleared_by: Option<String>,
    /// `password` or `key`, when a credential is stored.
    pub credential: Option<&'static str>,
    /// The pinned host key, when a connection has been made.
    pub host_key: Option<String>,
}

/// Read what the page and the health check show.
pub async fn overview(store: &Store) -> Result<ProbeOverview> {
    let settings = ProbeSettings::load(store).await;
    let credential = load_credential(store).await?;
    let status = store.storage_probe_status(PROBE).await?;
    Ok(ProbeOverview {
        enabled: settings.enabled,
        missing: settings.missing(credential.is_some()),
        failed: status
            .as_ref()
            .is_some_and(|s| s.state == ProbeState::Failed),
        reason: status.as_ref().and_then(|s| s.reason.clone()),
        failed_at: status.as_ref().and_then(|s| s.failed_at),
        cleared_at: status.as_ref().and_then(|s| s.cleared_at),
        cleared_by: status.and_then(|s| s.cleared_by),
        credential: credential.as_ref().map(ProbeCredential::kind),
        host_key: settings.pinned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_of_the_python_reader_and_of_filefrag_are_both_read() {
        assert_eq!(parse_answer(Some(0), "skadi-pieces 9059\n"), Ok(9059));
        assert_eq!(
            parse_answer(Some(0), "/volume1/media/a b.mkv: 28 extents found\n"),
            Ok(28)
        );
        assert_eq!(parse_answer(Some(0), "/v/a.mkv: 1 extent found\n"), Ok(1));
        // A login banner before the answer does not hide it.
        assert_eq!(
            parse_answer(Some(0), "Welcome to the NAS\nskadi-pieces 3\n"),
            Ok(3)
        );
    }

    #[test]
    fn each_way_the_host_cannot_answer_has_its_own_meaning() {
        assert_eq!(
            parse_answer(Some(44), "skadi-probe missing\n"),
            Err(ProbeError::Missing)
        );
        assert_eq!(
            parse_answer(Some(1), "open: No such file or directory\n"),
            Err(ProbeError::Missing)
        );
        for (out, says) in [
            ("skadi-probe no-tool\n", "neither filefrag nor python3"),
            ("skadi-probe denied Permission denied\n", "may not read"),
            (
                "skadi-probe unsupported Operation not supported\n",
                "does not give an extent map",
            ),
            ("sh: syntax error\n", "was not understood (exit 2)"),
        ] {
            match parse_answer(Some(2), out) {
                Err(ProbeError::Permanent(why)) => assert!(why.contains(says), "{why}"),
                other => panic!("{out:?} gave {other:?}"),
            }
        }
        // No exit status and no output: the connection broke, which may pass.
        assert!(matches!(
            parse_answer(None, ""),
            Err(ProbeError::Transient(_))
        ));
    }

    #[test]
    fn a_path_is_quoted_so_the_shell_reads_it_as_one_word() {
        assert_eq!(shell_quote("/v/a b.mkv"), "'/v/a b.mkv'");
        assert_eq!(shell_quote("/v/it's $(rm).mkv"), r"'/v/it'\''s $(rm).mkv'");
        assert!(
            !FIEMAP_PY.contains('\''),
            "the script goes in single quotes"
        );
    }

    /// The command is real shell: run it here. A file that is not there is
    /// "missing" on every machine; a file that is there gives a count, or a
    /// reason, but never "missing".
    #[cfg(unix)]
    #[test]
    fn the_command_runs_in_a_posix_shell() {
        let dir = skadi_core::unique_temp_path("probe-sh");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("it's a film (2020).mkv");
        std::fs::write(&file, vec![0u8; 4096]).unwrap();
        let run = |path: &Path| {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(remote_command(&path.to_string_lossy()))
                .output()
                .expect("sh");
            assert!(
                !String::from_utf8_lossy(&out.stderr).contains("yntax error"),
                "the shell could not read the command"
            );
            parse_answer(
                out.status.code().map(|c| c as u32),
                &String::from_utf8_lossy(&out.stdout),
            )
        };
        assert_eq!(run(&dir.join("not there.mkv")), Err(ProbeError::Missing));
        let there = run(&file);
        assert_ne!(there, Err(ProbeError::Missing), "{there:?}");
        if let Ok(n) = there {
            assert!(n >= 0);
        }
    }

    #[test]
    fn the_path_map_gives_the_path_on_the_host() {
        assert_eq!(
            parse_path_map(" /library = /volume1/media/ "),
            Some((PathBuf::from("/library"), "/volume1/media".into()))
        );
        assert_eq!(parse_path_map(""), None);
        assert_eq!(parse_path_map("/library"), None);
        assert_eq!(parse_path_map("library=/volume1"), None);
        let settings = ProbeSettings {
            enabled: true,
            host: "nas".into(),
            port: 22,
            user: "skadi".into(),
            path_map: parse_path_map("/library=/volume1/media"),
            per_hour: 120,
            remeasure_days: 30,
            pinned: None,
        };
        assert_eq!(
            settings.host_path(Path::new("/library/movie/A/a.mkv")),
            Some("/volume1/media/movie/A/a.mkv".into())
        );
        // `/library2` is not under `/library`.
        assert_eq!(settings.host_path(Path::new("/library2/a.mkv")), None);
        assert!(settings.missing(true).is_empty());
        assert_eq!(settings.missing(false), vec!["the credential"]);
    }

    #[test]
    fn ticks_add_up_to_the_hourly_rate() {
        assert_eq!(batch_size(120, Duration::from_secs(300)), 10);
        assert_eq!(batch_size(12, Duration::from_secs(300)), 1);
        assert_eq!(batch_size(1, Duration::from_secs(300)), 1, "never zero");
        assert_eq!(batch_size(0, Duration::from_secs(300)), 1);
    }

    #[test]
    fn a_credential_never_prints_its_secret() {
        let password = ProbeCredential::Password {
            secret: "hunter2-secret".into(),
        };
        let key = ProbeCredential::Key {
            secret: "-----BEGIN OPENSSH PRIVATE KEY-----".into(),
            passphrase: Some("open-sesame".into()),
        };
        for c in [&password, &key] {
            let shown = format!("{c:?}");
            assert!(!shown.contains("secret") || shown == "ProbeCredential(password)");
            assert!(
                !shown.contains("hunter2") && !shown.contains("BEGIN"),
                "{shown}"
            );
            assert!(!shown.contains("open-sesame"), "{shown}");
        }
        // The stored form reads back.
        let raw = serde_json::to_string(&key).unwrap();
        assert_eq!(serde_json::from_str::<ProbeCredential>(&raw).unwrap(), key);
        assert_eq!(key.kind(), "key");
    }
}
