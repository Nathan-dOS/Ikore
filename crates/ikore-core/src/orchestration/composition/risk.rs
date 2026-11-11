//! Risk-driven scrutiny of a composed pulsation (ADR-034, M3).
//!
//! A versioned policy turns the facts the host already holds into one risk
//! level: the Intake's per-task risk, the `risk` of the notes delivered as
//! brief sources, sensitive paths, a changed interface and the size of the
//! change. The level is the maximum signal; an `unknown` input stays visible
//! and selects like `medium`. The level only ever adds scrutiny: more checks
//! become required, the diff budget favors sensitive files and approving a
//! high-risk review with unverified items needs an acknowledgement. Nothing
//! here removes a stage, a gate or a checkpoint. Everything is pure: the host
//! gathers the facts.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use super::recipes::{StageInputs, contract_of, intake_of, specification_of};
use super::{
    CheckKind, CompositionError, ErrorCode, GroupIntent, Handle, InterfaceIntent, Invariant, Risk,
    Sha256Hex, Stage, ValidationCommandProfileV1, Verdict, canonical_sha256,
};

/// Version of the built-in rules below. Changing a glob, a threshold or the
/// selection table is a new version.
pub const RISK_POLICY_VERSION: u32 = 1;

/// Paths whose change raises the level to `high` (hypothesis, ADR-034).
pub const BUILTIN_SENSITIVE_GLOBS: &[&str] = &[
    "**/auth/**",
    "**/*auth*.*",
    "**/*crypt*.*",
    "**/*secret*.*",
    "**/*credential*.*",
    "**/*token*.*",
    "**/*permission*.*",
    "**/migrations/**",
    "**/*.sql",
    ".github/workflows/**",
    "scripts/**",
    "**/Dockerfile*",
    "**/docker-compose*.yml",
    "**/Cargo.lock",
    "**/package-lock.json",
    "**/pnpm-lock.yaml",
    "**/yarn.lock",
    "**/.env*",
    "**/*.pem",
    "**/*.key",
];

/// More planned or candidate files than this raise the level to `medium`.
pub const LARGE_CHANGE_FILES: usize = 20;

pub const MAX_PROJECT_GLOBS: usize = 64;
pub const MAX_GLOB_CHARS: usize = 200;
pub const MAX_EXTRA_CHECKS: usize = 32;

/// Checks a project adds per level. Never removes a built-in rule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtraRequiredChecks {
    #[serde(default)]
    pub medium: Vec<Handle>,
    #[serde(default)]
    pub high: Vec<Handle>,
}

/// The project's additions to the built-in policy (composition settings
/// `riskPolicy`). Additions only: a project cannot lower a built-in rule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectRiskPolicy {
    #[serde(default)]
    pub sensitive_path_globs: Vec<String>,
    #[serde(default)]
    pub extra_required_check_refs: ExtraRequiredChecks,
}

impl ProjectRiskPolicy {
    pub fn is_empty(&self) -> bool {
        self.sensitive_path_globs.is_empty()
            && self.extra_required_check_refs.medium.is_empty()
            && self.extra_required_check_refs.high.is_empty()
    }

    pub fn validate(&self) -> Result<(), CompositionError> {
        let invalid = |message: String| {
            CompositionError::new(Invariant::Sc006, ErrorCode::HostContractError, message)
        };
        if self.sensitive_path_globs.len() > MAX_PROJECT_GLOBS {
            return Err(invalid(format!(
                "riskPolicy.sensitivePathGlobs accepts at most {MAX_PROJECT_GLOBS} globs."
            )));
        }
        for glob in &self.sensitive_path_globs {
            if glob.trim().is_empty() || glob.chars().count() > MAX_GLOB_CHARS {
                return Err(invalid(format!(
                    "riskPolicy.sensitivePathGlobs entries must have 1 to {MAX_GLOB_CHARS} characters."
                )));
            }
            Glob::new(&normalize_glob(glob)).map_err(|error| {
                invalid(format!("Invalid sensitive path glob `{glob}`: {error}"))
            })?;
        }
        for (level, refs) in [
            ("medium", &self.extra_required_check_refs.medium),
            ("high", &self.extra_required_check_refs.high),
        ] {
            if refs.len() > MAX_EXTRA_CHECKS {
                return Err(invalid(format!(
                    "riskPolicy.extraRequiredCheckRefs.{level} accepts at most {MAX_EXTRA_CHECKS} checks."
                )));
            }
            let unique = refs.iter().collect::<HashSet<_>>();
            if unique.len() != refs.len() {
                return Err(invalid(format!(
                    "riskPolicy.extraRequiredCheckRefs.{level} repeats a check."
                )));
            }
        }
        Ok(())
    }
}

/// Whether the project's additions apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectPolicyState {
    BuiltinOnly,
    ProjectApplied,
    /// The stored additions failed validation: the built-in policy applies.
    ProjectPolicyInvalid,
}

/// The policy one assessment used: built-in rules plus valid project additions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RiskPolicyV1 {
    pub version: u32,
    pub sensitive_path_globs: Vec<String>,
    pub large_change_files: usize,
    pub extra_required_check_refs: ExtraRequiredChecks,
    pub project_state: ProjectPolicyState,
}

impl RiskPolicyV1 {
    pub fn builtin() -> Self {
        Self {
            version: RISK_POLICY_VERSION,
            sensitive_path_globs: BUILTIN_SENSITIVE_GLOBS
                .iter()
                .map(|glob| (*glob).to_owned())
                .collect(),
            large_change_files: LARGE_CHANGE_FILES,
            extra_required_check_refs: ExtraRequiredChecks::default(),
            project_state: ProjectPolicyState::BuiltinOnly,
        }
    }

