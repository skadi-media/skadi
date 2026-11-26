//! The piece probe against a test SSH host in this process (SKADI-T-0775). No
//! test here needs a real storage host.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, Msg, Server as _, Session, VerifiedPublicKey};
use russh::{Channel, ChannelId};
use skadi_library_scan::probe::{
    self, PieceProbeWorker, ProbeCredential, TRANSIENT_LIMIT, Tick, save_credential,
};
use skadi_store::{
    ConfigRepo, ConfigSource, LibraryFileRepo, LibraryFileStat, ProbeState, StorageProbeRepo, Store,
};
use tokio::net::TcpListener;

const PASSWORD: &str = "correct-horse-battery";

/// What the host answers to one command: its output and exit status.
type Answer = Arc<dyn Fn(&str) -> (String, u32) + Send + Sync>;

#[derive(Clone)]
struct Host {
    password: String,
    /// The public key that may log in, in OpenSSH form.
    key: Option<String>,
    answer: Answer,
    connections: Arc<AtomicUsize>,
    commands: Arc<Mutex<Vec<String>>>,
}

impl russh::server::Server for Host {
    type Handler = Self;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
        self.connections.fetch_add(1, Ordering::SeqCst);
        self.clone()
    }
}

impl russh::server::Handler for Host {
    type Error = russh::Error;

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if password == self.password {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        key: VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        let shown = key.public_key.to_openssh().unwrap_or_default();
        Ok(if self.key.as_deref() == Some(shown.as_str()) {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).into_owned();
        let (out, code) = (self.answer)(&command);
        self.commands.lock().unwrap().push(command);
        session.channel_success(channel)?;
        session.data(channel, out.into_bytes())?;
        session.exit_status_request(channel, code)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
}

/// A running test host and the store of a daemon set up to probe it.
struct Rig {
    store: Store,
    port: u16,
    connections: Arc<AtomicUsize>,
    commands: Arc<Mutex<Vec<String>>>,
    _dir: std::path::PathBuf,
}

fn pieces(n: i64) -> Answer {
    Arc::new(move |_| (format!("skadi-pieces {n}\n"), 0))
}

async fn start_host(host: Host) -> u16 {
    let config = Arc::new(russh::server::Config {
        auth_rejection_time: Duration::from_millis(10),
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()],
        ..Default::default()
    });
    let socket = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = socket.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut host = host;
        let _ = host.run_on_socket(config, &socket).await;
    });
    port
}

impl Rig {
    /// A host that takes [`PASSWORD`] (and `key` when given), a store with
    /// the probe on and set up for it, and `files` library files that wait
    /// for a count.
    async fn new(answer: Answer, key: Option<String>, files: usize) -> Self {
        let connections = Arc::new(AtomicUsize::new(0));
        let commands = Arc::new(Mutex::new(Vec::new()));
        let port = start_host(Host {
            password: PASSWORD.into(),
            key,
            answer,
            connections: Arc::clone(&connections),
            commands: Arc::clone(&commands),
        })
        .await;

        let dir = skadi_core::unique_temp_path("piece-probe");
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        let store = Store::connect(&format!("sqlite://{}", dir.join("t.db").display())).unwrap();
        store.run_migrations().await.unwrap();
        let lib = dir.join("lib");
        for (key, value) in [
            (probe::ENABLED_KEY, "true".to_string()),
            (probe::HOST_KEY, "127.0.0.1".to_string()),
            (probe::PORT_KEY, port.to_string()),
            (probe::USER_KEY, "skadi".to_string()),
            (
                probe::PATH_MAP_KEY,
                format!("{}=/volume1/media", lib.display()),
            ),
        ] {
            store
                .set_config(key, &value, ConfigSource::Runtime)
                .await
                .unwrap();
        }
        save_credential(
            &store,
            &ProbeCredential::Password {
                secret: PASSWORD.into(),
            },
        )
        .await
        .unwrap();
        for i in 0..files {
            let path = lib.join(format!("film {i:02}.mkv"));
            std::fs::write(&path, b"x").unwrap();
            let stat = LibraryFileStat {
                kind: "movie".into(),
                acquirable_ref: format!("ed-{i}"),
                path: path.to_string_lossy().into_owned(),
                dev: 1,
                ino: i as i64 + 1,
                size_bytes: 1,
                disk_bytes: 1,
                links: 1,
                mtime: Utc::now(),
            };
            // Each a second apart, so the queue has one order.
            let at = Utc::now() + chrono::Duration::seconds(i as i64);
            store.record_library_file_stat(&stat, at).await.unwrap();
        }
        Rig {
            store,
            port,
            connections,
            commands,
            _dir: dir,
        }
    }

