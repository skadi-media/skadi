//! Credential storage.
//!
//! A small repository over the `credentials` table for secret material owned by
//! indexers, downloaders, metadata providers, etc. Secrets are transparently
//! sealed with the [`Cipher`](crate::crypto::Cipher) when `SKADI_SECRET_KEY` is
//! set (the row's `nonce` is non-NULL), and stored as plaintext otherwise.
//! Callers always work with plain `String` secrets — encryption is invisible.

use async_trait::async_trait;
use diesel::prelude::*;
use diesel_dualdb::types::Bytes;

use skadi_core::{AppError, Result};

use crate::schema::credentials as creds;
use crate::{Store, db_err};

/// Owner + secret as stored: `secret` bytes plus an optional `nonce`
/// (present ⇒ encrypted, absent ⇒ plaintext).
#[derive(Clone)]
struct StoredSecret {
    secret: Vec<u8>,
    nonce: Option<Vec<u8>>,
}

/// One credential exactly as stored, for backup and restore (SKADI-T-0463).
///
/// The bytes are the **sealed** form — never decrypted on the way out — so a
/// backup file is useless to anyone without `SKADI_SECRET_KEY`. `encrypted` is
/// false only when the daemon was running without a key, in which case these
/// bytes are the plaintext secret and the backup must be treated as sensitive.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SealedSecret {
    pub owner_kind: String,
    pub owner_id: String,
    pub secret: Vec<u8>,
    pub nonce: Option<Vec<u8>>,
}

impl SealedSecret {
    /// Whether this secret is ciphertext (a nonce is present) rather than a
    /// plaintext blob stored by a keyless daemon.
    #[must_use]
    pub fn encrypted(&self) -> bool {
        self.nonce.is_some()
    }
}

/// CRUD for owner-scoped secret material.
#[async_trait]
pub trait CredentialRepo {
    /// The decrypted secret for `(owner_kind, owner_id)`, if present.
    async fn get_secret(&self, owner_kind: &str, owner_id: &str) -> Result<Option<String>>;
    /// Insert or replace the secret (encrypting at rest when a key is configured).
    async fn set_secret(&self, owner_kind: &str, owner_id: &str, secret: &str) -> Result<()>;
    /// Remove the secret if present.
    async fn delete_secret(&self, owner_kind: &str, owner_id: &str) -> Result<()>;
}

impl Store {
    /// Seal a plaintext secret for storage using the configured cipher (or
    /// store as plaintext when none is configured).
    fn seal(&self, secret: &str) -> Result<StoredSecret> {
        match &self.cipher {
            Some(c) => {
                let (ciphertext, nonce) = c.encrypt(secret.as_bytes())?;
                Ok(StoredSecret {
                    secret: ciphertext,
                    nonce: Some(nonce),
                })
            }
            None => {
                // First time this process actually stores a secret without a key
                // (SKADI-T-0520).
                Self::warn_plaintext_credentials();
                Ok(StoredSecret {
                    secret: secret.as_bytes().to_vec(),
                    nonce: None,
                })
            }
        }
    }

    /// Recover a plaintext secret from stored bytes.
    fn unseal(&self, stored: StoredSecret) -> Result<String> {
        let bytes = match stored.nonce {
            Some(nonce) => {
                let cipher = self.cipher.as_ref().ok_or_else(|| {
                    AppError::Config(
                        "credential is encrypted but SKADI_SECRET_KEY is not set".into(),
                    )
                })?;
                cipher.decrypt(&stored.secret, &nonce)?
            }
            None => {
                // A plaintext-stored secret being read back: same warning, same
                // once-per-process (SKADI-T-0520).
                if self.cipher.is_none() {
                    Self::warn_plaintext_credentials();
                }
                stored.secret
            }
        };
        String::from_utf8(bytes)
            .map_err(|e| AppError::Internal(format!("credential is not valid UTF-8: {e}")))
    }
}

/// The stored row. `secret`/`nonce` use the portable `Bytes` type
/// (`BLOB`/`bytea`); the cipher operates on the inner `Vec<u8>` before storage.
#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = creds)]
struct Row {
    owner_kind: String,
    owner_id: String,
    secret: Bytes,
    nonce: Option<Bytes>,
}