    /// The built-in policy plus the project's additions when they validate.
    pub fn effective(project: &ProjectRiskPolicy) -> Self {
        let mut policy = Self::builtin();
        if project.is_empty() {
            return policy;
        }
        if project.validate().is_err() {
            policy.project_state = ProjectPolicyState::ProjectPolicyInvalid;
            return policy;
        }
        for glob in &project.sensitive_path_globs {
            if !policy.sensitive_path_globs.contains(glob) {
                policy.sensitive_path_globs.push(glob.clone());
            }
        }
        policy.extra_required_check_refs = project.extra_required_check_refs.clone();
        policy.project_state = ProjectPolicyState::ProjectApplied;
        policy
    }

    pub fn sha256(&self) -> String {
        canonical_sha256(self, &[])
            .map(|sha| sha.as_str().to_owned())
            .unwrap_or_else(|_| Sha256Hex::of(b"unhashable").as_str().to_owned())
    }

    fn compile(globs: impl IntoIterator<Item = impl AsRef<str>>) -> GlobSet {
        let mut builder = GlobSetBuilder::new();
        for glob in globs {
            if let Ok(glob) = Glob::new(&normalize_glob(glob.as_ref())) {
                builder.add(glob);
            }
        }
        builder.build().unwrap_or_else(|_| GlobSet::empty())
    }

    /// The compiled globs. The built-in list, which most projects use as is,
    /// is compiled once: the detail reads it on every refresh (ADR-035 M2).
    fn sensitive_set(&self) -> std::borrow::Cow<'static, GlobSet> {
        static BUILTIN: std::sync::OnceLock<GlobSet> = std::sync::OnceLock::new();
        if self
            .sensitive_path_globs
            .iter()
            .map(String::as_str)
            .eq(BUILTIN_SENSITIVE_GLOBS.iter().copied())
        {
            std::borrow::Cow::Borrowed(
                BUILTIN.get_or_init(|| Self::compile(BUILTIN_SENSITIVE_GLOBS)),
            )
        } else {
            std::borrow::Cow::Owned(Self::compile(&self.sensitive_path_globs))
        }
    }

    /// The paths among `paths` that match a sensitive glob, case-insensitive
    /// and with either separator.
    pub fn sensitive_paths<'a>(&self, paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let set = self.sensitive_set();
        let mut hits = paths
            .into_iter()
            .map(normalize_path)
            .filter(|path| set.is_match(path))
            .collect::<Vec<_>>();
        hits.sort();
        hits.dedup();
        hits
    }
}

fn normalize_glob(glob: &str) -> String {
    glob.trim().replace('\\', "/").to_ascii_lowercase()
}

fn normalize_path(path: &str) -> String {
    path.trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_ascii_lowercase()
}

/// A note's own `risk` from its YAML front matter, if it declares one.
pub fn note_risk(content: &str) -> Option<Risk> {
    let body = content.strip_prefix('\u{feff}').unwrap_or(content);
    let rest = body
        .strip_prefix("---\n")
        .or_else(|| body.strip_prefix("---\r\n"))?;
    let end = rest.find("\n---")?;
    let front: serde_yaml::Value = serde_yaml::from_str(&rest[..end]).ok()?;
    let value = front.get("risk")?.as_str()?.trim().to_ascii_lowercase();
    match value.as_str() {
        "low" => Some(Risk::Low),
        "medium" => Some(Risk::Medium),
        "high" => Some(Risk::High),
        "unknown" => Some(Risk::Unknown),
        _ => None,
    }
}

/// The risk facts of a composed stage: the Intake's per-task risk, the risk
/// of the notes delivered as sources, the Specification's planned and
/// existing files plus `candidates`, and the interfaces marked as changed.
pub fn inputs_from_stage(inputs: &StageInputs, candidates: &[String]) -> RiskInputs {
    let mut facts = RiskInputs::default();
    // Files granted at a failure checkpoint are written like planned ones.
    facts.paths.extend(
        inputs
            .scope_amendments
            .iter()
            .map(|(_, path)| path.as_str().to_owned()),
    );
    if let Some(intake) = inputs.prior.get(&Stage::Intake).and_then(intake_of) {
        facts.intake = intake
            .tasks
            .iter()
            .map(|task| (task.task_ref.clone(), task.risk))
            .collect();
    }
    for source in &inputs.sources {
        let path = source.path.as_str();
        if path.to_ascii_lowercase().ends_with(".md")
            && let Some(risk) = note_risk(&source.content)
        {
            facts.notes.push((path.to_owned(), risk));
        }
    }
    if let Some(record) = inputs.prior.get(&Stage::Specification)
        && let Some(spec) = specification_of(record)
    {
        for handle in spec
            .planned_file_refs
            .iter()
            .chain(spec.existing_file_refs.iter())
        {
            if let Some(path) = record.path_of(handle) {
                facts.paths.push(path.as_str().to_owned());
            }
        }
        for intent in &spec.interface_intent {
            if intent.intent == InterfaceIntent::Change {
                let paths = intent
                    .source_refs
                    .iter()
                    .filter_map(|handle| record.path_of(handle))
                    .map(|path| path.as_str().to_owned())
                    .collect::<Vec<_>>();
                facts.changed_interfaces.push(if paths.is_empty() {
                    "specification interface".to_owned()
                } else {
                    paths.join(", ")
                });
            }
        }
    }
    if let Some(contract) = inputs.prior.get(&Stage::Contract).and_then(contract_of) {
        for group in &contract.groups {
            if group.intent == GroupIntent::Change {
                facts
                    .changed_interfaces
                    .push(group.interface_key.to_string());
            }
        }
    }
    facts.paths.extend(candidates.iter().cloned());
    facts
}

