//! Signed local attestation of a finished pulsation (ADR-036).
//!
//! When a pulsation ends, the host seals an in-toto Statement v1 whose
//! subjects are the files it applied and whose predicate records, as digests,
//! states and counters only, what was approved, granted, run and checked. The
//! canonical bytes are signed in a DSSE envelope with an Ed25519 key kept in
//! the operating system's credential store. The attestation proves integrity
//! since sealing and which local key sealed it; it claims no SLSA level, no
//! trusted time and no personal identity.
//!
//! This module is pure: statement types, the DSSE encoding, the key material
//! and the states the host reports. Reading rows, sealing and verifying live
//! in the host (`persistence_service::attestation_persistence`).

pub mod dsse;
pub mod keys;
pub mod statement;
pub mod status;
#[cfg(test)]
mod tests;

pub use dsse::{
    EnvelopeV1, KEY_ID_PREFIX, PAYLOAD_TYPE, SignatureV1, VerifyError, key_id, pae,
    public_key_from_pem, public_key_pem,
};
pub use keys::{KEY_CREDENTIAL_REF, SigningKey};
pub use statement::*;
pub use status::*;