impl Store {
    /// Every credential in its sealed form (SKADI-T-0463).
    ///
    /// Deliberately does not decrypt: a backup should be restorable without ever
    /// writing a provider password or API key to disk in the clear.
    pub async fn export_sealed_secrets(&self) -> Result<Vec<SealedSecret>> {
        self.with_conn(move |conn| {
            let rows: Vec<Row> = creds::table
                .select(Row::as_select())
                .load(conn)
                .map_err(db_err)?;
            Ok(rows
                .into_iter()
                .map(|r| SealedSecret {
                    owner_kind: r.owner_kind,
                    owner_id: r.owner_id,
                    secret: r.secret.0,
                    nonce: r.nonce.map(|b| b.0),
                })
                .collect())
        })
        .await
    }

    /// Write a sealed credential back verbatim (SKADI-T-0463).
    ///
    /// The bytes are stored as given, so a restore under a *different*
    /// `SKADI_SECRET_KEY` leaves rows that fail to decrypt on read. That is the
    /// honest outcome — the alternative is silently discarding them — and the
    /// provider loader already warns and skips an unreadable credential rather
    /// than aborting (SKADI-T-0519).
    pub async fn import_sealed_secret(&self, sealed: &SealedSecret) -> Result<()> {
        let row = Row {
            owner_kind: sealed.owner_kind.clone(),
            owner_id: sealed.owner_id.clone(),
            secret: Bytes(sealed.secret.clone()),
            nonce: sealed.nonce.clone().map(Bytes),
        };
        self.with_conn(move |conn| {
            // Same per-backend dispatch as `set_secret`: `on_conflict` is not
            // supported through MultiBackend.
            conn.dispatch(
                |pg| {
                    diesel::insert_into(creds::table)
                        .values(&row)
                        .on_conflict((creds::owner_kind, creds::owner_id))
                        .do_update()
                        .set((creds::secret.eq(&row.secret), creds::nonce.eq(&row.nonce)))
                        .execute(pg)
                },
                |sqlite| {
                    diesel::insert_into(creds::table)
                        .values(&row)
                        .on_conflict((creds::owner_kind, creds::owner_id))
                        .do_update()
                        .set((creds::secret.eq(&row.secret), creds::nonce.eq(&row.nonce)))
                        .execute(sqlite)
                },
            )
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }
}

#[async_trait]
impl CredentialRepo for Store {
    async fn get_secret(&self, owner_kind: &str, owner_id: &str) -> Result<Option<String>> {
        let (kind, id) = (owner_kind.to_string(), owner_id.to_string());
        let stored = self
            .with_conn(move |conn| {
                let row: Option<Row> = creds::table
                    .find((kind, id))
                    .select(Row::as_select())
                    .first(conn)
                    .optional()
                    .map_err(db_err)?;
                Ok(row.map(|r| StoredSecret {
                    secret: r.secret.0,
                    nonce: r.nonce.map(|b| b.0),
                }))
            })
            .await?;
        stored.map(|s| self.unseal(s)).transpose()
    }

    async fn set_secret(&self, owner_kind: &str, owner_id: &str, secret: &str) -> Result<()> {
        let sealed = self.seal(secret)?;
        let row = Row {
            owner_kind: owner_kind.to_string(),
            owner_id: owner_id.to_string(),
            secret: Bytes(sealed.secret),
            nonce: sealed.nonce.map(Bytes),
        };
        self.with_conn(move |conn| {
            // `on_conflict` isn't supported through MultiBackend — dispatch the
            // (identical) upsert per backend.
            conn.dispatch(
                |pg| {
                    diesel::insert_into(creds::table)
                        .values(&row)
                        .on_conflict((creds::owner_kind, creds::owner_id))
                        .do_update()
                        .set((creds::secret.eq(&row.secret), creds::nonce.eq(&row.nonce)))
                        .execute(pg)
                },
                |sqlite| {
                    diesel::insert_into(creds::table)
                        .values(&row)
                        .on_conflict((creds::owner_kind, creds::owner_id))
                        .do_update()
                        .set((creds::secret.eq(&row.secret), creds::nonce.eq(&row.nonce)))
                        .execute(sqlite)
                },
            )
            .map_err(db_err)?;
            Ok(())
        })
        .await
    }

    async fn delete_secret(&self, owner_kind: &str, owner_id: &str) -> Result<()> {
        let (kind, id) = (owner_kind.to_string(), owner_id.to_string());
        self.with_conn(move |conn| {
            diesel::delete(creds::table.find((kind, id)))
                .execute(conn)
                .map_err(db_err)?;
            Ok(())
        })
        .await
    }
}

/// What a rekey did (SKADI-T-0521).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RekeyReport {
    /// Rows re-sealed under the current key.
    pub rekeyed: usize,
    /// Rows already readable under the current key — left untouched.
    pub already_current: usize,
    /// Rows readable under **neither** key. Left exactly as they were.
    pub unreadable: usize,
}

impl Store {
    /// Re-seal every credential from `old` to the store's current key
    /// (SKADI-T-0521), and seal any plaintext rows.
    ///
    /// Rotating `SKADI_SECRET_KEY` used to strand every stored credential: the
    /// rows stay sealed under the key that is gone, the provider loader warns and
    /// skips each one, and the operator's only route back was re-entering every
    /// password by hand. Sealing plaintext rows matters for the same reason in
    /// reverse — a stack that ran without a key has its credentials in the clear,
    /// and configuring a key later did nothing to the rows already written.
    ///
    /// **A row that decrypts under neither key is left exactly as it is.** It may
    /// be sealed under a third key the operator still has; overwriting it with
    /// anything would destroy the only copy. It is counted and reported instead.
    ///
    /// Returns a [`RekeyReport`] rather than a bare count: "12 rekeyed, 0
    /// unreadable" and "0 rekeyed, 12 unreadable" are the same number of rows and
    /// completely different outcomes.
    pub async fn rekey_credentials(&self, old: Option<&str>) -> Result<RekeyReport> {
        let old_cipher = old.map(crate::crypto::Cipher::from_secret);
        let rows = self.export_sealed_secrets().await?;
        let mut report = RekeyReport::default();
        for row in rows {
            let stored = StoredSecret {
                secret: row.secret.clone(),
                nonce: row.nonce.clone(),
            };
            // Already readable as we stand? Then it is current — including a
            // plaintext row when no key is configured, which is not an error.
            if self.unseal(stored.clone()).is_ok() && row.nonce.is_some() == self.cipher.is_some() {
                report.already_current += 1;
                continue;
            }
            // Recover the plaintext: under the old key when the row is sealed,
            // or directly when it is not.
            let plaintext = match (&row.nonce, &old_cipher) {
                (Some(nonce), Some(c)) => c
                    .decrypt(&row.secret, nonce)
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok()),
                (None, _) => String::from_utf8(row.secret.clone()).ok(),
                (Some(_), None) => None,
            };
            let Some(plaintext) = plaintext else {
                tracing::warn!(
                    owner_kind = %row.owner_kind,
                    owner_id = %row.owner_id,
                    "credential is readable under neither the old nor the current key; left untouched"
                );
                report.unreadable += 1;
                continue;
            };
            // `set_secret` seals with the *current* cipher, so this is the rekey.
            self.set_secret(&row.owner_kind, &row.owner_id, &plaintext)
                .await?;
            report.rekeyed += 1;
        }
        tracing::info!(
            rekeyed = report.rekeyed,
            already_current = report.already_current,
            unreadable = report.unreadable,
            "credential rekey complete"
        );
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn temp_store() -> Store {
        let path = skadi_core::unique_temp_path("credtest").with_extension("db");
        let store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();
        store
    }

