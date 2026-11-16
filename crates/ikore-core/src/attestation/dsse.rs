//! The DSSE v1 envelope around a statement, signed with Ed25519.
//!
//! `PAE = "DSSEv1" SP len(type) SP type SP len(body) SP body`, lengths in
//! ASCII decimal bytes, and the signature covers the PAE. The payload is the
//! standard base64 of the statement's canonical bytes. A key is named
//! `hvc-ed25519:<sha256 of its 32-byte public key>`, and its PEM is the SPKI
//! form of RFC 8410, which OpenSSL 3 reads (see `docs/attestation.md`).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};

use super::keys::SigningKey;
use crate::orchestration::ContentSha256;
use crate::orchestration::composition::canonical_json;

pub const PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";
/// The key identifier prefix for DSSE envelopes (ADR-036).
/// Preserved as `hvc-ed25519:` (Recommendation R1) to keep historical key IDs and signatures
/// cryptographically valid and verifiable by OpenSSL.
pub const KEY_ID_PREFIX: &str = "hvc-ed25519:";
/// DER of the SPKI header for an Ed25519 public key (RFC 8410).
const SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeV1 {
    pub payload: String,
    #[serde(rename = "payloadType")]
    pub payload_type: String,
    pub signatures: Vec<SignatureV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureV1 {
    pub keyid: String,
    pub sig: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    #[error("the envelope is not a DSSE envelope of an in-toto statement")]
    Malformed,
    #[error("the envelope is not signed by key `{0}`")]
    NotSignedBy(String),
    #[error("the signature does not match the payload")]
    Invalid,
}

/// The pre-authentication encoding the signature covers.
pub fn pae(payload_type: &str, payload: &[u8]) -> Vec<u8> {
    let mut bytes = format!(
        "DSSEv1 {} {payload_type} {} ",
        payload_type.len(),
        payload.len()
    )
    .into_bytes();
    bytes.extend_from_slice(payload);
    bytes
}

pub fn key_id(public_key: &[u8]) -> String {
    format!("{KEY_ID_PREFIX}{}", ContentSha256::of(public_key).as_str())
}

pub fn public_key_pem(public_key: &[u8]) -> String {
    let mut der = SPKI_PREFIX.to_vec();
    der.extend_from_slice(public_key);
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
        STANDARD.encode(der)
    )
}

/// The 32-byte key of an Ed25519 SPKI PEM, as `public_key_pem` writes it.
pub fn public_key_from_pem(pem: &str) -> Option<Vec<u8>> {
    let body = pem
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("-----"))
        .collect::<String>();
    let der = STANDARD.decode(body).ok()?;
    let key = der.strip_prefix(SPKI_PREFIX.as_slice())?;
    (key.len() == 32).then(|| key.to_vec())
}

impl EnvelopeV1 {
    /// Signs `statement` (its canonical bytes) with `key`.
    pub fn sign(statement: &[u8], key: &SigningKey) -> Self {
        Self {
            payload: STANDARD.encode(statement),
            payload_type: PAYLOAD_TYPE.to_owned(),
            signatures: vec![SignatureV1 {
                keyid: key.key_id(),
                sig: STANDARD.encode(key.sign(&pae(PAYLOAD_TYPE, statement))),
            }],
        }
    }

    /// The envelope as one canonical JSON line.
    pub fn to_line(&self) -> Result<String, String> {
        let value = serde_json::to_value(self).map_err(|error| error.to_string())?;
        canonical_json(&value).map_err(|error| error.message)
    }

    pub fn from_line(line: &str) -> Result<Self, VerifyError> {
        let envelope: Self =
            serde_json::from_str(line.trim()).map_err(|_| VerifyError::Malformed)?;
        if envelope.payload_type != PAYLOAD_TYPE || envelope.signatures.is_empty() {
            return Err(VerifyError::Malformed);
        }
        Ok(envelope)
    }

    pub fn payload_bytes(&self) -> Result<Vec<u8>, VerifyError> {
        STANDARD
            .decode(&self.payload)
            .map_err(|_| VerifyError::Malformed)
    }

    /// The key ids of the signatures, in envelope order.
    pub fn key_ids(&self) -> Vec<&str> {
        self.signatures
            .iter()
            .map(|signature| signature.keyid.as_str())
            .collect()
    }

    /// Verifies the signature by `public_key` and returns the payload.
    pub fn verify(&self, public_key: &[u8]) -> Result<Vec<u8>, VerifyError> {
        let id = key_id(public_key);
        let signature = self
            .signatures
            .iter()
            .find(|signature| signature.keyid == id)
            .ok_or_else(|| VerifyError::NotSignedBy(id.clone()))?;
        let sig = STANDARD
            .decode(&signature.sig)
            .map_err(|_| VerifyError::Malformed)?;
        let payload = self.payload_bytes()?;
        UnparsedPublicKey::new(&ED25519, public_key)
            .verify(&pae(&self.payload_type, &payload), &sig)
            .map_err(|_| VerifyError::Invalid)?;
        Ok(payload)
    }
}
