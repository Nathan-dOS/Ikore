//! Pure commit plan & rule validator for Feature 08 M2.
//!
//! Validates every blob, group, message, branch, secret scan, and host restriction
//! upfront and returns all warnings and blocking violations in deterministic order.

use std::collections::{BTreeMap, BTreeSet};

use globset::{GlobBuilder, GlobMatcher};
use ikore_contracts::git_tool::{
    BranchMode, CommitPlanV1, CommitRulesV1, MessageFormat, RuleAction, RuleViolation,
    format_commit_message, violation_codes,
};
use regex::Regex;

use crate::secret_redactor::{MAX_SANITIZATION_BYTES, SecretRedactor};

#[derive(Debug, Clone)]
pub struct PlannedBlob {
    pub workspace_rel_path: String,
    pub repo_path: String,
    pub content_bytes: Vec<u8>,
    pub base_sha256: Option<String>,
    pub desired_sha256: String,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub excluded_from_count: bool,
    pub touches_code: bool,
    pub is_test_file: bool,
    pub is_binary: bool,
    pub is_deleted: bool,
    pub is_renamed: bool,
    pub is_symlink: bool,
    pub is_submodule: bool,
    pub is_lfs: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PlanValidationOutcome {
    pub warnings: Vec<RuleViolation>,
    pub blocking_violations: Vec<RuleViolation>,
}

pub fn compile_path_matchers(patterns: &[String]) -> Result<Vec<(String, GlobMatcher)>, String> {
    let mut compiled = Vec::with_capacity(patterns.len());
    for raw in patterns {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = trimmed.replace('\\', "/");
        let glob = GlobBuilder::new(&normalized)
            .literal_separator(false)
            .build()
            .map_err(|error| format!("Invalid glob pattern {trimmed}: {error}"))?;
        compiled.push((trimmed.to_owned(), glob.compile_matcher()));
    }
    Ok(compiled)
}

pub fn path_matches_any(path: &str, matchers: &[(String, GlobMatcher)]) -> Option<String> {
    let normalized = path.replace('\\', "/");
    let file_name = normalized.rsplit('/').next().unwrap_or(&normalized);
    for (raw, matcher) in matchers {
        if matcher.is_match(&normalized) || matcher.is_match(file_name) {
            return Some(raw.clone());
        }
    }
    None
}

pub fn is_protected_branch(branch: &str, protected: &[String]) -> bool {
    let trimmed = branch.trim();
    if trimmed.is_empty() {
        return false;
    }
    if let Ok(matchers) = compile_path_matchers(protected) {
        for (_, matcher) in &matchers {
            if matcher.is_match(trimmed) {
                return true;
            }
        }
    }
    protected.iter().any(|entry| entry.trim() == trimmed)
}

fn scan_bytes_for_secrets(bytes: &[u8], redactor: &SecretRedactor) -> Result<bool, String> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Ok(false);
    };
    if text.len() <= MAX_SANITIZATION_BYTES {
        return Ok(!redactor.scan(text)?.is_empty());
    }
    let chunk_size = MAX_SANITIZATION_BYTES / 2;
    let step = chunk_size / 2;
    let mut start = 0usize;
    while start < text.len() {
        let mut end = (start + chunk_size).min(text.len());
        while end < text.len() && !text.is_char_boundary(end) {
            end -= 1;
        }
        let slice = &text[start..end];
        if !redactor.scan(slice)?.is_empty() {
            return Ok(true);
        }
        if end >= text.len() {
            break;
        }
        let mut next_start = start + step;
        while next_start < text.len() && !text.is_char_boundary(next_start) {
            next_start += 1;
        }
        start = next_start.max(start + 1);
    }
    Ok(false)
}

