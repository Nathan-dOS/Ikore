//! Signed local attestations of finished pulsations in the host (ADR-036 M3).
//!
//! The terminal transition records a seal intent in its own transaction. The
//! background lane then seals each pending intent once the pulsation's main
//! publications settled: it derives the statement from rows, signs it with
//! the local key and stores the immutable record, all in one immediate
//! transaction on one connection. Verification re-checks the signature,
//! re-derives the statement from the current rows and hashes the subjects in
//! the workspace; the workspace level only informs.
//!
//! Included into `persistence_service` so it can use the service's private
//! helpers.

use std::collections::BTreeMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rusqlite::TransactionBehavior;
use zeroize::Zeroizing;

use super::{
    Connection, OptionalExtension, PersistenceService, PersistenceServiceError, PulsationStage,
    params,
};
use crate::attestation::{
    AcknowledgementRefV1, ApprovalRefV1, AttemptRefV1, AttestationExport, AttestationFileCheck,
    AttestationKeyRecord, AttestationKeyState, AttestationKeyStatus, AttestationState,
    AttestationStatus, AttestationVerification, AuthorityBlock, BuilderBlock, CategoryCountV1,
    ConformanceBlock, CoverageRefV1, DERIVER_VERSION, EffectV1, EnvelopeV1, EvidenceBlock,
    ExecutionBlock, FileKey, IntegratorManifestRefV1, IntentBlock, KEY_CREDENTIAL_REF,
    MAX_STATEMENT_BYTES, MandateRefV1, ModelUsageV1, PREDICATE_TYPE, PreflightFindingV1,
    PulsationAttestationV1, PulsationBlock, ReceiptRefV1, RederivationCheck, RiskRefV1,
    SanitizationBlock, SealingBlock, SignatureCheck, SigningKey, StageBudgetRefV1, StageRunV1,
    StatementV1, SubjectCheck, SubjectDigest, SubjectV1, UsageBlock, VerifyError, WorkspaceCheck,
    public_key_from_pem, public_key_pem,
};
use crate::orchestration::ContentSha256;
use crate::orchestration::composition::RelPath;
use crate::orchestration::composition::risk::{CoverageMatrix, CoverageState};
use crate::orchestration::mandate::{digest_of_set, path_key, task_revisions, text_sha256};
use crate::sanitization::SanitizationStore;

/// `runtime_settings` key; sealing is on unless it says `false`.
pub const ATTESTATION_SEALING_SETTING: &str = "attestation.sealing_enabled";
const MAX_DIAGNOSTIC_CHARS: usize = 2_000;

fn runtime(error: impl std::fmt::Display) -> PersistenceServiceError {
    PersistenceServiceError::Runtime(error.to_string())
}

fn sha_of(text: &str) -> String {
    ContentSha256::of(text.as_bytes()).as_str().to_owned()
}

/// The stored record of one attestation.
struct StoredAttestation {
    statement_json: Option<String>,
    statement_sha256: String,
    envelope_json: Option<String>,
    key_id: Option<String>,
    signature_state: String,
    outcome: String,
    subject_count: usize,
    sealed_at: String,
    signed_at: Option<String>,
}

impl PersistenceService {
    // ----------------------------------------------------------- settings

    fn sealing_enabled_on(db: &Connection) -> Result<bool, PersistenceServiceError> {
        let value: Option<String> = db
            .query_row(
                "SELECT value FROM runtime_settings WHERE key=?1",
                [ATTESTATION_SEALING_SETTING],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.as_deref() != Some("false"))
    }

    pub fn attestation_sealing_enabled(&self) -> Result<bool, PersistenceServiceError> {
        Self::sealing_enabled_on(&self.connection()?)
    }