    // Runs with SKADI_SECRET_KEY unset (plaintext path) to avoid mutating global
    // env in parallel tests; the encrypted path is covered by crypto unit tests.
    #[tokio::test]
    async fn credential_round_trip_plaintext() {
        let store = temp_store().await;
        assert_eq!(store.get_secret("indexer", "x").await.unwrap(), None);

        store
            .set_secret("indexer", "x", "my-api-key")
            .await
            .unwrap();
        assert_eq!(
            store.get_secret("indexer", "x").await.unwrap().as_deref(),
            Some("my-api-key")
        );

        // upsert overwrites
        store.set_secret("indexer", "x", "rotated").await.unwrap();
        assert_eq!(
            store.get_secret("indexer", "x").await.unwrap().as_deref(),
            Some("rotated")
        );

        store.delete_secret("indexer", "x").await.unwrap();
        assert_eq!(store.get_secret("indexer", "x").await.unwrap(), None);
    }
}

#[cfg(test)]
mod rekey_tests {
    use super::*;
    use crate::crypto::Cipher;

    async fn store_with(cipher: Option<Cipher>) -> Store {
        let path = skadi_core::unique_temp_path("rekey").with_extension("db");
        let mut store = Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.cipher = cipher;
        store.run_migrations().await.unwrap();
        store
    }

