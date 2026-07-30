//! At-rest encryption for the fields Go marks `encrypt:"true"`.
//!
//! Two things are encrypted: OAuth tokens, which are bearer credentials for a
//! third-party account, and fetched update payloads, which are the user's
//! personal data. Everything else in this database is either the user's own
//! configuration or a provider definition.
//!
//! The primitive matches Go (XChaCha20-Poly1305 under `ENCRYPTION_KEY`) but the
//! encoding does not: Go gob-encodes the value, this serialises it as a BSON
//! document. Nothing reads both, because only one implementation of this
//! service runs at a time and there is no existing database to stay compatible
//! with.

use anyhow::{Context, anyhow};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use mongodb::bson::spec::BinarySubtype;
use mongodb::bson::{Binary, Bson, Document, doc};
use std::io::Cursor;
use std::sync::Arc;

const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;

/// The value is wrapped in a single-field document so that BSON, which has no
/// top-level representation for a bare scalar, can carry any `Bson`.
const FIELD: &str = "v";

#[derive(Clone)]
pub struct Cipher(Arc<XChaCha20Poly1305>);

impl Cipher {
    /// Reads `ENCRYPTION_KEY` from the environment.
    ///
    /// # Panics
    /// Panics when the key is absent or not exactly 32 bytes. Starting without
    /// it would mean writing tokens and personal data in the clear.
    pub fn from_env() -> Self {
        let raw = perfice_common::config::require("ENCRYPTION_KEY");
        let bytes = raw.as_bytes();
        assert!(
            bytes.len() == KEY_LEN,
            "ENCRYPTION_KEY must be exactly {KEY_LEN} bytes, got {}",
            bytes.len()
        );

        Self::with_key(bytes)
    }

    fn with_key(bytes: &[u8]) -> Self {
        let key = Key::try_from(bytes).expect("key length was already checked");
        Self(Arc::new(XChaCha20Poly1305::new(&key)))
    }

    pub fn encrypt(&self, value: impl Into<Bson>) -> anyhow::Result<Binary> {
        let wrapped = doc! { FIELD: value.into() };
        let plaintext = mongodb::bson::to_vec(&wrapped).context("failed to encode value")?;

        // A fresh nonce per write. Reuse under a fixed key would leak the
        // relationship between two payloads.
        let mut out = perfice_common::random::bytes(NONCE_LEN);
        let nonce = XNonce::try_from(out.as_slice()).context("failed to generate a nonce")?;
        let sealed = self
            .0
            .encrypt(&nonce, plaintext.as_slice())
            .map_err(|_| anyhow!("encryption failed"))?;
        out.extend_from_slice(&sealed);

        Ok(Binary {
            subtype: BinarySubtype::Generic,
            bytes: out,
        })
    }

    pub fn decrypt(&self, binary: &Binary) -> anyhow::Result<Bson> {
        if binary.bytes.len() < NONCE_LEN {
            return Err(anyhow!("ciphertext is too short to contain a nonce"));
        }

        let (nonce, ciphertext) = binary.bytes.split_at(NONCE_LEN);
        let nonce = XNonce::try_from(nonce).context("malformed nonce")?;
        let plaintext = self
            .0
            .decrypt(&nonce, ciphertext)
            .map_err(|_| anyhow!("decryption failed"))?;

        let mut wrapped = Document::from_reader(Cursor::new(plaintext))
            .context("decrypted bytes are not a BSON document")?;

        wrapped
            .remove(FIELD)
            .ok_or_else(|| anyhow!("decrypted document has no value"))
    }

    /// Decrypts a field that may legitimately be absent or null.
    pub fn decrypt_optional(&self, value: Option<&Bson>) -> anyhow::Result<Option<Bson>> {
        match value {
            None | Some(Bson::Null) => Ok(None),
            Some(Bson::Binary(binary)) => self.decrypt(binary).map(Some),
            Some(other) => Err(anyhow!(
                "expected an encrypted field, found {:?}",
                other.element_type()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cipher() -> Cipher {
        Cipher::with_key(b"0123456789abcdef0123456789abcdef")
    }

    #[test]
    fn round_trips_a_string() {
        let cipher = cipher();
        let sealed = cipher.encrypt("a-token").unwrap();
        assert_eq!(cipher.decrypt(&sealed).unwrap(), Bson::from("a-token"));
    }

    #[test]
    fn round_trips_a_document() {
        let cipher = cipher();
        let value = doc! { "question-1": 100, "question-2": "yes" };
        let sealed = cipher.encrypt(value.clone()).unwrap();
        assert_eq!(cipher.decrypt(&sealed).unwrap(), Bson::Document(value));
    }

    #[test]
    fn each_write_uses_a_fresh_nonce() {
        let cipher = cipher();
        let first = cipher.encrypt("same").unwrap();
        let second = cipher.encrypt("same").unwrap();
        assert_ne!(first.bytes, second.bytes);
    }

    #[test]
    fn rejects_a_truncated_ciphertext() {
        let cipher = cipher();
        let sealed = cipher.encrypt("a-token").unwrap();
        let truncated = Binary {
            subtype: BinarySubtype::Generic,
            bytes: sealed.bytes[..NONCE_LEN].to_vec(),
        };
        assert!(cipher.decrypt(&truncated).is_err());
    }
}