    /// Turns sealing of pulsations that end from now on on or off.
    pub fn set_attestation_sealing_enabled(
        &self,
        enabled: bool,
    ) -> Result<(), PersistenceServiceError> {
        self.connection()?.execute(
            "INSERT INTO runtime_settings(key,value,updated_at) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
            params![
                ATTESTATION_SEALING_SETTING,
                if enabled { "true" } else { "false" },
                Self::now_iso()
            ],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------- intent

    /// Records the seal intent of a pulsation that just ended, in its
    /// terminal transition's transaction.
    pub(super) fn enqueue_attestation_seal(
        tx: &Connection,
        pulsation_id: &str,
    ) -> Result<(), PersistenceServiceError> {
        let state = if Self::sealing_enabled_on(tx)? {
            "pending"
        } else {
            "skipped"
        };
        tx.execute(
            "INSERT OR IGNORE INTO attestation_seal_intents(pulsation_id,state,created_at,updated_at) VALUES(?1,?2,?3,?3)",
            params![pulsation_id, state, Self::now_iso()],
        )?;
        Ok(())
    }

    /// Seals the oldest pending intent whose main publications settled.
    /// `false` when none is ready.
    pub fn seal_next_attestation(&self) -> Result<bool, PersistenceServiceError> {
        let next: Option<String> = self
            .connection()?
            .query_row(
                "SELECT i.pulsation_id FROM attestation_seal_intents i WHERE i.state='pending'
                   AND NOT EXISTS(SELECT 1 FROM artifact_publications o WHERE o.pulsation_id=i.pulsation_id AND o.job_class='main' AND o.state IN ('pending','applying'))
                 ORDER BY i.created_at,i.pulsation_id LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(pulsation_id) = next else {
            return Ok(false);
        };
        match self.seal_attestation(&pulsation_id) {
            Ok(()) => {}
            // A busy database is retried on the next turn.
            Err(PersistenceServiceError::Db(rusqlite::Error::SqliteFailure(failure, detail)))
                if matches!(
                    failure.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) =>
            {
                return Err(PersistenceServiceError::Db(rusqlite::Error::SqliteFailure(
                    failure, detail,
                )));
            }
            // Anything else would fail the same way again: kept, visible,
            // and out of the queue so it never holds the lane.
            Err(error) => {
                self.connection()?.execute(
                    "UPDATE attestation_seal_intents SET state='failed',diagnostic=?2,updated_at=?3 WHERE pulsation_id=?1 AND state='pending'",
                    params![
                        pulsation_id,
                        error.to_string().chars().take(MAX_DIAGNOSTIC_CHARS).collect::<String>(),
                        Self::now_iso()
                    ],
                )?;
            }
        }
        Ok(true)
    }

    /// Puts a failed seal back in the queue.
    pub fn retry_attestation_seal(
        &self,
        pulsation_id: &str,
    ) -> Result<AttestationStatus, PersistenceServiceError> {
        self.connection()?.execute(
            "UPDATE attestation_seal_intents SET state='pending',diagnostic=NULL,updated_at=?2 WHERE pulsation_id=?1 AND state='failed'",
            params![pulsation_id, Self::now_iso()],
        )?;
        self.wake_scheduler();
        self.attestation_status(pulsation_id)
    }

    fn current_builder(key_id: Option<String>) -> BuilderBlock {
        BuilderBlock {
            hvc_version: env!("CARGO_PKG_VERSION").to_owned(),
            build_id: option_env!("IKORE_BUILD_ID")
                .or(option_env!("HVC_BUILD_ID"))
                .map(str::to_owned),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            key_id,
        }
    }

    fn seal_attestation(&self, pulsation_id: &str) -> Result<(), PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pending: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM attestation_seal_intents WHERE pulsation_id=?1 AND state='pending')",
            [pulsation_id],
            |row| row.get(0),
        )?;
        if !pending {
            return Ok(());
        }
        let key = self.resolve_attestation_key(&tx, true)?;
        let sealed_at = Self::now_iso();
        let statement = Self::derive_statement_v1(
            &tx,
            pulsation_id,
            Self::current_builder(key.as_ref().ok().map(SigningKey::key_id)),
            &sealed_at,
        )?;
        let bytes = statement.canonical_bytes().map_err(runtime)?;
        let digest = sha_of(&bytes);
        let (state, json, envelope, key_id, signed_at) = if bytes.len() > MAX_STATEMENT_BYTES {
            // Only the digest is kept; the key the statement names is kept
            // with it, so a re-derivation can reproduce the builder block.
            (
                "too_large",
                None,
                None,
                statement.predicate.builder.key_id.clone(),
                None,
            )
        } else {
            match &key {
                Ok(key) => (
                    "signed",
                    Some(bytes.clone()),
                    Some(
                        EnvelopeV1::sign(bytes.as_bytes(), key)
                            .to_line()
                            .map_err(runtime)?,
                    ),
                    Some(key.key_id()),
                    Some(sealed_at.clone()),
                ),
                Err(_) => ("unsigned", Some(bytes.clone()), None, None, None),
            }
        };
        tx.execute(
            "INSERT INTO pulsation_attestations(pulsation_id,schema_version,predicate_type,deriver_version,statement_json,statement_sha256,envelope_json,key_id,signature_state,outcome,subject_count,sealed_at,signed_at)
             VALUES(?1,1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                pulsation_id,
                PREDICATE_TYPE,
                DERIVER_VERSION,
                json,
                digest,
                envelope,
                key_id,
                state,
                statement.predicate.pulsation.outcome,
                statement.subject.len() as i64,
                sealed_at,
                signed_at
            ],
        )?;
        tx.execute(
            "UPDATE attestation_seal_intents SET state='sealed',updated_at=?2 WHERE pulsation_id=?1",
            params![pulsation_id, Self::now_iso()],
        )?;
        tx.commit()?;
        Ok(())
    }

    // ---------------------------------------------------------------- keys