/// The facts one assessment reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RiskInputs {
    /// The Intake's per-task risk; empty when no Intake payload was accepted.
    pub intake: Vec<(Handle, Risk)>,
    /// `risk` of the notes delivered as sources, by path.
    pub notes: Vec<(String, Risk)>,
    /// Planned and existing files (Code/Test) or candidates (Integrator).
    pub paths: Vec<String>,
    /// Interfaces the Specification or the Contract marks as changed.
    pub changed_interfaces: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskSignal {
    IntakeTask,
    NoteRisk,
    SensitivePath,
    InterfaceChange,
    ChangeSize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RiskReason {
    pub signal: RiskSignal,
    pub level: Risk,
    /// English, value-free: a task handle, a path or an interface key.
    pub subject: String,
}

impl RiskReason {
    /// One English line for the report and the Intent layer.
    pub fn describe(&self) -> String {
        format!(
            "{} `{}` ({})",
            match self.signal {
                RiskSignal::IntakeTask => "Intake risk of task",
                RiskSignal::NoteRisk => "note risk",
                RiskSignal::SensitivePath => "sensitive path",
                RiskSignal::InterfaceChange => "changed interface",
                RiskSignal::ChangeSize => "change size",
            },
            self.subject,
            super::review_boundary::risk_level_name(self.level)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RiskAssessment {
    pub level: Risk,
    pub reasons: Vec<RiskReason>,
    /// Inputs that said `unknown` or were missing.
    pub unknown_inputs: Vec<String>,
    pub policy_version: u32,
    pub policy_sha256: String,
    pub project_policy_state: ProjectPolicyState,
}

const fn rank(level: Risk) -> u8 {
    match level {
        Risk::Low => 0,
        Risk::Unknown => 1,
        Risk::Medium => 2,
        Risk::High => 3,
    }
}

/// The level that selects checks: `unknown` selects like `medium`.
pub const fn selection_level(level: Risk) -> Risk {
    match level {
        Risk::Unknown => Risk::Medium,
        other => other,
    }
}

pub fn assess(inputs: &RiskInputs, policy: &RiskPolicyV1) -> RiskAssessment {
    let mut reasons = BTreeSet::new();
    let mut unknown = BTreeSet::new();
    if inputs.intake.is_empty() {
        unknown.insert("intake".to_owned());
    }
    for (task, risk) in &inputs.intake {
        match risk {
            Risk::Unknown => {
                unknown.insert(format!("intake:{task}"));
            }
            Risk::Low => {}
            level => {
                reasons.insert(RiskReason {
                    signal: RiskSignal::IntakeTask,
                    level: *level,
                    subject: task.to_string(),
                });
            }
        }
    }
    for (path, risk) in &inputs.notes {
        match risk {
            Risk::Medium | Risk::High => {
                reasons.insert(RiskReason {
                    signal: RiskSignal::NoteRisk,
                    level: *risk,
                    subject: path.clone(),
                });
            }
            Risk::Unknown => {
                unknown.insert(format!("note:{path}"));
            }
            Risk::Low => {}
        }
    }
    for path in policy.sensitive_paths(inputs.paths.iter().map(String::as_str)) {
        reasons.insert(RiskReason {
            signal: RiskSignal::SensitivePath,
            level: Risk::High,
            subject: path,
        });
    }
    for interface in &inputs.changed_interfaces {
        reasons.insert(RiskReason {
            signal: RiskSignal::InterfaceChange,
            level: Risk::Medium,
            subject: interface.clone(),
        });
    }
    let files = inputs
        .paths
        .iter()
        .map(|path| normalize_path(path))
        .collect::<BTreeSet<_>>()
        .len();
    if files > policy.large_change_files {
        reasons.insert(RiskReason {
            signal: RiskSignal::ChangeSize,
            level: Risk::Medium,
            subject: format!("{files} files"),
        });
    }
    let known = reasons
        .iter()
        .map(|reason| reason.level)
        .max_by_key(|level| rank(*level))
        .unwrap_or(Risk::Low);
    let level = if known == Risk::Low && !unknown.is_empty() {
        Risk::Unknown
    } else {
        known
    };
    RiskAssessment {
        level,
        reasons: reasons.into_iter().collect(),
        unknown_inputs: unknown.into_iter().collect(),
        policy_version: policy.version,
        policy_sha256: policy.sha256(),
        project_policy_state: policy.project_state,
    }
}

/// Within a pulsation the level only rises: the Integrator keeps the
/// highest of its own assessment and the Code assessment.
pub fn raise(current: RiskAssessment, earlier: Option<&RiskAssessment>) -> RiskAssessment {
    let Some(earlier) = earlier else {
        return current;
    };
    if rank(earlier.level) <= rank(current.level) {
        return current;
    }
    let mut raised = current;
    raised.level = earlier.level;
    let mut reasons = raised.reasons.into_iter().collect::<BTreeSet<_>>();
    reasons.extend(earlier.reasons.iter().cloned());
    raised.reasons = reasons.into_iter().collect();
    let mut unknown = raised.unknown_inputs.into_iter().collect::<BTreeSet<_>>();
    unknown.extend(earlier.unknown_inputs.iter().cloned());
    raised.unknown_inputs = unknown.into_iter().collect();
    raised
}

/// A check the level requires that no configured profile provides.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum MissingRiskCheck {
    CheckKind(String),
    CheckRef(Handle),
}

impl MissingRiskCheck {
    pub fn label(&self) -> String {
        match self {
            Self::CheckKind(kind) => format!("a `{kind}` check"),
            Self::CheckRef(handle) => format!("check `{handle}`"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RiskCheckSelection {
    /// Checks whose failure blocks the stage at this level.
    pub required: BTreeSet<Handle>,
    pub missing: Vec<MissingRiskCheck>,
}

const fn kind_name(kind: CheckKind) -> &'static str {
    match kind {
        CheckKind::Compile => "compile",
        CheckKind::Test => "test",
        CheckKind::Lint => "lint",
        CheckKind::Document => "document",
        CheckKind::Custom => "custom",
    }
}

/// The checks a level requires among the configured profiles, on top of
/// the ones the approved specification already made mandatory.
pub fn select_checks(
    level: Risk,
    profiles: &[ValidationCommandProfileV1],
    specification_required: &HashSet<Handle>,
    policy: &RiskPolicyV1,
) -> RiskCheckSelection {
    let level = selection_level(level);
    let configured = profiles
        .iter()
        .map(|profile| &profile.check_ref)
        .collect::<HashSet<_>>();
    let mut required = specification_required
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut missing = BTreeSet::new();
    if rank(level) >= rank(Risk::Medium) {
        required.extend(
            profiles
                .iter()
                .filter(|profile| profile.required)
                .map(|profile| profile.check_ref.clone()),
        );
        for check in &policy.extra_required_check_refs.medium {
            if configured.contains(check) {
                required.insert(check.clone());
            } else {
                missing.insert(MissingRiskCheck::CheckRef(check.clone()));
            }
        }
    }
    if level == Risk::High {
        for kind in [CheckKind::Compile, CheckKind::Test, CheckKind::Lint] {
            let of_kind = profiles
                .iter()
                .filter(|profile| profile.kind == kind)
                .map(|profile| profile.check_ref.clone())
                .collect::<Vec<_>>();
            if of_kind.is_empty() {
                missing.insert(MissingRiskCheck::CheckKind(kind_name(kind).to_owned()));
            }
            required.extend(of_kind);
        }
        for check in &policy.extra_required_check_refs.high {
            if configured.contains(check) {
                required.insert(check.clone());
            } else {
                missing.insert(MissingRiskCheck::CheckRef(check.clone()));
            }
        }
    }
    RiskCheckSelection {
        required,
        missing: missing.into_iter().collect(),
    }
}

/// Orders candidate paths for the diff budget: sensitive files first, then
/// the rest, each by path.
pub fn diff_priority(paths: &[String], policy: &RiskPolicyV1) -> Vec<String> {
    let sensitive = policy
        .sensitive_paths(paths.iter().map(String::as_str))
        .into_iter()
        .collect::<HashSet<_>>();
    let mut ordered = paths.to_vec();
    ordered.sort_by(|a, b| {
        let a_sensitive = sensitive.contains(&normalize_path(a));
        let b_sensitive = sensitive.contains(&normalize_path(b));
        b_sensitive.cmp(&a_sensitive).then_with(|| a.cmp(b))
    });
    ordered
}

// ------------------------------------------------------------ coverage

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageState {
    /// A fresh host receipt backs the row.
    Executed,
    /// Only the model's assessment backs the row.
    ModelOnly,
    /// The verdict is `unverified`, or the receipt is stale or unverifiable.
    Unverified,
    /// A check the risk level requires did not run or is not configured.
    NotRun,
}

impl CoverageState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Executed => "executed",
            Self::ModelOnly => "model_only",
            Self::Unverified => "unverified",
            Self::NotRun => "not_run",
        }
    }

    /// Rows that need an acknowledgement before a high-risk approval.
    pub const fn is_pending(self) -> bool {
        matches!(self, Self::Unverified | Self::NotRun)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageRowKind {
    Criterion,
    Check,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageRow {
    pub kind: CoverageRowKind,
    pub subject: Handle,
    pub state: CoverageState,
    /// The receipt status, the verdict or the reason, for display.
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageMatrix {
    pub level: Risk,
    pub rows: Vec<CoverageRow>,
    /// Check kinds the level requires that no profile provides.
    #[serde(default)]
    pub missing_kinds: Vec<String>,
}

impl CoverageMatrix {
    /// Rows left unverified or not run, plus required kinds nobody provides.
    pub fn pending(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.state.is_pending())
            .count()
            + self.missing_kinds.len()
    }

    /// Whether approving the review needs an explicit acknowledgement.
    pub fn needs_acknowledgement(&self) -> bool {
        self.level == Risk::High && self.pending() > 0
    }
}

/// What the host knows about one check when the matrix is built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckFact {
    pub check_ref: Handle,
    /// Receipt status (`passed`, `failed`, `not_run`, …).
    pub status: String,
    /// `fresh`, `stale`, `unverifiable` or not applicable.
    pub fresh: Option<bool>,
    /// Absent only for evidence under the older generic command contract.
    pub interpretation: Option<String>,
    pub result: Option<String>,
    pub limitations: Vec<String>,
}

impl CheckFact {
    fn fresh_executed(&self) -> bool {
        self.fresh == Some(true)
            && matches!(self.status.as_str(), "passed" | "failed")
            && match (self.interpretation.as_deref(), self.result.as_deref()) {
                (None, None) => true,
                (Some("supported"), Some(result)) => result == self.status,
                _ => false,
            }
    }

    fn inconclusive(&self) -> bool {
        !self.fresh_executed()
            && (self.interpretation.is_some()
                || self.result.is_some()
                || !matches!(
                    self.status.as_str(),
                    "passed" | "failed" | "not_run" | "unavailable"
                ))
    }

    fn detail(&self) -> String {
        let mut parts = vec![self.status.clone()];
        if let Some(interpretation) = &self.interpretation {
            parts.push(format!("interpretation: {interpretation}"));
        }
        if let Some(result) = &self.result {
            parts.push(format!("result: {result}"));
        }
        parts.extend(self.limitations.iter().take(8).cloned());
        parts.join("; ").chars().take(400).collect()
    }
}

/// The verdict the host kept for one criterion, after gap normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerdictFact {
    pub criterion: Handle,
    pub verdict: Verdict,
    /// Whether the host downgraded it for an evidence gap.
    pub downgraded: bool,
}

/// Builds the matrix from the kept verdicts, the criterion → check map and
/// the host's check facts. No model text enters it.
pub fn coverage_matrix(
    level: Risk,
    verdicts: &[VerdictFact],
    criterion_checks: &BTreeMap<Handle, BTreeSet<Handle>>,
    checks: &[CheckFact],
    selection: &RiskCheckSelection,
) -> CoverageMatrix {
    let by_ref = checks
        .iter()
        .map(|check| (&check.check_ref, check))
        .collect::<BTreeMap<_, _>>();
    let mut rows = Vec::new();
    for fact in verdicts {
        let mapped = criterion_checks.get(&fact.criterion);
        let executed = mapped
            .into_iter()
            .flatten()
            .filter_map(|check| by_ref.get(check))
            .filter(|check| check.fresh_executed())
            .collect::<Vec<_>>();
        let inconclusive = mapped
            .into_iter()
            .flatten()
            .filter_map(|check| by_ref.get(check))
            .filter(|check| check.inconclusive())
            .collect::<Vec<_>>();
        let (state, detail) = if fact.verdict == Verdict::Unverified {
            (
                CoverageState::Unverified,
                if fact.downgraded {
                    "unverified (downgraded by an evidence gap)".to_owned()
                } else {
                    "unverified".to_owned()
                },
            )
        } else if !inconclusive.is_empty() {
            (
                CoverageState::Unverified,
                inconclusive
                    .iter()
                    .map(|check| {
                        format!(
                            "{}: {} (not verified/fresh)",
                            check.check_ref,
                            check.detail()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
                    .chars()
                    .take(400)
                    .collect(),
            )
        } else if !executed.is_empty() {
            (
                CoverageState::Executed,
                executed
                    .iter()
                    .map(|check| format!("{}: {}", check.check_ref, check.detail()))
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        } else {
            (
                CoverageState::ModelOnly,
                match fact.verdict {
                    Verdict::Supported => "supported by the model's review only".to_owned(),
                    _ => "contradicted by the model's review".to_owned(),
                },
            )
        };
        rows.push(CoverageRow {
            kind: CoverageRowKind::Criterion,
            subject: fact.criterion.clone(),
            state,
            detail,
        });
    }
    for check in &selection.required {
        let (state, detail) = match by_ref.get(check) {
            Some(fact) if fact.fresh_executed() => (CoverageState::Executed, fact.detail()),
            Some(fact) if matches!(fact.status.as_str(), "not_run" | "unavailable") => {
                (CoverageState::NotRun, fact.detail())
            }
            Some(fact) => (
                CoverageState::Unverified,
                format!("{} (not verified/fresh)", fact.detail()),
            ),
            None => (CoverageState::NotRun, "no receipt".to_owned()),
        };
        rows.push(CoverageRow {
            kind: CoverageRowKind::Check,
            subject: check.clone(),
            state,
            detail,
        });
    }
    for missing in &selection.missing {
        if let MissingRiskCheck::CheckRef(check) = missing {
            if selection.required.contains(check) {
                continue;
            }
            rows.push(CoverageRow {
                kind: CoverageRowKind::Check,
                subject: check.clone(),
                state: CoverageState::NotRun,
                detail: "not configured".to_owned(),
            });
        }
    }
    rows.sort();
    rows.dedup();
    let missing_kinds = selection
        .missing
        .iter()
        .filter_map(|missing| match missing {
            MissingRiskCheck::CheckKind(kind) => Some(kind.clone()),
            MissingRiskCheck::CheckRef(_) => None,
        })
        .collect();
    CoverageMatrix {
        level,
        rows,
        missing_kinds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::SchemaV1;
    use crate::orchestration::composition::Uuid;

    fn h(text: &str) -> Handle {
        Handle::new(text).unwrap()
    }

    fn profile(check: &str, kind: CheckKind, required: bool) -> ValidationCommandProfileV1 {
        ValidationCommandProfileV1 {
            schema_version: SchemaV1,
            profile_id: Uuid::new("00000000-0000-4000-8000-000000000001").unwrap(),
            profile_version: 1,
            check_ref: h(check),
            kind,
            required,
            executable: "cargo".into(),
            argv: vec!["check".into()],
            cwd_relative_path: ".".into(),
            input_globs: vec!["**/*.rs".into()],
            environment_names: Vec::new(),
            toolchain_identity: "cargo".into(),
            timeout_seconds: 180,
            max_log_bytes: 65_536,
        }
    }

    fn inputs(intake: &[Risk], paths: &[&str]) -> RiskInputs {
        RiskInputs {
            intake: intake
                .iter()
                .enumerate()
                .map(|(index, risk)| (h(&format!("task_{}", index + 1)), *risk))
                .collect(),
            paths: paths.iter().map(|path| (*path).to_owned()).collect(),
            ..RiskInputs::default()
        }
    }

    #[test]
    fn a_note_declares_its_own_risk_in_front_matter() {
        assert_eq!(
            note_risk("---\ntype: feature\nrisk: high\n---\n# A\n"),
            Some(Risk::High)
        );
        assert_eq!(
            note_risk("---\r\nrisk: Medium\r\n---\r\n# A\r\n"),
            Some(Risk::Medium)
        );
        assert_eq!(note_risk("\u{feff}---\nrisk: low\n---\n"), Some(Risk::Low));
        // Boundary: no front matter, no key, a value outside the closed set,
        // a front matter that never closes and invalid YAML.
        assert_eq!(note_risk("# Title\nrisk: high\n"), None);
        assert_eq!(note_risk("---\ntype: feature\n---\n"), None);
        assert_eq!(note_risk("---\nrisk: catastrophic\n---\n"), None);
        assert_eq!(note_risk("---\nrisk: high\n"), None);
        assert_eq!(note_risk("---\nrisk: [high\n---\n"), None);
    }

    #[test]
    fn the_level_is_the_highest_signal_and_unknown_stays_visible() {
        let policy = RiskPolicyV1::builtin();
        let low = assess(&inputs(&[Risk::Low], &["src/lib.rs"]), &policy);
        assert_eq!(low.level, Risk::Low);
        assert!(low.reasons.is_empty());

        let medium = assess(
            &inputs(&[Risk::Low, Risk::Medium], &["src/lib.rs"]),
            &policy,
        );
        assert_eq!(medium.level, Risk::Medium);
        assert_eq!(medium.reasons[0].signal, RiskSignal::IntakeTask);
        assert_eq!(medium.reasons[0].subject, "task_2");

        // A sensitive path outranks everything the Intake said.
        let sensitive = assess(&inputs(&[Risk::Low], &["src\\Auth\\session.rs"]), &policy);
        assert_eq!(sensitive.level, Risk::High);
        assert_eq!(sensitive.reasons[0].signal, RiskSignal::SensitivePath);
        assert_eq!(sensitive.reasons[0].subject, "src/auth/session.rs");

        // `unknown` only decides when nothing known is above `low`.
        let unknown = assess(&inputs(&[Risk::Unknown, Risk::Low], &[]), &policy);
        assert_eq!(unknown.level, Risk::Unknown);
        assert_eq!(unknown.unknown_inputs, ["intake:task_1"]);
        let unknown_but_high = assess(&inputs(&[Risk::Unknown, Risk::High], &[]), &policy);
        assert_eq!(unknown_but_high.level, Risk::High);
        assert_eq!(unknown_but_high.unknown_inputs, ["intake:task_1"]);
        // A missing Intake is unknown, never a silent `low`.
        let missing = assess(&inputs(&[], &["src/lib.rs"]), &policy);
        assert_eq!(missing.level, Risk::Unknown);
        assert_eq!(missing.unknown_inputs, ["intake"]);
    }

    #[test]
    fn notes_interfaces_and_size_raise_the_level() {
        let policy = RiskPolicyV1::builtin();
        let mut facts = inputs(&[Risk::Low], &["src/lib.rs"]);
        facts.notes = vec![("knowledge/obsidian/features/a.md".into(), Risk::High)];
        assert_eq!(assess(&facts, &policy).level, Risk::High);
        facts.notes = vec![("knowledge/obsidian/features/a.md".into(), Risk::Unknown)];
        let note_unknown = assess(&facts, &policy);
        assert_eq!(note_unknown.level, Risk::Unknown);
        assert_eq!(
            note_unknown.unknown_inputs,
            ["note:knowledge/obsidian/features/a.md"]
        );

        let mut interface = inputs(&[Risk::Low], &["src/lib.rs"]);
        interface.changed_interfaces = vec!["api_session".into()];
        let changed = assess(&interface, &policy);
        assert_eq!(changed.level, Risk::Medium);
        assert_eq!(changed.reasons[0].signal, RiskSignal::InterfaceChange);

        // Boundary: exactly the threshold stays low; one more file is medium,
        // and the same file twice (separators, case) counts once.
        let at = (0..LARGE_CHANGE_FILES)
            .map(|index| format!("src/f{index}.rs"))
            .collect::<Vec<_>>();
        let at_refs = at.iter().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(
            assess(&inputs(&[Risk::Low], &at_refs), &policy).level,
            Risk::Low
        );
        let mut duplicated = at_refs.clone();
        duplicated.push("SRC\\f0.rs");
        assert_eq!(
            assess(&inputs(&[Risk::Low], &duplicated), &policy).level,
            Risk::Low
        );
        let mut over = at_refs.clone();
        over.push("src/extra.rs");
        let large = assess(&inputs(&[Risk::Low], &over), &policy);
        assert_eq!(large.level, Risk::Medium);
        assert_eq!(large.reasons[0].signal, RiskSignal::ChangeSize);
    }

    #[test]
    fn the_same_inputs_and_policy_give_the_same_assessment_in_any_order() {
        let policy = RiskPolicyV1::builtin();
        let mut forward = inputs(
            &[Risk::Medium, Risk::Unknown, Risk::High],
            &["migrations/V1.sql", "src/a.rs", "Dockerfile"],
        );
        forward.notes = vec![
            ("n/b.md".into(), Risk::Medium),
            ("n/a.md".into(), Risk::Unknown),
        ];
        forward.changed_interfaces = vec!["b".into(), "a".into()];
        let mut backward = forward.clone();
        backward.intake.reverse();
        backward.paths.reverse();
        backward.notes.reverse();
        backward.changed_interfaces.reverse();
        let first = assess(&forward, &policy);
        assert_eq!(first, assess(&backward, &policy));
        assert_eq!(
            canonical_sha256(&first, &[]).unwrap(),
            canonical_sha256(&assess(&forward, &policy), &[]).unwrap()
        );
        assert_eq!(first.policy_version, RISK_POLICY_VERSION);
        assert_eq!(first.policy_sha256, policy.sha256());
    }

    #[test]
    fn within_a_pulsation_the_level_never_falls() {
        let policy = RiskPolicyV1::builtin();
        let code = assess(&inputs(&[Risk::High], &[]), &policy);
        let integrator = assess(&inputs(&[Risk::Low], &["src/lib.rs"]), &policy);
        let raised = raise(integrator.clone(), Some(&code));
        assert_eq!(raised.level, Risk::High);
        assert!(
            raised
                .reasons
                .iter()
                .any(|r| r.signal == RiskSignal::IntakeTask)
        );
        // A higher current level is kept as is.
        let higher = assess(&inputs(&[Risk::Low], &["src/auth/a.rs"]), &policy);
        assert_eq!(raise(higher.clone(), Some(&integrator)), higher);
        assert_eq!(raise(integrator.clone(), None), integrator);
    }

    #[test]
    fn a_project_only_adds_to_the_builtin_policy() {
        let project = ProjectRiskPolicy {
            sensitive_path_globs: vec!["billing/**".into()],
            extra_required_check_refs: ExtraRequiredChecks {
                medium: vec![h("audit")],
                high: Vec::new(),
            },
        };
        let effective = RiskPolicyV1::effective(&project);
        assert_eq!(effective.project_state, ProjectPolicyState::ProjectApplied);
        assert!(
            effective
                .sensitive_path_globs
                .contains(&"billing/**".to_owned())
        );
        for builtin in BUILTIN_SENSITIVE_GLOBS {
            assert!(effective.sensitive_path_globs.iter().any(|g| g == builtin));
        }
        assert_ne!(effective.sha256(), RiskPolicyV1::builtin().sha256());
        assert_eq!(
            assess(&inputs(&[Risk::Low], &["billing/invoice.rs"]), &effective).level,
            Risk::High
        );
        assert_eq!(
            RiskPolicyV1::effective(&ProjectRiskPolicy::default()),
            RiskPolicyV1::builtin()
        );
        // Invalid additions never apply partially: the built-in rules stand.
        for invalid in [
            ProjectRiskPolicy {
                sensitive_path_globs: vec!["src/[".into()],
                ..ProjectRiskPolicy::default()
            },
            ProjectRiskPolicy {
                sensitive_path_globs: vec![" ".into()],
                ..ProjectRiskPolicy::default()
            },
            ProjectRiskPolicy {
                sensitive_path_globs: vec!["x".repeat(MAX_GLOB_CHARS + 1)],
                ..ProjectRiskPolicy::default()
            },
            ProjectRiskPolicy {
                sensitive_path_globs: (0..=MAX_PROJECT_GLOBS)
                    .map(|i| format!("d{i}/**"))
                    .collect(),
                ..ProjectRiskPolicy::default()
            },
            ProjectRiskPolicy {
                extra_required_check_refs: ExtraRequiredChecks {
                    medium: vec![h("audit"), h("audit")],
                    high: Vec::new(),
                },
                ..ProjectRiskPolicy::default()
            },
        ] {
            assert!(invalid.validate().is_err(), "{invalid:?}");
            let fallback = RiskPolicyV1::effective(&invalid);
            assert_eq!(
                fallback.project_state,
                ProjectPolicyState::ProjectPolicyInvalid
            );
            assert_eq!(
                fallback.sensitive_path_globs,
                RiskPolicyV1::builtin().sensitive_path_globs
            );
        }
    }

    #[test]
    fn the_level_selects_required_checks_among_configured_profiles() {
        let policy = RiskPolicyV1::effective(&ProjectRiskPolicy {
            extra_required_check_refs: ExtraRequiredChecks {
                medium: vec![h("audit")],
                high: vec![h("bench")],
            },
            ..ProjectRiskPolicy::default()
        });
        let profiles = [
            profile("build", CheckKind::Compile, false),
            profile("unit", CheckKind::Test, true),
            profile("audit", CheckKind::Custom, false),
        ];
        let specification = HashSet::from([h("docs")]);

        let low = select_checks(Risk::Low, &profiles, &specification, &policy);
        assert_eq!(low.required, BTreeSet::from([h("docs")]));
        assert!(low.missing.is_empty());

        let medium = select_checks(Risk::Medium, &profiles, &specification, &policy);
        assert_eq!(
            medium.required,
            BTreeSet::from([h("audit"), h("docs"), h("unit")])
        );
        assert!(medium.missing.is_empty());
        // `unknown` selects like `medium`.
        assert_eq!(
            select_checks(Risk::Unknown, &profiles, &specification, &policy),
            medium
        );

        let high = select_checks(Risk::High, &profiles, &specification, &policy);
        assert_eq!(
            high.required,
            BTreeSet::from([h("audit"), h("build"), h("docs"), h("unit")])
        );
        assert_eq!(
            high.missing,
            [
                MissingRiskCheck::CheckKind("lint".into()),
                MissingRiskCheck::CheckRef(h("bench")),
            ]
        );
        // Boundary: no profile at all; the level never invents a check.
        let none = select_checks(Risk::High, &[], &HashSet::new(), &RiskPolicyV1::builtin());
        assert!(none.required.is_empty());
        assert_eq!(none.missing.len(), 3);
    }

    #[test]
    fn sensitive_files_take_the_diff_budget_first() {
        let policy = RiskPolicyV1::builtin();
        let ordered = diff_priority(
            &[
                "src/z.rs".into(),
                "src/auth/login.rs".into(),
                "README.md".into(),
                "migrations/V2.sql".into(),
            ],
            &policy,
        );
        assert_eq!(
            ordered,
            [
                "migrations/V2.sql",
                "src/auth/login.rs",
                "README.md",
                "src/z.rs"
            ]
        );
        assert!(diff_priority(&[], &policy).is_empty());
    }

    #[test]
    fn the_coverage_matrix_separates_executed_model_only_and_unverified() {
        let criterion_checks = BTreeMap::from([
            (h("crit_1_1"), BTreeSet::from([h("unit")])),
            (h("crit_1_2"), BTreeSet::from([h("lint")])),
        ]);
        let checks = [
            CheckFact {
                check_ref: h("unit"),
                status: "passed".into(),
                fresh: Some(true),
                interpretation: None,
                result: None,
                limitations: Vec::new(),
            },
            CheckFact {
                check_ref: h("lint"),
                status: "passed".into(),
                fresh: Some(false),
                interpretation: None,
                result: None,
                limitations: Vec::new(),
            },
            CheckFact {
                check_ref: h("build"),
                status: "not_run".into(),
                fresh: None,
                interpretation: None,
                result: None,
                limitations: Vec::new(),
            },
        ];
        let verdicts = [
            VerdictFact {
                criterion: h("crit_1_1"),
                verdict: Verdict::Supported,
                downgraded: false,
            },
            VerdictFact {
                criterion: h("crit_1_2"),
                verdict: Verdict::Supported,
                downgraded: false,
            },
            VerdictFact {
                criterion: h("crit_1_3"),
                verdict: Verdict::Unverified,
                downgraded: true,
            },
        ];
        let selection = RiskCheckSelection {
            required: BTreeSet::from([h("build"), h("unit")]),
            missing: vec![
                MissingRiskCheck::CheckKind("lint".into()),
                MissingRiskCheck::CheckRef(h("bench")),
            ],
        };
        let matrix = coverage_matrix(
            Risk::High,
            &verdicts,
            &criterion_checks,
            &checks,
            &selection,
        );
        let state = |subject: &str| {
            matrix
                .rows
                .iter()
                .find(|row| row.subject.as_str() == subject)
                .map(|row| row.state)
                .unwrap()
        };
        assert_eq!(state("crit_1_1"), CoverageState::Executed);
        // A stale receipt does not back a criterion: only the model did.
        assert_eq!(state("crit_1_2"), CoverageState::ModelOnly);
        assert_eq!(state("crit_1_3"), CoverageState::Unverified);
        assert_eq!(state("unit"), CoverageState::Executed);
        assert_eq!(state("build"), CoverageState::NotRun);
        assert_eq!(state("bench"), CoverageState::NotRun);
        assert_eq!(matrix.missing_kinds, ["lint"]);
        // crit_1_3, build, bench and the missing lint kind.
        assert_eq!(matrix.pending(), 4);
        assert!(matrix.needs_acknowledgement());

        // Only a high level asks for an acknowledgement.
        let medium = coverage_matrix(
            Risk::Medium,
            &verdicts,
            &criterion_checks,
            &checks,
            &selection,
        );
        assert!(!medium.needs_acknowledgement());
        // Nothing pending: no acknowledgement even when high.
        let clean = coverage_matrix(
            Risk::High,
            &verdicts[..1],
            &criterion_checks,
            &checks,
            &RiskCheckSelection {
                required: BTreeSet::from([h("unit")]),
                missing: Vec::new(),
            },
        );
        assert_eq!(clean.pending(), 0);
        assert!(!clean.needs_acknowledgement());
    }

    #[test]
    fn coverage_requires_completed_interpreted_evidence_and_keeps_failed_execution() {
        for (status, interpretation, result, fresh, expected) in [
            ("passed", None, None, Some(true), CoverageState::Executed),
            ("failed", None, None, Some(true), CoverageState::Executed),
            (
                "passed",
                Some("supported"),
                Some("passed"),
                Some(true),
                CoverageState::Executed,
            ),
            (
                "failed",
                Some("supported"),
                Some("failed"),
                Some(true),
                CoverageState::Executed,
            ),
            ("error", None, None, Some(true), CoverageState::Unverified),
            (
                "timed_out",
                None,
                None,
                Some(true),
                CoverageState::Unverified,
            ),
            (
                "cancelled",
                None,
                None,
                Some(true),
                CoverageState::Unverified,
            ),
            ("running", None, None, Some(true), CoverageState::Unverified),
            (
                "passed",
                Some("partial"),
                Some("unknown"),
                Some(true),
                CoverageState::Unverified,
            ),
            (
                "passed",
                Some("uninterpreted"),
                Some("unknown"),
                Some(true),
                CoverageState::Unverified,
            ),
            (
                "passed",
                Some("supported"),
                Some("unknown"),
                Some(true),
                CoverageState::Unverified,
            ),
            (
                "passed",
                Some("supported"),
                Some("failed"),
                Some(true),
                CoverageState::Unverified,
            ),
            (
                "failed",
                Some("supported"),
                Some("passed"),
                Some(true),
                CoverageState::Unverified,
            ),
            (
                "passed",
                Some("supported"),
                Some("passed"),
                Some(false),
                CoverageState::Unverified,
            ),
        ] {
            let checks = [CheckFact {
                check_ref: h("unit"),
                status: status.into(),
                fresh,
                interpretation: interpretation.map(str::to_owned),
                result: result.map(str::to_owned),
                limitations: vec!["bounded execution scope".into()],
            }];
            let matrix = coverage_matrix(
                Risk::High,
                &[VerdictFact {
                    criterion: h("crit_1_1"),
                    verdict: Verdict::Supported,
                    downgraded: false,
                }],
                &BTreeMap::from([(h("crit_1_1"), BTreeSet::from([h("unit")]))]),
                &checks,
                &RiskCheckSelection {
                    required: BTreeSet::from([h("unit")]),
                    missing: Vec::new(),
                },
            );
            let check = matrix
                .rows
                .iter()
                .find(|row| row.subject == h("unit"))
                .unwrap();
            assert_eq!(check.state, expected, "{checks:?}");
            let criterion = matrix
                .rows
                .iter()
                .find(|row| row.subject == h("crit_1_1"))
                .unwrap();
            assert_eq!(criterion.state, expected, "{checks:?}");
            assert_eq!(
                matrix.needs_acknowledgement(),
                expected == CoverageState::Unverified
            );
            assert!(check.detail.contains("bounded execution scope"));
            if let Some(interpretation) = interpretation {
                assert!(check.detail.contains(interpretation));
            }
        }
    }

    #[test]
    fn one_executed_check_cannot_hide_other_mapped_qualified_inconclusive_evidence() {
        let good = CheckFact {
            check_ref: h("good"),
            status: "passed".into(),
            fresh: Some(true),
            interpretation: Some("supported".into()),
            result: Some("passed".into()),
            limitations: Vec::new(),
        };
        for (interpretation, result, fresh) in [
            ("partial", "unknown", Some(true)),
            ("uninterpreted", "unknown", Some(true)),
            ("supported", "unknown", Some(true)),
            ("supported", "passed", Some(false)),
        ] {
            let inconclusive = CheckFact {
                check_ref: h("optional"),
                status: "passed".into(),
                fresh,
                interpretation: Some(interpretation.into()),
                result: Some(result.into()),
                limitations: vec!["inconclusive inputs".into()],
            };
            let matrix = coverage_matrix(
                Risk::High,
                &[VerdictFact {
                    criterion: h("crit_1_1"),
                    verdict: Verdict::Supported,
                    downgraded: false,
                }],
                &BTreeMap::from([(h("crit_1_1"), BTreeSet::from([h("good"), h("optional")]))]),
                &[good.clone(), inconclusive],
                &RiskCheckSelection {
                    // The optional check has no independent required-check row.
                    required: BTreeSet::from([h("good")]),
                    missing: Vec::new(),
                },
            );
            let criterion = matrix
                .rows
                .iter()
                .find(|row| row.subject == h("crit_1_1"))
                .unwrap();
            assert_eq!(criterion.state, CoverageState::Unverified);
            assert!(criterion.detail.contains("optional: passed"));
            assert!(criterion.detail.contains("inconclusive inputs"));
            assert_eq!(matrix.pending(), 1);
            assert!(matrix.needs_acknowledgement());
        }
    }
}
