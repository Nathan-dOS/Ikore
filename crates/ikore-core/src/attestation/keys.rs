//! The local signing key (ADR-036 D5).
//!
//! One Ed25519 key per installation. Its PKCS#8 document, in base64, lives in
//! the credential store under [`KEY_CREDENTIAL_REF`] (the operating system's
//! keyring in the application, memory in tests); only the public key reaches
//! the database. Decoded key bytes are zeroed after use, and nothing here
//! prints or serializes private material.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use zeroize::Zeroizing;

use super::dsse::key_id;

/// The credential that holds the key's PKCS#8 document.
pub const KEY_CREDENTIAL_REF: &str = "attestation-signing-key-v1";

pub struct SigningKey {
    pair: Ed25519KeyPair,
}

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SigningKey").field(&self.key_id()).finish()
    }
}

impl SigningKey {
    /// A new key and the secret to store for it.
    pub fn generate() -> Result<(Self, Zeroizing<String>), String> {
        let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .map_err(|_| "The system random source failed.".to_owned())?;
        let pkcs8 = Zeroizing::new(document.as_ref().to_vec());
        let pair = Ed25519KeyPair::from_pkcs8(&pkcs8)
            .map_err(|_| "A generated key was rejected.".to_owned())?;
        Ok((Self { pair }, Zeroizing::new(STANDARD.encode(&*pkcs8))))
    }

    /// The key behind a stored secret.
    pub fn from_stored(secret: &str) -> Result<Self, String> {
        let pkcs8 = Zeroizing::new(
            STANDARD
                .decode(secret.trim())
                .map_err(|_| "The stored attestation key is not base64.".to_owned())?,
        );
        let pair = Ed25519KeyPair::from_pkcs8(&pkcs8)
            .map_err(|_| "The stored attestation key is not an Ed25519 key.".to_owned())?;
        Ok(Self { pair })
    }

    /// A key from a fixed seed, for test vectors only.
    #[cfg(test)]
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self {
            pair: Ed25519KeyPair::from_seed_unchecked(seed).expect("a 32-byte seed is a valid key"),
        }
    }

    pub fn public_key(&self) -> &[u8] {
        self.pair.public_key().as_ref()
    }

    pub fn key_id(&self) -> String {
        key_id(self.public_key())
    }

    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.pair.sign(message).as_ref().to_vec()
    }
}
