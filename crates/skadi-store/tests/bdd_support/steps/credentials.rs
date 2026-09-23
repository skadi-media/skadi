//! C04 credential-vault steps: encryption at rest under `SKADI_SECRET_KEY`,
//! the plaintext fallback, key rotation and what the stored bytes look like.
use cucumber::{given, then, when};
use diesel::prelude::*;

use skadi_store::CredentialRepo;

use crate::bdd_support::{Db, World};

/// The raw `credentials` row as stored — bypassing the repo's seal/unseal.
#[derive(QueryableByName)]
struct RawRow {
    #[diesel(sql_type = diesel::sql_types::Binary)]
    secret: Vec<u8>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Binary>)]
    nonce: Option<Vec<u8>>,
}

async fn raw_row(w: &World, kind: &str, id: &str) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    let (kind, id) = (kind.to_string(), id.to_string());
    w.store()
        .with_conn(move |conn| {
            let q = "SELECT secret, nonce FROM credentials WHERE owner_kind = $1 AND owner_id = $2";
            let q_sq =
                "SELECT secret, nonce FROM credentials WHERE owner_kind = ? AND owner_id = ?";
            let rows: Vec<RawRow> = conn
                .dispatch(
                    |pg| {
                        diesel::sql_query(q)
                            .bind::<diesel::sql_types::Text, _>(&kind)
                            .bind::<diesel::sql_types::Text, _>(&id)
                            .load(pg)
                    },
                    |sq| {
                        diesel::sql_query(q_sq)
                            .bind::<diesel::sql_types::Text, _>(&kind)
                            .bind::<diesel::sql_types::Text, _>(&id)
                            .load(sq)
                    },
                )
                .map_err(|e| skadi_core::AppError::Internal(e.to_string()))?;
            Ok(rows.into_iter().next().map(|r| (r.secret, r.nonce)))
        })
        .await
        .expect("raw row")
}

// ---- key handling (@serial) ----------------------------------------------------------

#[given(expr = "the master key {string} is configured")]
async fn key_set(w: &mut World, key: String) {
    w.env_touched.push((
        "SKADI_SECRET_KEY".into(),
        std::env::var("SKADI_SECRET_KEY").ok(),
    ));
    // SAFETY: `@serial` scenario — no other scenario opens a store concurrently.
    unsafe { std::env::set_var("SKADI_SECRET_KEY", &key) };
}

#[given("no master key is configured")]
async fn key_unset(w: &mut World) {
    w.env_touched.push((
        "SKADI_SECRET_KEY".into(),
        std::env::var("SKADI_SECRET_KEY").ok(),
    ));
    // SAFETY: see `key_set`.
    unsafe { std::env::remove_var("SKADI_SECRET_KEY") };
}

/// Open a second store against the same database under whatever key is now in
/// the environment (a restart with a different `SKADI_SECRET_KEY`).
#[when(expr = "the daemon restarts with the master key {string}")]
async fn restart_with_key(w: &mut World, key: String) {
    key_set(w, key).await;
    let url = w.db.as_ref().expect("db").url.clone();
    w.other = Some(Db {
        store: skadi_store::Store::connect(&url).expect("reconnect"),
        url,
        backend: w.db.as_ref().unwrap().backend,
        sqlite_path: None,
        pg: None,
    });
    w.restore_env();
}

#[when("the daemon restarts with no master key")]
async fn restart_without_key(w: &mut World) {
    key_unset(w).await;
    let url = w.db.as_ref().expect("db").url.clone();
    w.other = Some(Db {
        store: skadi_store::Store::connect(&url).expect("reconnect"),
        url,
        backend: w.db.as_ref().unwrap().backend,
        sqlite_path: None,
        pg: None,
    });
    w.restore_env();
}

// ---- repo operations ----------------------------------------------------------------

#[when(expr = "the {word} {string} secret {string} is stored")]
#[given(expr = "the {word} {string} secret {string} is stored")]
async fn set_secret(w: &mut World, kind: String, id: String, secret: String) {
    w.store()
        .set_secret(&kind, &id, &secret)
        .await
        .expect("set_secret");
    // The env only matters at `Store::connect`; release it for later scenarios.
    w.restore_env();
}

#[when(expr = "the {word} {string} secret is read")]
async fn get_secret(w: &mut World, kind: String, id: String) {
    match w.store().get_secret(&kind, &id).await {
        Ok(s) => {
            w.secret = Some(s);
            w.last_err = None;
        }
        Err(e) => {
            w.secret = None;
            w.last_err = Some(e.to_string());
        }
    }
}

