//! Feature 08: project git rules (`commit-rules-v1`), read-only git/gh status,
//! and host git plan/receipt contracts.
//!
//! All user-facing messages here use plain English without literal backticks.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub const COMMIT_RULES_SCHEMA_VERSION: &str = "commit-rules-v1";
pub const REQUIRED_MESSAGE_LANGUAGE: &str = "en";
pub const MAX_PATTERN_CHARS: usize = 500;
pub const MAX_LIST_ENTRIES: usize = 100;
pub const MAX_ENTRY_CHARS: usize = 200;

pub const DEFAULT_LOCKFILE_EXCLUDES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
    "Pipfile.lock",
    "uv.lock",
    "Gemfile.lock",
    "composer.lock",
    "go.sum",
    "flake.lock",
];

pub const DEFAULT_FORBIDDEN_PATHS: &[&str] = &[
    ".env",
    ".env.*",
    "**/*.pem",
    "**/*.key",
    "**/*.p12",
    "**/*.pfx",
    "**/id_rsa*",
];

pub const DEFAULT_PROTECTED_BRANCHES: &[&str] = &["main", "master", "develop", "release/*"];

pub const DEFAULT_COMMIT_TYPES: &[&str] = &[
    "feat", "fix", "refactor", "perf", "test", "docs", "chore", "build", "ci",
];

pub const REQUIRED_TASK_KINDS: &[&str] = &[
    "Feature",
    "Bug",
    "Refactor",
    "Improvement",
    "Adjustment",
    "Foundation",
];

pub mod violation_codes {
    // Eligibility
    pub const TOOL_DISABLED: &str = "tool_disabled";
    pub const PULSATION_NOT_DONE: &str = "pulsation_not_done";
    pub const EARLIER_PULSATION_PENDING: &str = "earlier_pulsation_pending";
    pub const NOT_A_GIT_REPOSITORY: &str = "not_a_git_repository";
    pub const DETACHED_HEAD: &str = "detached_head";
    pub const OPERATION_IN_PROGRESS: &str = "operation_in_progress";
    pub const INDEX_LOCKED: &str = "index_locked";
    pub const IDENTITY_MISSING: &str = "identity_missing";

    // Content
    pub const BASE_MISMATCH: &str = "base_mismatch";
    pub const CONTENT_HASH_MISMATCH: &str = "content_hash_mismatch";
    pub const FORBIDDEN_PATH: &str = "forbidden_path";
    pub const SECRET_DETECTED: &str = "secret_detected";
    pub const BINARY_FILE: &str = "binary_file";
    pub const DELETION_UNSUPPORTED: &str = "deletion_unsupported";
    pub const RENAME_UNSUPPORTED: &str = "rename_unsupported";
    pub const SYMLINK_PATH: &str = "symlink_path";
    pub const SUBMODULE_PATH: &str = "submodule_path";
    pub const LFS_PATH_UNSUPPORTED: &str = "lfs_path_unsupported";

    // Size & consistency
    pub const TOO_MANY_LINES: &str = "too_many_lines";
    pub const TOO_MANY_FILES: &str = "too_many_files";
    pub const MISSING_TESTS: &str = "missing_tests";

    // Message
    pub const MESSAGE_TYPE_NOT_ALLOWED: &str = "message_type_not_allowed";
    pub const SUBJECT_TOO_LONG: &str = "subject_too_long";
    pub const BODY_MISSING: &str = "body_missing";
    pub const PATTERN_MISMATCH: &str = "pattern_mismatch";

    // Branch & remote
    pub const PROTECTED_BRANCH: &str = "protected_branch";
    pub const BRANCH_EXISTS: &str = "branch_exists";
    pub const REMOTE_MISSING: &str = "remote_missing";
    pub const PUSH_REJECTED: &str = "push_rejected";
    pub const CREDENTIALS_MISSING: &str = "credentials_missing";