    /// SKADI-T-0521: rotating `SKADI_SECRET_KEY` used to strand every credential —
    /// the rows stay sealed under a key that is gone, the provider loader warns
    /// and skips each one, and the only way back was re-entering every password
    /// by hand.
    #[tokio::test]
    async fn rekey_moves_rows_from_the_old_key_to_the_current_one() {
        // Written under the OLD key…
        let old = store_with(Some(Cipher::from_secret("old-key"))).await;
        old.set_secret("indexers", "a", "hunter2").await.unwrap();
        let sealed = old.export_sealed_secrets().await.unwrap();
        assert_eq!(sealed.len(), 1);
        assert!(sealed[0].nonce.is_some(), "sealed, not plaintext");

        // …then the daemon restarts holding the NEW key.
        let new = store_with(Some(Cipher::from_secret("new-key"))).await;
        for s in &sealed {
            new.import_sealed_secret(s).await.unwrap();
        }
        // Unreadable until rekeyed — this is the stranding the ticket describes.
        assert!(new.get_secret("indexers", "a").await.is_err());

        let report = new.rekey_credentials(Some("old-key")).await.unwrap();
        assert_eq!(report.rekeyed, 1);
        assert_eq!(report.unreadable, 0);
        assert_eq!(
            new.get_secret("indexers", "a").await.unwrap().as_deref(),
            Some("hunter2")
        );
    }

    /// SKADI-T-0521: a stack that ran without a key has its credentials in the
    /// clear, and configuring one later did nothing to the rows already written.
    #[tokio::test]
    async fn rekey_seals_plaintext_rows_once_a_key_is_configured() {
        let open = store_with(None).await;
        open.set_secret("downloaders", "d", "pw").await.unwrap();
        let rows = open.export_sealed_secrets().await.unwrap();
        assert!(rows[0].nonce.is_none(), "stored in the clear");

        let keyed = store_with(Some(Cipher::from_secret("k"))).await;
        for r in &rows {
            keyed.import_sealed_secret(r).await.unwrap();
        }
        let report = keyed.rekey_credentials(None).await.unwrap();
        assert_eq!(report.rekeyed, 1);
        assert!(
            keyed.export_sealed_secrets().await.unwrap()[0]
                .nonce
                .is_some(),
            "now sealed"
        );
        assert_eq!(
            keyed
                .get_secret("downloaders", "d")
                .await
                .unwrap()
                .as_deref(),
            Some("pw")
        );
    }

    /// A row readable under neither key is **left exactly as it is**: it may be
    /// sealed under a third key the operator still has, and overwriting it would
    /// destroy the only copy.
    #[tokio::test]
    async fn a_row_readable_under_neither_key_is_left_untouched() {
        let third = store_with(Some(Cipher::from_secret("third-key"))).await;
        third.set_secret("indexers", "x", "secret").await.unwrap();
        let sealed = third.export_sealed_secrets().await.unwrap();

        let new = store_with(Some(Cipher::from_secret("new-key"))).await;
        for s in &sealed {
            new.import_sealed_secret(s).await.unwrap();
        }
        let report = new.rekey_credentials(Some("wrong-old-key")).await.unwrap();
        assert_eq!(report.unreadable, 1);
        assert_eq!(report.rekeyed, 0);
        // Byte-identical to what came in — recoverable by whoever still has the key.
        assert_eq!(new.export_sealed_secrets().await.unwrap(), sealed);
    }
}