#[when(expr = "the restarted daemon reads the {word} {string} secret")]
async fn other_get_secret(w: &mut World, kind: String, id: String) {
    let store = w.other.as_ref().expect("restarted store").store.clone();
    match store.get_secret(&kind, &id).await {
        Ok(s) => {
            w.secret = Some(s);
            w.last_err = None;
        }
        Err(e) => {
            w.secret = None;
            w.last_err = Some(e.to_string());
        }
    }
}

#[when(expr = "the {word} {string} secret is deleted")]
async fn delete_secret(w: &mut World, kind: String, id: String) {
    w.store()
        .delete_secret(&kind, &id)
        .await
        .expect("delete_secret");
}

#[then(expr = "the secret reads back as {string}")]
async fn reads_back(w: &mut World, want: String) {
    assert_eq!(w.last_err, None, "read failed");
    assert_eq!(w.secret.clone().flatten().as_deref(), Some(want.as_str()));
}

#[then("the secret is absent")]
async fn absent(w: &mut World) {
    assert_eq!(w.last_err, None, "read failed");
    assert_eq!(w.secret, Some(None));
}

#[then(expr = "the read fails mentioning {string}")]
async fn read_fails(w: &mut World, needle: String) {
    let e = w.last_err.clone().expect("expected the read to fail");
    assert!(e.contains(&needle), "{e:?} lacks {needle:?}");
}

// ---- at-rest inspection -----------------------------------------------------------------

#[then(expr = "the {word} {string} row stores the plaintext bytes of {string} with no nonce")]
async fn stored_plaintext(w: &mut World, kind: String, id: String, plain: String) {
    let (secret, nonce) = raw_row(w, &kind, &id).await.expect("row");
    assert_eq!(secret, plain.as_bytes(), "secret column is the plaintext");
    assert_eq!(nonce, None, "no nonce ⇒ plaintext marker");
}

#[then(
    expr = "the {word} {string} row stores ciphertext that is not {string} with a 12-byte nonce"
)]
async fn stored_ciphertext(w: &mut World, kind: String, id: String, plain: String) {
    let (secret, nonce) = raw_row(w, &kind, &id).await.expect("row");
    assert_ne!(
        secret,
        plain.as_bytes(),
        "secret column must not be the plaintext"
    );
    assert!(
        !secret
            .windows(plain.len())
            .any(|win| win == plain.as_bytes()),
        "plaintext must not appear inside the stored bytes"
    );
    let nonce = nonce.expect("encrypted rows carry a nonce");
    assert_eq!(nonce.len(), 12, "ChaCha20-Poly1305 nonce");
    assert_eq!(
        secret.len(),
        plain.len() + 16,
        "ciphertext = plaintext + 16-byte AEAD tag"
    );
}

#[then(expr = "the two encryptions of {string} for {word} {string} and {string} differ")]
async fn nonces_differ(w: &mut World, _plain: String, kind: String, a: String, b: String) {
    let (sa, na) = raw_row(w, &kind, &a).await.expect("row a");
    let (sb, nb) = raw_row(w, &kind, &b).await.expect("row b");
    assert_ne!(na, nb, "fresh random nonce per encryption");
    assert_ne!(sa, sb, "same plaintext never yields the same ciphertext");
}

// ---- parity gaps ------------------------------------------------------------------------

/// Sonarr/Radarr always encrypt provider secrets; skadi leaves rows written
/// before a key existed as plaintext forever (only a re-save re-seals them).
#[then(expr = "the {word} {string} row has been sealed now that a key exists")]
async fn plaintext_upgraded(w: &mut World, kind: String, id: String) {
    let (_, nonce) = raw_row(w, &kind, &id).await.expect("row");
    assert!(
        nonce.is_some(),
        "{kind}/{id} is still stored as plaintext after a key was configured — \
         no plaintext→encrypted migration exists (crates/skadi-store/src/credentials.rs:52-68)"
    );
}

/// Rotating `SKADI_SECRET_KEY` needs a re-encrypt pass; none exists, so every
/// stored credential becomes unreadable and the provider build aborts.
#[then("stored credentials are re-encrypted under the new master key")]
async fn rotation(w: &mut World) {
    let e = w.last_err.clone().unwrap_or_default();
    panic!(
        "no key-rotation tool: after the key changed the read failed with {e:?}; \
         Store has no `rekey(old, new)` (crates/skadi-store/src/crypto.rs) and every \
         `get_secret` on an old row errors"
    );
}
