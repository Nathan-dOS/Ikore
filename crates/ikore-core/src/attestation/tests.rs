use super::*;
use crate::orchestration::mandate::TaskRevisionRef;

/// The seed of the test vector. It signs nothing outside these tests.
const VECTOR_SEED: [u8; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31,
];
const FIXTURES: &str = "src/attestation/fixtures";

fn sha(byte: char) -> String {
    byte.to_string().repeat(64)
}

pub(crate) fn sample_statement() -> StatementV1 {
    StatementV1::new(
        vec![
            SubjectV1 {
                name: "src/auth.rs".into(),
                digest: SubjectDigest { sha256: sha('a') },
            },
            SubjectV1 {
                name: "tests/auth_test.rs".into(),
                digest: SubjectDigest { sha256: sha('b') },
            },
        ],
        PulsationAttestationV1 {
            pulsation: PulsationBlock {
                id: "5d0c2a6e-8b1f-4c3a-9e27-61f0b4a9d2c8".into(),
                project_id: "0f6d7c1e-3a2b-4c5d-8e9f-a0b1c2d3e4f5".into(),
                pipeline_version: 4,
                outcome: "done".into(),
                terminal_reason: None,
                created_at: "2026-10-26T12:00:00Z".into(),
                ended_at: Some("2026-10-26T12:45:00Z".into()),
            },
            builder: BuilderBlock {
                hvc_version: "0.1.0".into(),
                build_id: None,
                os: "windows".into(),
                arch: "x86_64".into(),
                key_id: Some(SigningKey::from_seed(&VECTOR_SEED).key_id()),
            },
            intent: IntentBlock {
                task_revisions: vec![TaskRevisionRef {
                    task_id: "task-1".into(),
                    revision: 2,
                    sha256: sha('c'),
                }],
                specification_sha256: Some(sha('d')),
                contract_sha256: Some(sha('e')),
                review_policy_snapshot_sha256: None,
                preflight_findings: Vec::new(),
            },
            authority: AuthorityBlock {
                mandates: vec![MandateRefV1 {
                    revision: 1,
                    cause: "issued".into(),
                    sha256: sha('f'),
                }],
                approvals: vec![ApprovalRefV1 {
                    checkpoint_id: "cp-spec".into(),
                    purpose: "artifact_review".into(),
                    stage: "SPECIFICATION".into(),
                    decision: "approved".into(),
                    receipt_sha256: sha('1'),
                }],
            },
            execution: ExecutionBlock {
                stages: vec![StageRunV1 {
                    stage: "Code".into(),
                    generation: 1,
                    plan_sha256: sha('2'),
                    recipe_version: 1,
                    policy_snapshot_sha256: Some(sha('3')),
                    status: "accepted".into(),
                    budget: Some(StageBudgetRefV1 {
                        initial_invocations: 1,
                        extra_invocations: 2,
                        output_tokens: 8_000,
                        prompt_chars: 120_000,
                    }),
                    attempts: vec![AttemptRefV1 {
                        executor_kind: "code_implementer".into(),
                        transport: "api".into(),
                        backend: None,
                        effective_model: Some("gpt-4.1-mini".into()),
                        observed_model: Some("gpt-4.1-mini".into()),
                        dispatch_sha256: Some(sha('4')),
                        input_sha256: Some(sha('5')),
                        result_sha256: Some(sha('6')),
                        state: "succeeded".into(),
                    }],
                }],
            },
            evidence: EvidenceBlock {
                receipts: vec![ReceiptRefV1 {
                    check_ref: "cargo_test".into(),
                    status: "passed".into(),
                    exit_code: Some(0),
                    candidate_sha256: sha('7'),
                    profile_sha256: sha('8'),
                    toolchain_sha256: sha('9'),
                    environment_sha256: sha('0'),
                    receipt_sha256: sha('a'),
                }],
                integrator_manifest: Some(IntegratorManifestRefV1 {
                    sha256: sha('b'),
                    gap_count: 0,
                    review_policy_state: "absent".into(),
                }),
                coverage: Some(CoverageRefV1 {
                    level: "medium".into(),
                    executed: 1,
                    model_only: 0,
                    unverified: 0,
                    not_run: 0,
                    matrix_sha256: sha('c'),
                }),
                risk: vec![RiskRefV1 {
                    stage: "Integrator".into(),
                    input_revision: 1,
                    level: "medium".into(),
                    policy_sha256: sha('d'),
                }],
                acknowledgements: Vec::new(),
            },
            sanitization: SanitizationBlock {
                policy_identities: vec!["secret-sanitization-v1".into()],
                redactions: 0,
                categories: Vec::new(),
            },
            effects: vec![EffectV1 {
                path: "src/auth.rs".into(),
                base_state: Some("present".into()),
                base_sha256: Some(sha('e')),
                applied_sha256: sha('a'),
                artifact_id: "artifact-1".into(),
                producer_stage: Some("Code".into()),
            }],
            conformance: ConformanceBlock {
                within_mandate: Some(true),
                enforcement_events: 0,
                scope_grants: 0,
            },
            usage: UsageBlock {
                models: vec![ModelUsageV1 {
                    provider: "openai".into(),
                    observed_model: Some("gpt-4.1-mini".into()),
                    input: Some(1_200),
                    output: Some(300),
                    reasoning: None,
                    cache_read: None,
                    cache_write: None,
                }],
                completeness: "complete".into(),
            },
            sealing: SealingBlock {
                sealed_at: "2026-10-26T12:45:05Z".into(),
                clock: "local".into(),
                deriver_version: DERIVER_VERSION,
            },
        },
    )
}