    // Pull
    pub const PULSATION_ACTIVE: &str = "pulsation_active";
    pub const AUTOMATION_RUNNING: &str = "automation_running";
    pub const LOCAL_CHANGES_WOULD_BE_OVERWRITTEN: &str = "local_changes_would_be_overwritten";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageFormat {
    Conventional,
    Free,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageScope {
    FeatureSlug,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitGranularity {
    PerTask,
    PerPulsation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    Block,
    Warn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocsPlacement {
    SeparateCommit,
    WithTasks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchMode {
    PerPulsation,
    CurrentBranch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullMode {
    FfOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitMessageRules {
    pub format: MessageFormat,
    pub types: Vec<String>,
    pub type_by_task_kind: BTreeMap<String, String>,
    pub scope: MessageScope,
    pub subject_max_chars: u32,
    pub body_required: bool,
    pub body_wrap: u32,
    pub pattern: String,
    pub language: String,
    pub trailers: bool,
    pub sign_off: bool,
}

impl Default for CommitMessageRules {
    fn default() -> Self {
        let mut type_by_task_kind = BTreeMap::new();
        type_by_task_kind.insert("Feature".into(), "feat".into());
        type_by_task_kind.insert("Bug".into(), "fix".into());
        type_by_task_kind.insert("Refactor".into(), "refactor".into());
        type_by_task_kind.insert("Improvement".into(), "feat".into());
        type_by_task_kind.insert("Adjustment".into(), "chore".into());
        type_by_task_kind.insert("Foundation".into(), "chore".into());
        Self {
            format: MessageFormat::Conventional,
            types: DEFAULT_COMMIT_TYPES
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            type_by_task_kind,
            scope: MessageScope::FeatureSlug,
            subject_max_chars: 72,
            body_required: true,
            body_wrap: 72,
            pattern: String::new(),
            language: REQUIRED_MESSAGE_LANGUAGE.to_owned(),
            trailers: true,
            sign_off: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitLimitsRules {
    pub max_changed_lines: u32,
    pub max_files: u32,
    pub on_exceed: RuleAction,
    pub exclude_from_count: Vec<String>,
}

impl Default for CommitLimitsRules {
    fn default() -> Self {
        Self {
            max_changed_lines: 400,
            max_files: 30,
            on_exceed: RuleAction::Block,
            exclude_from_count: DEFAULT_LOCKFILE_EXCLUDES
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitConsistencyRules {
    pub missing_tests: RuleAction,
}

impl Default for CommitConsistencyRules {
    fn default() -> Self {
        Self {
            missing_tests: RuleAction::Block,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitPathsRules {
    pub forbidden: Vec<String>,
}

impl Default for CommitPathsRules {
    fn default() -> Self {
        Self {
            forbidden: DEFAULT_FORBIDDEN_PATHS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitBranchRules {
    pub mode: BranchMode,
    pub pattern: String,
    pub protected: Vec<String>,
}

impl Default for CommitBranchRules {
    fn default() -> Self {
        Self {
            mode: BranchMode::PerPulsation,
            pattern: "ikore/{pulsationShort}-{slug}".to_owned(),
            protected: DEFAULT_PROTECTED_BRANCHES
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitPushRules {
    pub remote: String,
    pub set_upstream: bool,
}

impl Default for CommitPushRules {
    fn default() -> Self {
        Self {
            remote: "origin".to_owned(),
            set_upstream: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitPullRules {
    pub remote: String,
    pub mode: PullMode,
}

impl Default for CommitPullRules {
    fn default() -> Self {
        Self {
            remote: "origin".to_owned(),
            mode: PullMode::FfOnly,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitRulesV1 {
    pub schema_version: String,
    pub message: CommitMessageRules,
    pub granularity: CommitGranularity,
    pub limits: CommitLimitsRules,
    pub docs_placement: DocsPlacement,
    pub consistency: CommitConsistencyRules,
    pub paths: CommitPathsRules,
    pub branch: CommitBranchRules,
    pub push: CommitPushRules,
    pub pull: CommitPullRules,
}

impl Default for CommitRulesV1 {
    fn default() -> Self {
        Self {
            schema_version: COMMIT_RULES_SCHEMA_VERSION.to_owned(),
            message: CommitMessageRules::default(),
            granularity: CommitGranularity::PerTask,
            limits: CommitLimitsRules::default(),
            docs_placement: DocsPlacement::SeparateCommit,
            consistency: CommitConsistencyRules::default(),
            paths: CommitPathsRules::default(),
            branch: CommitBranchRules::default(),
            push: CommitPushRules::default(),
            pull: CommitPullRules::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedGitRule {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
}

const FIXED_HOST_RULES: &[FixedGitRule] = &[
    FixedGitRule {
        id: "no_force_or_tag_push",
        title: "No force push or remote deletion",
        description: "Force push, refspecs with plus, deleting remote refs, and pushing tags, mirror, or all refs are always blocked.",
    },
    FixedGitRule {
        id: "no_protected_branch_writes",
        title: "Protected branches stay untouched",
        description: "The host never commits or pushes directly to a protected branch.",
    },
    FixedGitRule {
        id: "no_history_rewrite",
        title: "No history rewriting",
        description: "Amend, rebase, reset, and filter operations are never run by the host.",
    },
    FixedGitRule {
        id: "no_co_authored_by",
        title: "Developer identity only",
        description: "Commits use the local git user identity and never add Co-authored-by trailers.",
    },
    FixedGitRule {
        id: "no_repo_hooks",
        title: "Repository hooks do not run",
        description: "Commits are built with git plumbing so project checks live in host rules instead of repository hooks.",
    },
    FixedGitRule {
        id: "explicit_paths_only",
        title: "Explicit planned paths only",
        description: "Broad staging commands like git add -A or git add . are never used.",
    },
    FixedGitRule {
        id: "argv_only_no_shell",
        title: "Direct executable launch without a shell",
        description: "Git and GitHub CLI run from canonical executable paths with argument vectors, never via a shell.",
    },
    FixedGitRule {
        id: "no_prompt_or_injected_creds",
        title: "No credential injection or interactive prompts",
        description: "GIT_TERMINAL_PROMPT=0 and GCM_INTERACTIVE=never are always set, and the host never supplies credentials.",
    },
    FixedGitRule {
        id: "done_pulsations_strict_order",
        title: "Done pulsations in completion order",
        description: "Only pulsations accepted after Integrator review (Done) are eligible, strictly in completion order.",
    },
    FixedGitRule {
        id: "secret_scan_required",
        title: "Deterministic secret scan before commit",
        description: "Every committed blob is scanned with the host secret redactor; any match blocks the commit.",
    },
    FixedGitRule {
        id: "no_binary_files",
        title: "Binary files are excluded",
        description: "Binary content is never staged into a host-managed commit.",
    },
    FixedGitRule {
        id: "no_secrets_to_models",
        title: "Credentials never reach models",
        description: "Tokens, remote URL passwords, and git credential settings are scrubbed before status display or model handoff.",
    },
];

pub fn fixed_git_rules() -> &'static [FixedGitRule] {
    FIXED_HOST_RULES
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleFieldError {
    pub field: String,
    pub code: String,
    pub message: String,
}

impl RuleFieldError {
    pub fn new(
        field: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            field: field.into(),
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleViolation {
    pub code: String,
    pub path: Option<String>,
    pub task_id: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolBinaryStatus {
    pub available: bool,
    pub executable_path: Option<String>,
    pub sha256: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusV1 {
    pub workspace_path: String,
    pub in_git_repository: bool,
    pub repository_root: Option<String>,
    pub git: ToolBinaryStatus,
    pub gh: ToolBinaryStatus,
    pub user_name: Option<String>,
    pub user_email: Option<String>,
    pub current_branch: Option<String>,
    pub detached_head: bool,
    pub remote_name: String,
    pub remote_url: Option<String>,
    pub credential_helper: Option<String>,
    pub gh_authenticated: bool,
    pub gh_account: Option<String>,
    pub gh_status_summary: String,
    pub setup_hints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectGitRulesRecord {
    pub project_id: String,
    pub version: i64,
    pub enabled: bool,
    pub rules: CommitRulesV1,
    pub rules_sha256: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveProjectGitRulesRequest {
    pub project_id: String,
    pub enabled: bool,
    pub rules: CommitRulesV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SaveProjectGitRulesOutcome {
    Saved(Box<ProjectGitRulesRecord>),
    Invalid(Vec<RuleFieldError>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitPlanGroupV1 {
    pub task_ids: Vec<String>,
    pub commit_type: String,
    pub scope: Option<String>,
    pub subject: String,
    pub body: String,
    pub trailers: Vec<String>,
    pub paths: Vec<String>,
    pub counted_files: usize,
    pub counted_lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitPlanV1 {
    pub project_id: String,
    pub pulsation_id: String,
    pub branch: String,
    pub base_head: String,
    pub groups: Vec<CommitPlanGroupV1>,
    pub warnings: Vec<RuleViolation>,
    pub blocking_violations: Vec<RuleViolation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitReceiptEntryV1 {
    pub sha: String,
    pub message: String,
    pub files: Vec<String>,
    pub lines: usize,
    pub task_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitReceiptV1 {
    pub project_id: String,
    pub pulsation_id: String,
    pub branch: String,
    pub base_head: String,
    pub commits: Vec<CommitReceiptEntryV1>,
    pub rules_version: i64,
    pub rules_sha256: String,
    pub git_version: String,
    pub warnings: Vec<RuleViolation>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PushReceiptV1 {
    pub project_id: String,
    pub branch: String,
    pub remote: String,
    pub set_upstream: bool,
    pub summary: String,
    pub rules_version: i64,
    pub rules_sha256: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PullReceiptV1 {
    pub project_id: String,
    pub branch: String,
    pub remote: String,
    pub fast_forwarded: bool,
    pub summary: String,
    pub rules_version: i64,
    pub rules_sha256: String,
    pub duration_ms: u64,
}

pub fn validate_commit_rules_structure(rules: &CommitRulesV1) -> Vec<RuleFieldError> {
    let mut errors = Vec::new();
    if rules.schema_version.trim() != COMMIT_RULES_SCHEMA_VERSION {
        errors.push(RuleFieldError::new(
            "schemaVersion",
            "invalid_schema_version",
            format!("Schema version must be {COMMIT_RULES_SCHEMA_VERSION}."),
        ));
    }
    validate_message_rules(&rules.message, &mut errors);
    validate_limits_rules(&rules.limits, &mut errors);
    validate_string_list(
        "paths.forbidden",
        &rules.paths.forbidden,
        false,
        &mut errors,
    );
    validate_branch_rules(&rules.branch, &mut errors);
    validate_remote_name("push.remote", &rules.push.remote, &mut errors);
    validate_remote_name("pull.remote", &rules.pull.remote, &mut errors);
    errors
}

fn validate_message_rules(message: &CommitMessageRules, errors: &mut Vec<RuleFieldError>) {
    if message.language.trim() != REQUIRED_MESSAGE_LANGUAGE {
        errors.push(RuleFieldError::new(
            "message.language",
            "fixed_language_en",
            "Commit message language is fixed to English (en) in this version.",
        ));
    }
    if message.types.is_empty() || message.types.len() > MAX_LIST_ENTRIES {
        errors.push(RuleFieldError::new(
            "message.types",
            "invalid_types_list",
            "Provide between 1 and 100 allowed commit types.",
        ));
    }
    let mut allowed = BTreeSet::new();
    for ty in &message.types {
        let trimmed = ty.trim();
        if trimmed.is_empty()
            || trimmed.len() > 32
            || !trimmed
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        {
            errors.push(RuleFieldError::new(
                "message.types",
                "invalid_commit_type",
                "Commit types may contain only ASCII letters, digits, hyphens, and underscores.",
            ));
            break;
        }
        allowed.insert(trimmed.to_owned());
    }
    for kind in REQUIRED_TASK_KINDS {
        match message.type_by_task_kind.get(*kind) {
            None => errors.push(RuleFieldError::new(
                format!("message.typeByTaskKind.{kind}"),
                "missing_task_kind_mapping",
                format!("Task kind {kind} must map to an allowed commit type."),
            )),
            Some(mapped) if !allowed.contains(mapped.trim()) => errors.push(RuleFieldError::new(
                format!("message.typeByTaskKind.{kind}"),
                "unknown_mapped_type",
                format!("Commit type mapped from {kind} must appear in message.types."),
            )),
            _ => {}
        }
    }
    if !(20..=200).contains(&message.subject_max_chars) {
        errors.push(RuleFieldError::new(
            "message.subjectMaxChars",
            "subject_max_out_of_range",
            "Subject max length must be between 20 and 200 characters.",
        ));
    }
    if !(40..=200).contains(&message.body_wrap) {
        errors.push(RuleFieldError::new(
            "message.bodyWrap",
            "body_wrap_out_of_range",
            "Body wrap width must be between 40 and 200 characters.",
        ));
    }
    if message.pattern.chars().count() > MAX_PATTERN_CHARS || message.pattern.contains('\0') {
        errors.push(RuleFieldError::new(
            "message.pattern",
            "pattern_too_long",
            "Message pattern must be at most 500 characters with no NUL bytes.",
        ));
    }
}

fn validate_limits_rules(limits: &CommitLimitsRules, errors: &mut Vec<RuleFieldError>) {
    if !(1..=50_000).contains(&limits.max_changed_lines) {
        errors.push(RuleFieldError::new(
            "limits.maxChangedLines",
            "max_lines_out_of_range",
            "Maximum changed lines per commit must be between 1 and 50000.",
        ));
    }
    if !(1..=1_000).contains(&limits.max_files) {
        errors.push(RuleFieldError::new(
            "limits.maxFiles",
            "max_files_out_of_range",
            "Maximum files per commit must be between 1 and 1000.",
        ));
    }
    validate_string_list(
        "limits.excludeFromCount",
        &limits.exclude_from_count,
        true,
        errors,
    );
}

fn validate_branch_rules(branch: &CommitBranchRules, errors: &mut Vec<RuleFieldError>) {
    let pattern = branch.pattern.trim();
    if pattern.is_empty()
        || pattern.len() > MAX_ENTRY_CHARS
        || pattern.chars().any(char::is_whitespace)
        || pattern.contains("..")
        || pattern.contains('\0')
        || pattern.starts_with('/')
        || pattern.ends_with('/')
    {
        errors.push(RuleFieldError::new(
            "branch.pattern",
            "invalid_branch_pattern",
            "Branch pattern must be a valid git ref pattern without whitespace or double dots.",
        ));
    } else {
        // Check placeholders inside `{...}` match the three supported tokens.
        let mut cursor = pattern;
        while let Some(open) = cursor.find('{') {
            let after_open = &cursor[open + 1..];
            let Some(close) = after_open.find('}') else {
                errors.push(RuleFieldError::new(
                    "branch.pattern",
                    "unclosed_placeholder",
                    "Branch pattern contains an unclosed placeholder brace.",
                ));
                break;
            };
            let token = &after_open[..close];
            if !matches!(token, "pulsationShort" | "slug" | "date") {
                errors.push(RuleFieldError::new(
                    "branch.pattern",
                    "unknown_placeholder",
                    "Branch pattern supports only {pulsationShort}, {slug}, and {date} placeholders.",
                ));
                break;
            }
            cursor = &after_open[close + 1..];
        }
    }
    validate_string_list("branch.protected", &branch.protected, true, errors);
}

fn validate_string_list(
    field: &str,
    items: &[String],
    allow_empty_list: bool,
    errors: &mut Vec<RuleFieldError>,
) {
    if !allow_empty_list && items.is_empty() {
        errors.push(RuleFieldError::new(
            field,
            "empty_list",
            "This list cannot be empty.",
        ));
        return;
    }
    if items.len() > MAX_LIST_ENTRIES {
        errors.push(RuleFieldError::new(
            field,
            "too_many_entries",
            "List cannot have more than 100 entries.",
        ));
        return;
    }
    for entry in items {
        let trimmed = entry.trim();
        if trimmed.is_empty() || trimmed.len() > MAX_ENTRY_CHARS || trimmed.contains('\0') {
            errors.push(RuleFieldError::new(
                field,
                "invalid_list_entry",
                "Each entry must be non-empty, at most 200 characters, and contain no NUL bytes.",
            ));
            return;
        }
    }
}

fn validate_remote_name(field: &str, remote: &str, errors: &mut Vec<RuleFieldError>) {
    let trimmed = remote.trim();
    if trimmed.is_empty()
        || trimmed.len() > 64
        || trimmed.starts_with('-')
        || trimmed.chars().any(char::is_whitespace)
        || trimmed.contains('\0')
        || trimmed.contains("://")
    {
        errors.push(RuleFieldError::new(
            field,
            "invalid_remote_name",
            "Remote must be a valid git remote name such as origin.",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rules_pass_structural_validation() {
        let rules = CommitRulesV1::default();
        assert!(validate_commit_rules_structure(&rules).is_empty());
        assert_eq!(rules.schema_version, COMMIT_RULES_SCHEMA_VERSION);
        assert_eq!(rules.limits.exclude_from_count.len(), 12);
    }

    #[test]
    fn fixed_rules_contain_no_literal_backticks_and_cover_all_twelve() {
        let rules = fixed_git_rules();
        assert_eq!(rules.len(), 12);
        for rule in rules {
            assert!(!rule.title.contains('`'), "backtick in {}", rule.id);
            assert!(!rule.description.contains('`'), "backtick in {}", rule.id);
        }
    }

    #[test]
    fn structural_validator_catches_invalid_fields() {
        let mut rules = CommitRulesV1 {
            schema_version: "wrong".into(),
            ..Default::default()
        };
        rules.message.language = "pt".into();
        rules.message.subject_max_chars = 10;
        rules.message.body_wrap = 10;
        rules.limits.max_changed_lines = 0;
        rules.limits.max_files = 5000;
        rules.branch.pattern = "ikore/{unknown}".into();
        rules.push.remote = "--upload-pack=bad".into();
        rules.pull.remote = " ".into();
        let errors = validate_commit_rules_structure(&rules);
        let fields: BTreeSet<&str> = errors.iter().map(|e| e.field.as_str()).collect();
        for expected in [
            "schemaVersion",
            "message.language",
            "message.subjectMaxChars",
            "message.bodyWrap",
            "limits.maxChangedLines",
            "limits.maxFiles",
            "branch.pattern",
            "push.remote",
            "pull.remote",
        ] {
            assert!(
                fields.contains(expected),
                "missing error for {expected}: {errors:?}"
            );
        }
    }
}