/// Validates every group and blob in `plan` against `rules` and fixed host guardrails.
pub fn validate_commit_plan(
    plan: &CommitPlanV1,
    blobs: &[PlannedBlob],
    rules: &CommitRulesV1,
    redactor: &SecretRedactor,
    author_identity: Option<(&str, &str)>,
    current_branch: Option<&str>,
    branch_already_exists: bool,
) -> PlanValidationOutcome {
    let mut out = PlanValidationOutcome {
        warnings: plan.warnings.clone(),
        blocking_violations: plan.blocking_violations.clone(),
    };

    let blob_by_path: BTreeMap<&str, &PlannedBlob> = blobs
        .iter()
        .map(|blob| (blob.workspace_rel_path.as_str(), blob))
        .collect();

    let forbidden_matchers = match compile_path_matchers(&rules.paths.forbidden) {
        Ok(m) => m,
        Err(error) => {
            out.blocking_violations.push(RuleViolation {
                code: violation_codes::FORBIDDEN_PATH.to_owned(),
                path: None,
                task_id: None,
                message: error,
            });
            Vec::new()
        }
    };

    for blob in blobs {
        if let Some(matched) = path_matches_any(&blob.workspace_rel_path, &forbidden_matchers)
            .or_else(|| path_matches_any(&blob.repo_path, &forbidden_matchers))
        {
            out.blocking_violations.push(RuleViolation {
                code: violation_codes::FORBIDDEN_PATH.to_owned(),
                path: Some(blob.workspace_rel_path.clone()),
                task_id: None,
                message: format!(
                    "Path {} matches forbidden rule pattern {matched}.",
                    blob.workspace_rel_path
                ),
            });
        }
        if blob.is_deleted {
            out.blocking_violations.push(RuleViolation {
                code: violation_codes::DELETION_UNSUPPORTED.to_owned(),
                path: Some(blob.workspace_rel_path.clone()),
                task_id: None,
                message: format!(
                    "File deletion is unsupported in this version: {}.",
                    blob.workspace_rel_path
                ),
            });
        }
        if blob.is_renamed {
            out.blocking_violations.push(RuleViolation {
                code: violation_codes::RENAME_UNSUPPORTED.to_owned(),
                path: Some(blob.workspace_rel_path.clone()),
                task_id: None,
                message: format!(
                    "File rename is unsupported in this version: {}.",
                    blob.workspace_rel_path
                ),
            });
        }
        if blob.is_symlink {
            out.blocking_violations.push(RuleViolation {
                code: violation_codes::SYMLINK_PATH.to_owned(),
                path: Some(blob.workspace_rel_path.clone()),
                task_id: None,
                message: format!(
                    "Symbolic links cannot be committed by the host: {}.",
                    blob.workspace_rel_path
                ),
            });
        }
        if blob.is_submodule {
            out.blocking_violations.push(RuleViolation {
                code: violation_codes::SUBMODULE_PATH.to_owned(),
                path: Some(blob.workspace_rel_path.clone()),
                task_id: None,
                message: format!(
                    "Git submodule paths cannot be committed by the host: {}.",
                    blob.workspace_rel_path
                ),
            });
        }
        if blob.is_lfs {
            out.blocking_violations.push(RuleViolation {
                code: violation_codes::LFS_PATH_UNSUPPORTED.to_owned(),
                path: Some(blob.workspace_rel_path.clone()),
                task_id: None,
                message: format!(
                    "Git LFS tracked paths are unsupported in this version: {}.",
                    blob.workspace_rel_path
                ),
            });
        }
        if blob.is_binary {
            out.blocking_violations.push(RuleViolation {
                code: violation_codes::BINARY_FILE.to_owned(),
                path: Some(blob.workspace_rel_path.clone()),
                task_id: None,
                message: format!(
                    "Binary files are excluded from host commits: {}.",
                    blob.workspace_rel_path
                ),
            });
        } else {
            match scan_bytes_for_secrets(&blob.content_bytes, redactor) {
                Ok(true) => {
                    out.blocking_violations.push(RuleViolation {
                        code: violation_codes::SECRET_DETECTED.to_owned(),
                        path: Some(blob.workspace_rel_path.clone()),
                        task_id: None,
                        message: format!(
                            "Deterministic secret scanner detected credential material in {}.",
                            blob.workspace_rel_path
                        ),
                    });
                }
                Ok(false) => {}
                Err(error) => {
                    out.blocking_violations.push(RuleViolation {
                        code: violation_codes::SECRET_DETECTED.to_owned(),
                        path: Some(blob.workspace_rel_path.clone()),
                        task_id: None,
                        message: format!(
                            "Secret scan could not complete for {}: {error}",
                            blob.workspace_rel_path
                        ),
                    });
                }
            }
        }
    }

    if !plan.branch.trim().is_empty() && is_protected_branch(&plan.branch, &rules.branch.protected)
    {
        out.blocking_violations.push(RuleViolation {
            code: violation_codes::PROTECTED_BRANCH.to_owned(),
            path: None,
            task_id: None,
            message: format!(
                "Target branch {} is protected and cannot be written.",
                plan.branch
            ),
        });
    }
    if rules.branch.mode == BranchMode::PerPulsation
        && branch_already_exists
        && current_branch != Some(plan.branch.as_str())
    {
        out.blocking_violations.push(RuleViolation {
            code: violation_codes::BRANCH_EXISTS.to_owned(),
            path: None,
            task_id: None,
            message: format!("Branch {} already exists.", plan.branch),
        });
    }

    let message_regex = if rules.message.pattern.trim().is_empty() {
        None
    } else {
        match Regex::new(&rules.message.pattern) {
            Ok(re) => Some(re),
            Err(error) => {
                out.blocking_violations.push(RuleViolation {
                    code: violation_codes::PATTERN_MISMATCH.to_owned(),
                    path: None,
                    task_id: None,
                    message: format!("Invalid message regex rule: {error}"),
                });
                None
            }
        }
    };

    let allowed_types: BTreeSet<&str> = rules.message.types.iter().map(String::as_str).collect();

    for group in &plan.groups {
        let task_id = group.task_ids.first().cloned();
        let mut counted_files = 0usize;
        let mut counted_lines = 0usize;
        let mut has_code = false;
        let mut has_tests = false;

        for rel_path in &group.paths {
            if let Some(blob) = blob_by_path.get(rel_path.as_str()) {
                if !blob.excluded_from_count {
                    counted_files += 1;
                    counted_lines += blob.added_lines + blob.removed_lines;
                }
                if blob.touches_code {
                    has_code = true;
                }
                if blob.is_test_file {
                    has_tests = true;
                }
            }
        }

        if counted_lines > rules.limits.max_changed_lines as usize {
            let violation = RuleViolation {
                code: violation_codes::TOO_MANY_LINES.to_owned(),
                path: None,
                task_id: task_id.clone(),
                message: format!(
                    "Commit group changes {counted_lines} counted lines (limit is {}).",
                    rules.limits.max_changed_lines
                ),
            };
            push_by_action(&mut out, rules.limits.on_exceed, violation);
        }

        if counted_files > rules.limits.max_files as usize {
            let violation = RuleViolation {
                code: violation_codes::TOO_MANY_FILES.to_owned(),
                path: None,
                task_id: task_id.clone(),
                message: format!(
                    "Commit group touches {counted_files} counted files (limit is {}).",
                    rules.limits.max_files
                ),
            };
            push_by_action(&mut out, rules.limits.on_exceed, violation);
        }

        if has_code && !has_tests {
            let violation = RuleViolation {
                code: violation_codes::MISSING_TESTS.to_owned(),
                path: None,
                task_id: task_id.clone(),
                message: "Commit group changes code without any test file.".to_owned(),
            };
            push_by_action(&mut out, rules.consistency.missing_tests, violation);
        }

        if rules.message.format == MessageFormat::Conventional
            && !allowed_types.contains(group.commit_type.as_str())
        {
            out.blocking_violations.push(RuleViolation {
                code: violation_codes::MESSAGE_TYPE_NOT_ALLOWED.to_owned(),
                path: None,
                task_id: task_id.clone(),
                message: format!("Commit type {} is not in allowed types.", group.commit_type),
            });
        }

        let formatted = format_commit_message(group, &rules.message, author_identity);
        let first_line = formatted.lines().next().unwrap_or_default();
        if first_line.chars().count() > rules.message.subject_max_chars as usize {
            out.blocking_violations.push(RuleViolation {
                code: violation_codes::SUBJECT_TOO_LONG.to_owned(),
                path: None,
                task_id: task_id.clone(),
                message: format!(
                    "Subject line is {} characters (max {}).",
                    first_line.chars().count(),
                    rules.message.subject_max_chars
                ),
            });
        }

        if rules.message.body_required && group.body.trim().is_empty() {
            out.blocking_violations.push(RuleViolation {
                code: violation_codes::BODY_MISSING.to_owned(),
                path: None,
                task_id: task_id.clone(),
                message: "Commit body is required by project rules.".to_owned(),
            });
        }

        if let Some(regex) = message_regex.as_ref()
            && !regex.is_match(&formatted)
        {
            out.blocking_violations.push(RuleViolation {
                code: violation_codes::PATTERN_MISMATCH.to_owned(),
                path: None,
                task_id,
                message: "Commit message does not match the required pattern.".to_owned(),
            });
        }
    }

    out
}

fn push_by_action(out: &mut PlanValidationOutcome, action: RuleAction, violation: RuleViolation) {
    match action {
        RuleAction::Block => out.blocking_violations.push(violation),
        RuleAction::Warn => out.warnings.push(violation),
    }
}