/// Compares `bytes` with the fixture, or rewrites it with
/// `IKORE_UPDATE_GOLDEN=1` after the change was reviewed.
fn golden(name: &str, bytes: &[u8]) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURES)
        .join(name);
    if ikore_contracts::var_os_with_legacy("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
    }
    let expected = std::fs::read(&path).unwrap_or_default();
    assert!(
        expected == bytes,
        "{FIXTURES}/{name} differs; rerun with IKORE_UPDATE_GOLDEN=1 after reviewing the change"
    );
}

#[test]
fn pae_follows_the_dsse_specification() {
    assert_eq!(
        pae("http://example.com/HelloWorld", b"hello world"),
        b"DSSEv1 29 http://example.com/HelloWorld 11 hello world"
    );
    assert_eq!(
        pae(PAYLOAD_TYPE, b""),
        b"DSSEv1 28 application/vnd.in-toto+json 0 "
    );
}

#[test]
fn the_test_vector_is_stable() {
    let key = SigningKey::from_seed(&VECTOR_SEED);
    let statement = sample_statement();
    let bytes = statement.canonical_bytes().unwrap();
    assert!(
        bytes.starts_with(r#"{"_type":"https://in-toto.io/Statement/v1","predicate":{"#),
        "keys are sorted and nothing is pretty-printed: {bytes}"
    );
    assert_eq!(bytes, sample_statement().canonical_bytes().unwrap());
    let envelope = EnvelopeV1::sign(bytes.as_bytes(), &key);
    let line = envelope.to_line().unwrap();
    golden("statement.json", bytes.as_bytes());
    golden("statement.sha256", statement.sha256().unwrap().as_bytes());
    golden("vector.intoto.jsonl", format!("{line}\n").as_bytes());
    golden(
        "vector.pub.pem",
        public_key_pem(key.public_key()).as_bytes(),
    );
    golden("pae.bin", &pae(PAYLOAD_TYPE, bytes.as_bytes()));
    golden("sig.bin", &key.sign(&pae(PAYLOAD_TYPE, bytes.as_bytes())));
    assert_eq!(
        envelope.verify(key.public_key()).unwrap(),
        bytes.as_bytes(),
        "the vector verifies with its own key"
    );
}

#[test]
fn any_change_to_the_envelope_breaks_the_signature() {
    let key = SigningKey::from_seed(&VECTOR_SEED);
    let bytes = sample_statement().canonical_bytes().unwrap();
    let envelope = EnvelopeV1::sign(bytes.as_bytes(), &key);
    let mut payload = envelope.clone();
    let mut tampered = bytes.clone().into_bytes();
    tampered[10] ^= 1;
    payload.payload = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &tampered);
    assert_eq!(payload.verify(key.public_key()), Err(VerifyError::Invalid));
    let mut signature = envelope.clone();
    let mut sig = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &signature.signatures[0].sig,
    )
    .unwrap();
    sig[0] ^= 1;
    signature.signatures[0].sig =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &sig);
    assert_eq!(
        signature.verify(key.public_key()),
        Err(VerifyError::Invalid)
    );
    let mut kind = envelope.clone();
    kind.payload_type = "application/json".into();
    assert_eq!(kind.verify(key.public_key()), Err(VerifyError::Invalid));
    let other = SigningKey::generate().unwrap().0;
    assert!(matches!(
        envelope.verify(other.public_key()),
        Err(VerifyError::NotSignedBy(_))
    ));
    assert_eq!(
        EnvelopeV1::from_line("{\"payload\":\"\",\"payloadType\":\"x\",\"signatures\":[]}"),
        Err(VerifyError::Malformed)
    );
    assert_eq!(
        EnvelopeV1::from_line(&envelope.to_line().unwrap()).unwrap(),
        envelope
    );
}

#[test]
fn a_key_round_trips_through_its_stored_secret_and_never_prints_it() {
    let (key, secret) = SigningKey::generate().unwrap();
    let restored = SigningKey::from_stored(&secret).unwrap();
    assert_eq!(restored.public_key(), key.public_key());
    assert_eq!(key.public_key().len(), 32);
    assert_eq!(key.key_id().len(), 76);
    assert!(key.key_id().starts_with(KEY_ID_PREFIX));
    let shown = format!("{key:?}");
    assert!(!shown.contains(secret.as_str()), "{shown}");
    assert_eq!(shown, format!("SigningKey({:?})", key.key_id()));
    assert!(SigningKey::from_stored("not base64!").is_err());
    assert!(SigningKey::from_stored("aGVsbG8=").is_err());
}

#[test]
fn a_public_key_round_trips_through_its_pem() {
    let key = SigningKey::from_seed(&VECTOR_SEED);
    let pem = public_key_pem(key.public_key());
    assert!(pem.starts_with("-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEA"));
    assert_eq!(public_key_from_pem(&pem).unwrap(), key.public_key());
    assert_eq!(
        public_key_from_pem("-----BEGIN PUBLIC KEY-----\nAAAA\n-----END PUBLIC KEY-----"),
        None
    );
}

#[test]
fn the_first_difference_names_the_block() {
    let statement = sample_statement();
    assert_eq!(statement.first_difference(&statement.clone()), None);
    let mut usage = statement.clone();
    usage.predicate.usage.completeness = "partial".into();
    assert_eq!(statement.first_difference(&usage), Some("usage"));
    let mut subject = statement.clone();
    subject.subject[0].digest.sha256 = sha('f');
    subject.predicate.usage.completeness = "partial".into();
    assert_eq!(statement.first_difference(&subject), Some("subject"));
    let mut evidence = statement.clone();
    evidence.predicate.evidence.receipts[0].status = "failed".into();
    assert_eq!(statement.first_difference(&evidence), Some("evidence"));
}