    fn register_attestation_key(
        db: &Connection,
        key: &SigningKey,
    ) -> Result<bool, PersistenceServiceError> {
        let known: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM attestation_keys WHERE key_id=?1)",
            [key.key_id()],
            |row| row.get(0),
        )?;
        if known {
            // A retired key never becomes active again.
            return Ok(false);
        }
        db.execute(
            "INSERT INTO attestation_keys(key_id,algorithm,public_key,state,created_at) VALUES(?1,'ed25519',?2,'active',?3)",
            params![key.key_id(), STANDARD.encode(key.public_key()), Self::now_iso()],
        )?;
        Ok(true)
    }

    /// The signing key, or why there is none. With `create`, a first key is
    /// generated when none exists anywhere; a key that went missing is never
    /// replaced silently.
    fn resolve_attestation_key(
        &self,
        db: &Connection,
        create: bool,
    ) -> Result<Result<SigningKey, AttestationKeyState>, PersistenceServiceError> {
        let active: Option<String> = db
            .query_row(
                "SELECT key_id FROM attestation_keys WHERE state='active'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Ok(secret) = self.credentials.get(KEY_CREDENTIAL_REF) else {
            return Ok(Err(AttestationKeyState::KeyringUnavailable));
        };
        let secret = secret.map(Zeroizing::new);
        Ok(match (active, secret) {
            (None, None) if create => {
                let (key, secret) = SigningKey::generate().map_err(runtime)?;
                if self.credentials.set(KEY_CREDENTIAL_REF, &secret).is_err() {
                    return Ok(Err(AttestationKeyState::KeyringUnavailable));
                }
                Self::register_attestation_key(db, &key)?;
                Ok(key)
            }
            (None, None) => Err(AttestationKeyState::NoKey),
            // A key stored by another data directory of this user is adopted.
            (None, Some(secret)) => match SigningKey::from_stored(&secret) {
                Ok(key) if Self::register_attestation_key(db, &key)? => Ok(key),
                _ => Err(AttestationKeyState::Mismatch),
            },
            (Some(_), None) => Err(AttestationKeyState::KeyMissing),
            (Some(active), Some(secret)) => match SigningKey::from_stored(&secret) {
                Ok(key) if key.key_id() == active => Ok(key),
                _ => Err(AttestationKeyState::Mismatch),
            },
        })
    }

    pub fn attestation_key_status(&self) -> Result<AttestationKeyStatus, PersistenceServiceError> {
        let db = self.connection()?;
        let state = match self.resolve_attestation_key(&db, false)? {
            Ok(_) => AttestationKeyState::Ready,
            Err(state) => state,
        };
        let keys = db
            .prepare(
                "SELECT key_id,state,created_at,retired_at FROM attestation_keys ORDER BY state='active' DESC,created_at DESC,key_id",
            )?
            .query_map([], |row| {
                Ok(AttestationKeyRecord {
                    key_id: row.get(0)?,
                    state: row.get(1)?,
                    created_at: row.get(2)?,
                    retired_at: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(AttestationKeyStatus {
            state,
            keys,
            sealing_enabled: Self::sealing_enabled_on(&db)?,
        })
    }

    /// Creates a new active key and retires the previous one: the rotation in
    /// Settings, and the explicit recovery from a missing or replaced key.
    /// Public keys are kept, so earlier attestations still verify.
    pub fn create_attestation_key(&self) -> Result<AttestationKeyStatus, PersistenceServiceError> {
        let (key, secret) = SigningKey::generate().map_err(runtime)?;
        self.credentials
            .set(KEY_CREDENTIAL_REF, &secret)
            .map_err(|_| {
                PersistenceServiceError::Validation(
                    "The operating system's credential store is unavailable; no key was created."
                        .into(),
                )
            })?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Self::now_iso();
        tx.execute(
            "UPDATE attestation_keys SET state='retired',retired_at=?1 WHERE state='active'",
            [&now],
        )?;
        Self::register_attestation_key(&tx, &key)?;
        tx.commit()?;
        self.attestation_key_status()
    }

    /// Signs an attestation sealed while no key was available.
    pub fn sign_attestation(
        &self,
        pulsation_id: &str,
    ) -> Result<AttestationStatus, PersistenceServiceError> {
        {
            let mut db = self.connection()?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let statement: Option<String> = tx
                .query_row(
                    "SELECT statement_json FROM pulsation_attestations WHERE pulsation_id=?1 AND signature_state='unsigned'",
                    [pulsation_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(statement) = statement else {
                return Err(PersistenceServiceError::Validation(
                    "Only an unsigned attestation can be signed.".into(),
                ));
            };
            let key = match self.resolve_attestation_key(&tx, true)? {
                Ok(key) => key,
                Err(state) => {
                    return Err(PersistenceServiceError::Validation(format!(
                        "No attestation key is ready ({}); check Settings → Security.",
                        serde_json::to_value(state)
                            .ok()
                            .and_then(|value| value.as_str().map(str::to_owned))
                            .unwrap_or_default()
                    )));
                }
            };
            let envelope = EnvelopeV1::sign(statement.as_bytes(), &key)
                .to_line()
                .map_err(runtime)?;
            tx.execute(
                "UPDATE pulsation_attestations SET envelope_json=?2,key_id=?3,signature_state='signed',signed_at=?4 WHERE pulsation_id=?1 AND signature_state='unsigned'",
                params![pulsation_id, envelope, key.key_id(), Self::now_iso()],
            )?;
            tx.commit()?;
        }
        self.attestation_status(pulsation_id)
    }

    // -------------------------------------------------------------- status

    fn stored_attestation(
        db: &Connection,
        pulsation_id: &str,
    ) -> Result<Option<StoredAttestation>, PersistenceServiceError> {
        Ok(db
            .query_row(
                "SELECT statement_json,statement_sha256,envelope_json,key_id,signature_state,outcome,subject_count,sealed_at,signed_at FROM pulsation_attestations WHERE pulsation_id=?1",
                [pulsation_id],
                |row| {
                    Ok(StoredAttestation {
                        statement_json: row.get(0)?,
                        statement_sha256: row.get(1)?,
                        envelope_json: row.get(2)?,
                        key_id: row.get(3)?,
                        signature_state: row.get(4)?,
                        outcome: row.get(5)?,
                        subject_count: row.get::<_, i64>(6)? as usize,
                        sealed_at: row.get(7)?,
                        signed_at: row.get(8)?,
                    })
                },
            )
            .optional()?)
    }

    fn cached_verification(
        db: &Connection,
        pulsation_id: &str,
    ) -> Result<Option<AttestationVerification>, PersistenceServiceError> {
        let row: Option<(String, String, String, Option<String>, String)> = db
            .query_row(
                "SELECT checked_at,signature,rederivation,difference,workspace_json FROM attestation_verifications WHERE pulsation_id=?1",
                [pulsation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()?;
        let Some((checked_at, signature, rederivation, difference, workspace)) = row else {
            return Ok(None);
        };
        Ok(Some(AttestationVerification {
            checked_at,
            signature: serde_json::from_value(serde_json::Value::String(signature))
                .map_err(runtime)?,
            rederivation: serde_json::from_value(serde_json::Value::String(rederivation))
                .map_err(runtime)?,
            difference,
            workspace: serde_json::from_str(&workspace).map_err(runtime)?,
        }))
    }

    /// The attestation state of a pulsation, read on `db`.
    pub(super) fn attestation_status_on(
        db: &Connection,
        pulsation_id: &str,
    ) -> Result<AttestationStatus, PersistenceServiceError> {
        let mut status = AttestationStatus {
            state: AttestationState::NotSealed,
            key_id: None,
            statement_sha256: None,
            subject_count: 0,
            outcome: None,
            sealed_at: None,
            signed_at: None,
            diagnostic: None,
            verification: None,
        };
        if let Some(stored) = Self::stored_attestation(db, pulsation_id)? {
            status.state = match stored.signature_state.as_str() {
                "signed" => AttestationState::Signed,
                "unsigned" => AttestationState::Unsigned,
                _ => AttestationState::TooLarge,
            };
            status.key_id = stored.key_id;
            status.statement_sha256 = Some(stored.statement_sha256);
            status.subject_count = stored.subject_count;
            status.outcome = Some(stored.outcome);
            status.sealed_at = Some(stored.sealed_at);
            status.signed_at = stored.signed_at;
            status.verification = Self::cached_verification(db, pulsation_id)?;
            return Ok(status);
        }
        let intent: Option<(String, Option<String>)> = db
            .query_row(
                "SELECT state,diagnostic FROM attestation_seal_intents WHERE pulsation_id=?1",
                [pulsation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((state, diagnostic)) = intent {
            status.state = match state.as_str() {
                "pending" | "sealed" => AttestationState::Pending,
                "skipped" => AttestationState::Skipped,
                _ => AttestationState::Failed,
            };
            status.diagnostic = diagnostic;
        }
        Ok(status)
    }

    pub fn attestation_status(
        &self,
        pulsation_id: &str,
    ) -> Result<AttestationStatus, PersistenceServiceError> {
        Self::attestation_status_on(&self.connection()?, pulsation_id)
    }

    // -------------------------------------------------------- verification

    /// Checks the attestation now and keeps the result for display. `None`
    /// when the pulsation has no attestation.
    pub fn verify_attestation(
        &self,
        pulsation_id: &str,
    ) -> Result<Option<AttestationVerification>, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction()?;
        let verification = Self::check_attestation_on(&tx, pulsation_id)?;
        tx.commit()?;
        if let Some(verification) = &verification {
            self.connection()?.execute(
                "INSERT INTO attestation_verifications(pulsation_id,checked_at,signature,rederivation,difference,workspace_json) VALUES(?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(pulsation_id) DO UPDATE SET checked_at=excluded.checked_at,signature=excluded.signature,rederivation=excluded.rederivation,difference=excluded.difference,workspace_json=excluded.workspace_json",
                params![
                    pulsation_id,
                    verification.checked_at,
                    verification.signature.as_str(),
                    verification.rederivation.as_str(),
                    verification.difference,
                    serde_json::to_string(&verification.workspace).map_err(runtime)?
                ],
            )?;
        }
        Ok(verification)
    }

    /// The three checks, read on `db` without keeping anything.
    pub(super) fn check_attestation_on(
        db: &Connection,
        pulsation_id: &str,
    ) -> Result<Option<AttestationVerification>, PersistenceServiceError> {
        let Some(stored) = Self::stored_attestation(db, pulsation_id)? else {
            return Ok(None);
        };
        let signature = match (&stored.envelope_json, &stored.statement_json) {
            (Some(envelope), Some(statement)) => {
                Self::check_signature(db, envelope, statement.as_bytes(), stored.key_id.as_deref())?
            }
            _ if stored.signature_state == "too_large" => SignatureCheck::TooLarge,
            _ => SignatureCheck::Unsigned,
        };
        let signed: Option<StatementV1> = stored
            .statement_json
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok());
        let (rederivation, difference, current) =
            Self::rederive(db, pulsation_id, &stored, signed.as_ref());
        let subjects = signed
            .as_ref()
            .or(current.as_ref())
            .map(|statement| statement.subject.clone())
            .unwrap_or_default();
        Ok(Some(AttestationVerification {
            checked_at: Self::now_iso(),
            signature,
            rederivation,
            difference,
            workspace: Self::check_workspace(db, pulsation_id, &subjects)?,
        }))
    }

    fn check_signature(
        db: &Connection,
        envelope: &str,
        statement: &[u8],
        key_id: Option<&str>,
    ) -> Result<SignatureCheck, PersistenceServiceError> {
        let Ok(envelope) = EnvelopeV1::from_line(envelope) else {
            return Ok(SignatureCheck::Invalid);
        };
        let Some(key_id) = key_id.or_else(|| envelope.key_ids().first().copied()) else {
            return Ok(SignatureCheck::Invalid);
        };
        let public: Option<String> = db
            .query_row(
                "SELECT public_key FROM attestation_keys WHERE key_id=?1",
                [key_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(public) = public.and_then(|public| STANDARD.decode(public).ok()) else {
            return Ok(SignatureCheck::UnknownKey);
        };
        Ok(match envelope.verify(&public) {
            Ok(payload) if payload == statement => SignatureCheck::Valid,
            Ok(_) | Err(VerifyError::Invalid | VerifyError::Malformed) => SignatureCheck::Invalid,
            Err(VerifyError::NotSignedBy(_)) => SignatureCheck::UnknownKey,
        })
    }

    /// Re-derives the statement from the current rows with the builder and
    /// seal time it recorded, and compares.
    fn rederive(
        db: &Connection,
        pulsation_id: &str,
        stored: &StoredAttestation,
        signed: Option<&StatementV1>,
    ) -> (RederivationCheck, Option<String>, Option<StatementV1>) {
        let (builder, sealed_at) = match signed {
            Some(statement) => (
                statement.predicate.builder.clone(),
                statement.predicate.sealing.sealed_at.clone(),
            ),
            // A too-large statement kept only its digest: the builder of this
            // build stands in, so only an unchanged build can match it.
            None => (
                Self::current_builder(stored.key_id.clone()),
                stored.sealed_at.clone(),
            ),
        };
        let Ok(current) = Self::derive_statement_v1(db, pulsation_id, builder, &sealed_at) else {
            return (RederivationCheck::Unavailable, None, None);
        };
        let check = match signed {
            Some(signed) => match signed.first_difference(&current) {
                None => (RederivationCheck::Matches, None),
                Some(block) => (RederivationCheck::Differs, Some(block.to_owned())),
            },
            None => match current.sha256() {
                Ok(digest) if digest == stored.statement_sha256 => {
                    (RederivationCheck::Matches, None)
                }
                _ => (RederivationCheck::Differs, None),
            },
        };
        (check.0, check.1, Some(current))
    }

    fn check_workspace(
        db: &Connection,
        pulsation_id: &str,
        subjects: &[SubjectV1],
    ) -> Result<Vec<SubjectCheck>, PersistenceServiceError> {
        let root: Option<String> = db
            .query_row(
                "SELECT p.workspace_path FROM projects p JOIN pulsations u ON u.project_id=p.id WHERE u.id=?1",
                [pulsation_id],
                |row| row.get(0),
            )
            .optional()?;
        let root = root.map(std::path::PathBuf::from);
        Ok(subjects
            .iter()
            .map(|subject| {
                let state = match (&root, RelPath::new(subject.name.clone())) {
                    (Some(root), Ok(path)) => {
                        let file = root.join(path.as_str());
                        match std::fs::read(&file) {
                            Ok(bytes)
                                if ContentSha256::of(&bytes).as_str() == subject.digest.sha256 =>
                            {
                                WorkspaceCheck::Unchanged
                            }
                            Ok(_) => WorkspaceCheck::Modified,
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                WorkspaceCheck::Missing
                            }
                            Err(_) => WorkspaceCheck::Unreadable,
                        }
                    }
                    _ => WorkspaceCheck::Unreadable,
                };
                SubjectCheck {
                    path: subject.name.clone(),
                    state,
                }
            })
            .collect())
    }

    // ------------------------------------------------------ export, import

    pub fn export_attestation(
        &self,
        pulsation_id: &str,
    ) -> Result<AttestationExport, PersistenceServiceError> {
        let db = self.connection()?;
        let stored = Self::stored_attestation(&db, pulsation_id)?
            .filter(|stored| stored.signature_state == "signed")
            .ok_or_else(|| {
                PersistenceServiceError::Validation(
                    "Only a signed attestation can be saved.".into(),
                )
            })?;
        let key_id = stored.key_id.unwrap_or_default();
        let public: String = db.query_row(
            "SELECT public_key FROM attestation_keys WHERE key_id=?1",
            [&key_id],
            |row| row.get(0),
        )?;
        let public = STANDARD.decode(public).map_err(runtime)?;
        let short = key_id
            .strip_prefix(crate::attestation::KEY_ID_PREFIX)
            .unwrap_or(&key_id)
            .chars()
            .take(8)
            .collect::<String>();
        Ok(AttestationExport {
            bundle_file_name: format!("{pulsation_id}.intoto.jsonl"),
            bundle: format!("{}\n", stored.envelope_json.unwrap_or_default()),
            key_file_name: format!("hvc-attestation-key-{short}.pub.pem"),
            public_key_pem: public_key_pem(&public),
            contents: vec![
                format!(
                    "{} applied file(s): relative paths and SHA-256 digests",
                    stored.subject_count
                ),
                "Approvals, mandate revisions, stage runs and host checks: identifiers, digests, states and counters".into(),
                "No file content, prompt, reason text, absolute path or credential; a note named after the feature keeps that name in its relative path".into(),
                format!("The public key {key_id}; the private key stays in this machine's credential store"),
            ],
        })
    }

    /// The trailer a commit of this pulsation's changes can carry.
    pub fn attestation_commit_trailer(
        &self,
        pulsation_id: &str,
    ) -> Result<String, PersistenceServiceError> {
        let stored =
            Self::stored_attestation(&self.connection()?, pulsation_id)?.ok_or_else(|| {
                PersistenceServiceError::NotFound("The pulsation has no attestation.".into())
            })?;
        Ok(crate::attestation::commit_trailer(
            pulsation_id,
            &stored.statement_sha256,
        ))
    }

    /// Checks an attestation file against this machine's keys and, when
    /// given, the public key that came with it.
    pub fn verify_attestation_file(
        &self,
        bundle: &str,
        public_key_pem_text: Option<&str>,
    ) -> Result<AttestationFileCheck, PersistenceServiceError> {
        let mut check = AttestationFileCheck {
            signature: SignatureCheck::Invalid,
            key: FileKey::Unavailable,
            key_id: None,
            pulsation_id: None,
            statement_sha256: None,
            subject_count: 0,
        };
        let Some(line) = bundle.lines().find(|line| !line.trim().is_empty()) else {
            return Ok(check);
        };
        let Ok(envelope) = EnvelopeV1::from_line(line) else {
            return Ok(check);
        };
        let key_id = envelope.key_ids().first().map(|id| (*id).to_owned());
        check.key_id.clone_from(&key_id);
        if let Ok(payload) = envelope.payload_bytes()
            && let Ok(statement) = serde_json::from_slice::<StatementV1>(&payload)
        {
            check.pulsation_id = Some(statement.predicate.pulsation.id.clone());
            check.statement_sha256 = Some(ContentSha256::of(&payload).as_str().to_owned());
            check.subject_count = statement.subject.len();
        }
        let known: Option<String> = match &key_id {
            Some(id) => self
                .connection()?
                .query_row(
                    "SELECT public_key FROM attestation_keys WHERE key_id=?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()?,
            None => None,
        };
        let (public, key) = match (
            known.and_then(|key| STANDARD.decode(key).ok()),
            public_key_pem_text.and_then(public_key_from_pem),
        ) {
            (Some(public), _) => (Some(public), FileKey::KnownOnThisMachine),
            (None, Some(public)) => (Some(public), FileKey::UnknownKey),
            (None, None) => (None, FileKey::Unavailable),
        };
        check.key = key;
        check.signature = match public {
            Some(public) => match envelope.verify(&public) {
                Ok(_) => SignatureCheck::Valid,
                Err(VerifyError::NotSignedBy(_)) => SignatureCheck::UnknownKey,
                Err(_) => SignatureCheck::Invalid,
            },
            None => SignatureCheck::UnknownKey,
        };
        Ok(check)
    }

    // ------------------------------------------------------------ deriver

    /// Deriver v1: the statement of a finished pulsation from its rows, with
    /// `builder` and `sealed_at` given so a re-derivation can reproduce the
    /// sealed bytes. Reads on `db` only.
    pub(super) fn derive_statement_v1(
        db: &Connection,
        pulsation_id: &str,
        builder: BuilderBlock,
        sealed_at: &str,
    ) -> Result<StatementV1, PersistenceServiceError> {
        let (project_id, version, status, terminal_reason, created_at, input_revision): (
            String,
            u32,
            String,
            Option<String>,
            String,
            u64,
        ) = db.query_row(
            "SELECT project_id,pipeline_version,status,terminal_reason,created_at,input_revision FROM pulsations WHERE id=?1",
            [pulsation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )?;
        let outcome = match (status.as_str(), terminal_reason.as_deref()) {
            ("done", _) => "done",
            (_, Some("hitl_rejected")) => "rejected",
            (_, Some("user_cancel")) => "aborted",
            _ => "failed",
        };
        let ended_at: Option<String> = db
            .query_row(
                "SELECT created_at FROM pulse_events WHERE pulsation_id=?1 AND to_status IN ('done','failed') ORDER BY created_at DESC,rowid DESC LIMIT 1",
                [pulsation_id],
                |row| row.get(0),
            )
            .optional()?;

        // Subjects and effects: the last applied main publication of each path.
        let mut applied = BTreeMap::<String, (String, String, String, Option<String>)>::new();
        for row in db
            .prepare(
                "SELECT o.relative_path,o.desired_sha256,o.artifact_id,a.producer_stage FROM artifact_publications o JOIN artifacts a ON a.id=o.artifact_id
                 WHERE o.pulsation_id=?1 AND o.job_class='main' AND o.state='applied' ORDER BY o.updated_at,o.rowid",
            )?
            .query_map([pulsation_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?
        {
            let (path, digest, artifact, stage) = row?;
            applied.insert(path_key(&path), (path, digest, artifact, stage));
        }
        let bases = db
            .prepare(
                "SELECT path_key,state,content_sha256 FROM pulsation_file_bases WHERE pulsation_id=?1",
            )?
            .query_map([pulsation_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    (row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?),
                ))
            })?
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let subject = applied
            .values()
            .map(|(path, digest, _, _)| SubjectV1 {
                name: path.clone(),
                digest: SubjectDigest {
                    sha256: digest.clone(),
                },
            })
            .collect();
        let effects = applied
            .iter()
            .map(|(key, (path, digest, artifact, stage))| {
                let base = bases.get(key);
                EffectV1 {
                    path: path.clone(),
                    base_state: base.map(|(state, _)| state.clone()),
                    base_sha256: base.and_then(|(_, sha)| sha.clone()),
                    applied_sha256: digest.clone(),
                    artifact_id: artifact.clone(),
                    producer_stage: stage.clone(),
                }
            })
            .collect();

        // Intent.
        let manifest = crate::feature_forge::pipeline::manifest(db, pulsation_id)?;
        let accepted = |stage: &str| -> Result<Option<String>, PersistenceServiceError> {
            let digests = db
                .prepare(
                    "SELECT content_sha256 FROM artifacts WHERE pulsation_id=?1 AND producer_stage=?2 AND acceptance='accepted' AND input_revision=?3 AND content_sha256 IS NOT NULL",
                )?
                .query_map(params![pulsation_id, stage, input_revision], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            if digests.is_empty() {
                return Ok(None);
            }
            Ok(Some(digest_of_set(&digests).map_err(runtime)?))
        };
        let mut preflight_findings = Vec::new();
        if let Some(manifest) = &manifest {
            for feature in &manifest.features {
                if let Some((revision, sha, choice)) = db
                    .query_row(
                        "SELECT feature_revision,inputs_sha256,resolution FROM forge_preflight_findings WHERE feature_id=?1 ORDER BY created_at DESC,id DESC LIMIT 1",
                        [&feature.id],
                        |row| Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
                    )
                    .optional()?
                {
                    preflight_findings.push(PreflightFindingV1 {
                        feature_revision: revision,
                        decisions_sha256: sha,
                        choice,
                    });
                }
            }
        }
        preflight_findings.sort_by(|a, b| {
            (a.feature_revision, &a.decisions_sha256)
                .cmp(&(b.feature_revision, &b.decisions_sha256))
        });
        let intent = IntentBlock {
            task_revisions: match &manifest {
                Some(manifest) => task_revisions(manifest).map_err(runtime)?,
                None => Vec::new(),
            },
            specification_sha256: accepted(PulsationStage::Specification.as_db_value())?,
            contract_sha256: accepted(PulsationStage::Contract.as_db_value())?,
            review_policy_snapshot_sha256: db
                .query_row(
                    "SELECT snapshot_sha256 FROM pulsation_review_policy_snapshots WHERE pulsation_id=?1",
                    [pulsation_id],
                    |row| row.get(0),
                )
                .optional()?,
            preflight_findings,
        };

        // Authority.
        let mandates = db
            .prepare(
                "SELECT revision,cause,mandate_sha256 FROM execution_mandates WHERE pulsation_id=?1 ORDER BY revision",
            )?
            .query_map([pulsation_id], |row| {
                Ok(MandateRefV1 {
                    revision: row.get(0)?,
                    cause: row.get(1)?,
                    sha256: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let approvals = db
            .prepare(
                "SELECT checkpoint_id,purpose,COALESCE(json_extract(receipt_json,'$.stage'),''),decision,receipt_sha256 FROM checkpoint_approval_receipts WHERE pulsation_id=?1 ORDER BY created_at,checkpoint_id",
            )?
            .query_map([pulsation_id], |row| {
                Ok(ApprovalRefV1 {
                    checkpoint_id: row.get(0)?,
                    purpose: row.get(1)?,
                    stage: row.get(2)?,
                    decision: row.get(3)?,
                    receipt_sha256: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        // Execution.
        let mut stages = db
            .prepare(
                "SELECT e.id,e.stage,e.parent_generation,e.plan_sha256,e.recipe_version,s.sha256,e.status,
                        b.initial_invocation_limit,b.extra_invocation_limit,b.initial_output_token_limit+b.extra_output_token_limit,b.initial_prompt_char_limit+b.extra_prompt_char_limit
                 FROM stage_executions e LEFT JOIN composition_policy_snapshots s ON s.id=e.policy_snapshot_id
                 LEFT JOIN stage_budget_accounts b ON b.id=e.budget_account_id
                 WHERE e.pulsation_id=?1 ORDER BY e.created_at,e.rowid",
            )?
            .query_map([pulsation_id], |row| {
                let budget = match row.get::<_, Option<u32>>(7)? {
                    Some(initial) => Some(StageBudgetRefV1 {
                        initial_invocations: initial,
                        extra_invocations: row.get(8)?,
                        output_tokens: row.get(9)?,
                        prompt_chars: row.get(10)?,
                    }),
                    None => None,
                };
                Ok((
                    row.get::<_, String>(0)?,
                    StageRunV1 {
                        stage: row.get(1)?,
                        generation: row.get(2)?,
                        plan_sha256: row.get(3)?,
                        recipe_version: row.get(4)?,
                        policy_snapshot_sha256: row.get(5)?,
                        status: row.get(6)?,
                        budget,
                        attempts: Vec::new(),
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (execution, stage) in &mut stages {
            stage.attempts = db
                .prepare(
                    "SELECT a.executor_kind,COALESCE(json_extract(s.snapshot_json,'$.transport'),'api'),json_extract(s.snapshot_json,'$.backend'),a.effective_model,
                            COALESCE(a.observed_model,
                              (SELECT o.observed_model FROM llm_model_observations o WHERE o.parent_invocation_id=a.id AND o.observed_model IS NOT NULL ORDER BY o.created_at DESC,o.ordinal DESC LIMIT 1),
                              (SELECT t.observed_model FROM terminal_runs t WHERE t.agent_run_id=a.agent_run_id AND t.observed_model IS NOT NULL ORDER BY t.created_at DESC LIMIT 1)),
                            a.dispatch_sha256,a.input_sha256,a.result_sha256,a.state
                     FROM stage_work_unit_attempts a LEFT JOIN composition_policy_snapshots s ON s.id=a.profile_snapshot_id
                     WHERE a.execution_id=?1 ORDER BY a.created_at,a.rowid",
                )?
                .query_map([execution.as_str()], |row| {
                    Ok(AttemptRefV1 {
                        executor_kind: row.get(0)?,
                        transport: row.get(1)?,
                        backend: row.get(2)?,
                        effective_model: row.get(3)?,
                        observed_model: row.get(4)?,
                        dispatch_sha256: row.get(5)?,
                        input_sha256: row.get(6)?,
                        result_sha256: row.get(7)?,
                        state: row.get(8)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
        }
        let execution = ExecutionBlock {
            stages: stages.into_iter().map(|(_, stage)| stage).collect(),
        };

        // Evidence.
        let receipts = db
            .prepare(
                "SELECT r.check_ref,r.status,r.exit_code,r.candidate_sha256,r.profile_sha256,r.toolchain_sha256,r.environment_sha256,r.receipt_json
                 FROM host_validation_runs r JOIN stage_executions e ON e.id=r.execution_id WHERE e.pulsation_id=?1 ORDER BY r.created_at,r.rowid",
            )?
            .query_map([pulsation_id], |row| {
                Ok(ReceiptRefV1 {
                    check_ref: row.get(0)?,
                    status: row.get(1)?,
                    exit_code: row.get(2)?,
                    candidate_sha256: row.get(3)?,
                    profile_sha256: row.get(4)?,
                    toolchain_sha256: row.get(5)?,
                    environment_sha256: row.get(6)?,
                    receipt_sha256: sha_of(&row.get::<_, String>(7)?),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        // The Integrator's inputs and coverage of the final input revision.
        let integrator_manifest = db
            .query_row(
                "SELECT manifest_sha256,gap_count,review_policy_state FROM integrator_review_inputs WHERE pulsation_id=?1 AND input_revision=?2 ORDER BY created_at DESC,rowid DESC LIMIT 1",
                params![pulsation_id, input_revision],
                |row| {
                    Ok(IntegratorManifestRefV1 {
                        sha256: row.get(0)?,
                        gap_count: row.get(1)?,
                        review_policy_state: row.get(2)?,
                    })
                },
            )
            .optional()?;
        let coverage = db
            .query_row(
                "SELECT level,matrix_json FROM integrator_coverage WHERE pulsation_id=?1 AND input_revision=?2 ORDER BY created_at DESC,rowid DESC LIMIT 1",
                params![pulsation_id, input_revision],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .map(|(level, json)| -> Result<CoverageRefV1, PersistenceServiceError> {
                let matrix: CoverageMatrix = serde_json::from_str(&json).map_err(runtime)?;
                let count = |state: CoverageState| {
                    matrix.rows.iter().filter(|row| row.state == state).count() as u32
                };
                Ok(CoverageRefV1 {
                    level,
                    executed: count(CoverageState::Executed),
                    model_only: count(CoverageState::ModelOnly),
                    unverified: count(CoverageState::Unverified),
                    not_run: count(CoverageState::NotRun),
                    matrix_sha256: sha_of(&json),
                })
            })
            .transpose()?;
        let risk = db
            .prepare(
                "SELECT stage,input_revision,level,policy_sha256 FROM pulsation_risk_assessments WHERE pulsation_id=?1 ORDER BY input_revision,stage",
            )?
            .query_map([pulsation_id], |row| {
                Ok(RiskRefV1 {
                    stage: row.get(0)?,
                    input_revision: row.get(1)?,
                    level: row.get(2)?,
                    policy_sha256: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let acknowledgements = db
            .prepare(
                "SELECT 'risk',reason,created_at,id FROM pulsation_risk_acknowledgements WHERE pulsation_id=?1
                 UNION ALL SELECT 'convergence',reason,created_at,id FROM pulsation_convergence_decisions WHERE pulsation_id=?1
                 ORDER BY 3,4",
            )?
            .query_map([pulsation_id], |row| {
                Ok(AcknowledgementRefV1 {
                    kind: row.get(0)?,
                    reason_sha256: text_sha256(&row.get::<_, String>(1)?),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let evidence = EvidenceBlock {
            receipts,
            integrator_manifest,
            coverage,
            risk,
            acknowledgements,
        };

        // Sanitization: totals and how many sanitized inputs and outputs
        // redacted each category, read like the sanitization summary.
        let summary =
            SanitizationStore::summary_on(db, &project_id, Some(pulsation_id)).map_err(runtime)?;
        let categories = SanitizationStore::category_counts_on(db, &project_id, Some(pulsation_id))
            .map_err(runtime)?;
        let policy_identities = db
            .prepare(
                "SELECT DISTINCT policy_identity FROM secret_sanitization_receipts WHERE pulsation_id=?1 ORDER BY policy_identity",
            )?
            .query_map([pulsation_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let sanitization = SanitizationBlock {
            policy_identities,
            redactions: summary.input_redaction_count + summary.output_redaction_count,
            categories: categories
                .into_iter()
                .map(|(category, count)| CategoryCountV1 { category, count })
                .collect(),
        };

        // Conformance: only an enforced mandate can be kept or broken.
        let (events, scope_grants, enforced): (u32, u32, bool) = db.query_row(
            "SELECT (SELECT COUNT(*) FROM mandate_enforcement_events WHERE pulsation_id=?1),(SELECT COUNT(*) FROM execution_mandates WHERE pulsation_id=?1 AND cause='scope_grant'),
                    EXISTS(SELECT 1 FROM execution_mandates WHERE pulsation_id=?1 AND enforcement='enforced_v4')",
            [pulsation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let conformance = ConformanceBlock {
            within_mandate: enforced.then_some(events == 0),
            enforcement_events: events,
            scope_grants,
        };

        // Usage: unknown counters stay unknown.
        let models = db
            .prepare(
                "SELECT provider_id,observed_model,
                        CASE WHEN COUNT(input_tokens)=COUNT(*) THEN SUM(input_tokens) END,
                        CASE WHEN COUNT(output_tokens)=COUNT(*) THEN SUM(output_tokens) END,
                        CASE WHEN COUNT(reasoning_tokens)=COUNT(*) THEN SUM(reasoning_tokens) END,
                        CASE WHEN COUNT(cache_read_tokens)=COUNT(*) THEN SUM(cache_read_tokens) END,
                        CASE WHEN COUNT(cache_write_tokens)=COUNT(*) THEN SUM(cache_write_tokens) END
                 FROM token_usage_records WHERE pulsation_id=?1 AND historical=0
                 GROUP BY provider_id,observed_model ORDER BY provider_id,observed_model",
            )?
            .query_map([pulsation_id], |row| {
                Ok(ModelUsageV1 {
                    provider: row.get(0)?,
                    observed_model: row.get(1)?,
                    input: row.get(2)?,
                    output: row.get(3)?,
                    reasoning: row.get(4)?,
                    cache_read: row.get(5)?,
                    cache_write: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let (records, complete): (u32, u32) = db.query_row(
            "SELECT COUNT(*),COUNT(CASE WHEN quality='complete' THEN 1 END) FROM token_usage_records WHERE pulsation_id=?1 AND historical=0",
            [pulsation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let usage = UsageBlock {
            models,
            completeness: match (records, complete) {
                (0, _) => "unavailable",
                (all, complete) if all == complete => "complete",
                _ => "partial",
            }
            .to_owned(),
        };

        Ok(StatementV1::new(
            subject,
            PulsationAttestationV1 {
                pulsation: PulsationBlock {
                    id: pulsation_id.to_owned(),
                    project_id,
                    pipeline_version: version,
                    outcome: outcome.to_owned(),
                    terminal_reason,
                    created_at,
                    ended_at,
                },
                builder,
                intent,
                authority: AuthorityBlock {
                    mandates,
                    approvals,
                },
                execution,
                evidence,
                sanitization,
                effects,
                conformance,
                usage,
                sealing: SealingBlock {
                    sealed_at: sealed_at.to_owned(),
                    clock: "local".to_owned(),
                    deriver_version: DERIVER_VERSION,
                },
            },
        ))
    }
}

#[cfg(test)]
#[path = "attestation_persistence_tests.rs"]
mod tests;