    fn worker(&self) -> PieceProbeWorker {
        PieceProbeWorker::new(self.store.clone())
    }

    async fn set(&self, key: &str, value: &str) {
        self.store
            .set_config(key, value, ConfigSource::Runtime)
            .await
            .unwrap();
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    async fn measured(&self) -> Vec<i64> {
        let rows = self.store.library_files_of_kind("movie").await.unwrap();
        rows.into_iter()
            .filter_map(|f| f.pieces_at.and(f.pieces))
            .collect()
    }

    async fn state(&self) -> Option<skadi_store::StorageProbeStatus> {
        self.store.storage_probe_status(probe::PROBE).await.unwrap()
    }
}

#[tokio::test]
async fn a_count_is_read_over_ssh_and_stored_and_the_host_key_is_pinned() {
    let rig = Rig::new(pieces(4242), None, 2).await;
    assert_eq!(rig.worker().tick_once().await, Tick::Measured(2));
    assert_eq!(rig.measured().await, vec![4242, 4242]);
    assert_eq!(rig.connections(), 1, "one connection for the batch");

    // The command names the file by its path on the host, in quotes.
    let commands = rig.commands.lock().unwrap().clone();
    assert_eq!(commands.len(), 2);
    assert!(
        commands[0].contains("'/volume1/media/film 00.mkv'"),
        "{}",
        commands[0]
    );

    // The key the host showed is kept, and the next connection checks it.
    let pinned = rig.store.get_config(probe::PINNED_KEY).await.unwrap();
    assert!(pinned.is_some_and(|k| k.value.starts_with("ssh-ed25519 ")));
    // Nothing waits now: no connection is opened for nothing.
    assert_eq!(rig.worker().tick_once().await, Tick::Idle);
    assert_eq!(rig.connections(), 1);
}

#[tokio::test]
async fn a_private_key_logs_in() {
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let public = key.public_key().to_openssh().unwrap();
    let rig = Rig::new(pieces(7), Some(public), 1).await;
    save_credential(
        &rig.store,
        &ProbeCredential::Key {
            secret: key.to_openssh(LineEnding::LF).unwrap().to_string(),
            passphrase: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(rig.worker().tick_once().await, Tick::Measured(1));
    assert_eq!(rig.measured().await, vec![7]);
}

#[tokio::test]
async fn while_the_flag_is_off_no_connection_is_opened() {
    let rig = Rig::new(pieces(1), None, 3).await;
    rig.set(probe::ENABLED_KEY, "false").await;
    assert_eq!(rig.worker().tick_once().await, Tick::Off);
    assert_eq!(rig.connections(), 0);
    assert!(rig.measured().await.is_empty());
}

#[tokio::test]
async fn a_probe_that_is_not_set_up_says_what_is_missing_and_connects_to_nothing() {
    let rig = Rig::new(pieces(1), None, 1).await;
    rig.set(probe::PATH_MAP_KEY, "").await;
    probe::delete_credential(&rig.store).await.unwrap();
    assert_eq!(
        rig.worker().tick_once().await,
        Tick::NotSetUp(vec![probe::PATH_MAP_KEY, "the credential"])
    );
    assert_eq!(rig.connections(), 0);
    assert_eq!(rig.state().await, None, "not set up is not a failure");
}

#[tokio::test]
async fn a_refused_login_stops_the_probe_until_an_admin_clears_it() {
    let rig = Rig::new(pieces(9), None, 2).await;
    let wrong = "not-the-password-xyzzy";
    save_credential(
        &rig.store,
        &ProbeCredential::Password {
            secret: wrong.into(),
        },
    )
    .await
    .unwrap();
    let worker = rig.worker();
    let Tick::Failed(reason) = worker.tick_once().await else {
        panic!("a refused login must fail the probe");
    };
    assert!(
        reason.contains("refused the password of user skadi"),
        "{reason}"
    );
    assert!(
        !reason.contains(wrong),
        "the reason must not carry the secret"
    );
    let state = rig.state().await.expect("the failure is recorded");
    assert_eq!(state.state, ProbeState::Failed);
    assert_eq!(state.reason.as_deref(), Some(reason.as_str()));

    // It does not try again by itself, whatever changes.
    let before = rig.connections();
    save_credential(
        &rig.store,
        &ProbeCredential::Password {
            secret: PASSWORD.into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(worker.tick_once().await, Tick::Stopped);
    assert_eq!(worker.tick_once().await, Tick::Stopped);
    assert_eq!(
        rig.connections(),
        before,
        "a stopped probe opens no connection"
    );

    // Cleared: it runs again, and the clear is on record.
    assert!(
        rig.store
            .clear_storage_probe(probe::PROBE, "robin", Utc::now())
            .await
            .unwrap()
    );
    assert_eq!(worker.tick_once().await, Tick::Measured(2));
    let state = rig.state().await.unwrap();
    assert_eq!(state.state, ProbeState::On);
    assert_eq!(state.cleared_by.as_deref(), Some("robin"));
    assert!(state.cleared_at.is_some());
}

#[tokio::test]
async fn a_host_that_shows_another_key_is_refused_and_the_probe_stops() {
    let rig = Rig::new(pieces(9), None, 1).await;
    // A key pinned at an earlier connection, which this host does not hold.
    let other = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    rig.set(probe::PINNED_KEY, &other.public_key().to_openssh().unwrap())
        .await;
    let Tick::Failed(reason) = rig.worker().tick_once().await else {
        panic!("a changed host key must fail the probe");
    };
    assert!(reason.contains("not the one that was pinned"), "{reason}");
    assert!(rig.commands.lock().unwrap().is_empty(), "nothing was run");
    assert!(rig.measured().await.is_empty());
    assert_eq!(rig.state().await.unwrap().state, ProbeState::Failed);
}

#[tokio::test]
async fn a_host_with_no_tool_stops_the_probe_with_the_reason() {
    let rig = Rig::new(Arc::new(|_| ("skadi-probe no-tool\n".into(), 47)), None, 2).await;
    let Tick::Failed(reason) = rig.worker().tick_once().await else {
        panic!("a host with no tool must fail the probe");
    };
    assert!(reason.contains("neither filefrag nor python3"), "{reason}");
    assert_eq!(
        rig.commands.lock().unwrap().len(),
        1,
        "it stops at the first file"
    );
    assert_eq!(rig.worker().tick_once().await, Tick::Stopped);
}

#[tokio::test]
async fn a_filesystem_with_no_extent_map_stops_the_probe() {
    let rig = Rig::new(
        Arc::new(|_| {
            (
                "skadi-probe unsupported Operation not supported\n".into(),
                48,
            )
        }),
        None,
        1,
    )
    .await;
    let Tick::Failed(reason) = rig.worker().tick_once().await else {
        panic!("an unsupported filesystem must fail the probe");
    };
    assert!(reason.contains("does not give an extent map"), "{reason}");
}

#[tokio::test]
async fn files_the_mount_has_and_the_host_has_not_mean_a_wrong_path_map() {
    let rig = Rig::new(Arc::new(|_| ("skadi-probe missing\n".into(), 44)), None, 5).await;
    let Tick::Failed(reason) = rig.worker().tick_once().await else {
        panic!("a wrong path map must fail the probe");
    };
    assert!(reason.contains(probe::PATH_MAP_KEY), "{reason}");
    assert_eq!(
        rig.commands.lock().unwrap().len(),
        probe::MISSING_LIMIT as usize,
        "it stops at the limit, not at the end of the batch"
    );
}

#[tokio::test]
async fn a_file_outside_the_path_map_stops_the_probe() {
    let rig = Rig::new(pieces(1), None, 1).await;
    rig.set(probe::PATH_MAP_KEY, "/somewhere/else=/volume1/media")
        .await;
    let Tick::Failed(reason) = rig.worker().tick_once().await else {
        panic!("a path map that does not cover the library must fail the probe");
    };
    assert!(reason.contains("does not cover"), "{reason}");
}

#[tokio::test]
async fn one_tick_measures_no_more_than_its_share_of_the_hourly_rate() {
    let rig = Rig::new(pieces(3), None, 30).await;
    // 120 an hour in ticks of 5 minutes: 10 a tick.
    rig.set(probe::PER_HOUR_KEY, "120").await;
    assert_eq!(rig.worker().tick_once().await, Tick::Measured(10));
    assert_eq!(rig.measured().await.len(), 10);
    assert_eq!(rig.commands.lock().unwrap().len(), 10);
    // 12 an hour: 1 a tick.
    rig.set(probe::PER_HOUR_KEY, "12").await;
    assert_eq!(rig.worker().tick_once().await, Tick::Measured(1));
    assert_eq!(rig.measured().await.len(), 11);
}

#[tokio::test]
async fn a_host_that_does_not_answer_is_tried_again_and_then_recorded() {
    let rig = Rig::new(pieces(1), None, 1).await;
    // A port nothing listens on.
    let closed = {
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        l.local_addr().unwrap().port()
    };
    assert_ne!(closed, rig.port);
    rig.set(probe::PORT_KEY, &closed.to_string()).await;
    let worker = rig.worker();
    for attempt in 1..TRANSIENT_LIMIT {
        assert!(
            matches!(worker.tick_once().await, Tick::Retrying(_)),
            "attempt {attempt} is tried again"
        );
        assert_eq!(rig.state().await, None, "no failure is recorded yet");
    }
    let Tick::Failed(reason) = worker.tick_once().await else {
        panic!("the last attempt in a row is a recorded failure");
    };
    assert!(reason.contains("did not answer"), "{reason}");
    assert_eq!(rig.state().await.unwrap().state, ProbeState::Failed);
}

#[tokio::test]
async fn a_measured_file_is_measured_again_only_after_the_set_age() {
    let rig = Rig::new(pieces(5), None, 1).await;
    assert_eq!(rig.worker().tick_once().await, Tick::Measured(1));
    // Measured a moment ago: left alone.
    assert_eq!(rig.worker().tick_once().await, Tick::Idle);
    // Measured 40 days ago, and the age is 30: measured again.
    let file = rig
        .store
        .library_files_of_kind("movie")
        .await
        .unwrap()
        .remove(0);
    let long_ago = Utc::now() - chrono::Duration::days(40);
    rig.store
        .record_library_file_pieces(&file.id, 5, long_ago)
        .await
        .unwrap();
    assert_eq!(rig.worker().tick_once().await, Tick::Measured(1));
    // With the age at 0, never.
    rig.store
        .record_library_file_pieces(&file.id, 5, long_ago)
        .await
        .unwrap();
    rig.set(probe::REMEASURE_DAYS_KEY, "0").await;
    assert_eq!(rig.worker().tick_once().await, Tick::Idle);
}
