//! Credential encryption at rest.
//!
//! When `SKADI_SECRET_KEY` is set, secret columns are sealed with
//! ChaCha20-Poly1305 (AEAD) using a key derived from the secret; the random
//! nonce is stored alongside the ciphertext. When unset, secrets are stored as
//! plaintext (with a startup WARN) so first-run users don't need to generate a
//! key — matching the vision's bootstrap behavior.

use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use sha2::{Digest, Sha256};

use skadi_core::{AppError, Result};

/// The env var that, when present, enables encryption of credential rows.
pub const SECRET_KEY_ENV: &str = "SKADI_SECRET_KEY";

/// An AEAD cipher derived from `SKADI_SECRET_KEY`.
#[derive(Clone)]
pub struct Cipher {
    // 32-byte key; intentionally no `Debug`/`Display` to avoid leaking it.
    key: [u8; 32],
}

impl Cipher {
    /// Derive a cipher from a secret string (SHA-256 → 32-byte key).
    #[must_use]
    pub fn from_secret(secret: &str) -> Self {
        let digest = Sha256::digest(secret.as_bytes());
        let mut key = [0u8; 32];
        key.copy_from_slice(&digest);
        Self { key }
    }

    /// Build a cipher from `SKADI_SECRET_KEY` if it is set and non-empty.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        match std::env::var(SECRET_KEY_ENV) {
            Ok(secret) if !secret.is_empty() => Some(Self::from_secret(&secret)),
            _ => None,
        }
    }

    fn aead(&self) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new_from_slice(&self.key).expect("32-byte key is always valid")
    }

    /// Encrypt `plaintext`, returning `(ciphertext, nonce)`.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ciphertext = self
            .aead()
            .encrypt(&nonce, plaintext)
            .map_err(|e| AppError::Internal(format!("credential encryption failed: {e}")))?;
        Ok((ciphertext, nonce.to_vec()))
    }

    /// Decrypt `ciphertext` using the stored `nonce`.
    pub fn decrypt(&self, ciphertext: &[u8], nonce: &[u8]) -> Result<Vec<u8>> {
        if nonce.len() != 12 {
            return Err(AppError::Internal(format!(
                "credential nonce must be 12 bytes, got {}",
                nonce.len()
            )));
        }
        let nonce = Nonce::from_slice(nonce);
        self.aead()
            .decrypt(nonce, ciphertext)
            .map_err(|_| AppError::Internal("credential decryption failed (wrong key?)".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let cipher = Cipher::from_secret("correct horse battery staple");
        let (ct, nonce) = cipher.encrypt(b"my-api-key").unwrap();
        assert_ne!(ct, b"my-api-key", "ciphertext is not plaintext");
        assert_eq!(nonce.len(), 12);
        let pt = cipher.decrypt(&ct, &nonce).unwrap();
        assert_eq!(pt, b"my-api-key");
    }

    #[test]
    fn wrong_key_fails_to_decrypt() {
        let a = Cipher::from_secret("key-a");
        let b = Cipher::from_secret("key-b");
        let (ct, nonce) = a.encrypt(b"secret").unwrap();
        assert!(b.decrypt(&ct, &nonce).is_err());
    }

    #[test]
    fn nonces_differ_per_encryption() {
        let cipher = Cipher::from_secret("k");
        let (_, n1) = cipher.encrypt(b"x").unwrap();
        let (_, n2) = cipher.encrypt(b"x").unwrap();
        assert_ne!(n1, n2, "each encryption uses a fresh random nonce");
    }
}
