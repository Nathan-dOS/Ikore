//! Pulsation digests (ADR-037 M4): what a finished pulsation was for, what
//! it decided and what it changed, derived by the host from its rows without
//! a model. The derivation is versioned and kept, like the attestation
//! derivers: the same rows give the same bytes in any later build.

use std::fmt::Write as _;

use ikore_contracts::exchange::{
    AgentProducer, ApprovalGate, ExchangeItemV1, ExchangeKind, HostComponent, HumanSource, Producer,
};
use ikore_contracts::stages::PulsationStage;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use sha2::Digest as _;

use crate::persistence_service::PersistenceServiceError;

/// The derivation in this file. A change to the text is a new version.
pub const DIGEST_VERSION: u32 = 1;
/// Longest digest, in bytes; `## Files` is cut first.
pub const MAX_DIGEST_BYTES: usize = 64 * 1024;
/// Terminal pulsations a search derives when their digest is missing,
/// newest first.
pub const BACKFILL_LIMIT: usize = 200;
/// The addressable sections, in order.
pub const SECTIONS: [&str; 9] = [
    "Header",
    "Intent",
    "Decisions",
    "Criteria",
    "Checkpoints",
    "Stages",
    "Files",
    "Risk and coverage",
    "Attestation",
];

/// What a pulsation did to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEffect {
    Modified,
    Created,
    Deleted,
}

impl FileEffect {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Modified => "modified",
            Self::Created => "created",
            Self::Deleted => "deleted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulsationDigestV1 {
    pub pulsation_id: String,
    pub project_id: String,
    pub title: String,
    /// `done` or `failed`.
    pub outcome: String,
    pub ended_at: String,
    /// Markdown; every section has its provenance line.
    pub content: String,
    pub paths: Vec<(String, FileEffect)>,
}

fn line(kind: ExchangeKind, producer: Producer, gates: &[ApprovalGate]) -> String {
    ExchangeItemV1::new(kind, producer)
        .approved(gates.iter().copied())
        .provenance_line()
}

fn host_line() -> String {
    line(
        ExchangeKind::PulsationDigest,
        Producer::Host(HostComponent::ExchangeDigest),
        &[],
    )
}

fn section(text: &mut String, name: &str, provenance: &str, body: &str) {
    let _ = write!(text, "## {name}\n{provenance}\n");
    if body.trim().is_empty() {
        text.push_str("None recorded.\n");
    } else {
        text.push_str(body.trim_end());
        text.push('\n');
    }
    text.push('\n');
}

/// One line, so no entry can start a section or a provenance line.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The lines under the first heading whose text contains `word`, at any
/// level, up to the next heading of the same or a higher level.
fn markdown_section(markdown: &str, word: &str, max_lines: usize) -> Vec<String> {
    let word = word.to_ascii_lowercase();
    let mut level = None;
    let mut lines = Vec::new();
    for raw in markdown.lines() {
        let hashes = raw.chars().take_while(|c| *c == '#').count();
        let heading = hashes > 0 && raw[hashes..].starts_with(' ');
        match level {
            None if heading && raw.to_ascii_lowercase().contains(&word) => level = Some(hashes),
            Some(current) if heading && hashes <= current => break,
            Some(_) if !raw.trim().is_empty() => {
                lines.push(one_line(raw));
                if lines.len() == max_lines {
                    break;
                }
            }
            _ => {}
        }
    }
    lines
}

fn first_paragraph(markdown: &str, max_chars: usize) -> String {
    let paragraph = markdown
        .split("\n\n")
        .map(str::trim)
        .find(|block| !block.is_empty() && !block.starts_with('#'))
        .unwrap_or_default();
    let text = one_line(paragraph);
    if text.chars().count() > max_chars {
        let mut cut: String = text.chars().take(max_chars).collect();
        cut.push_str("...");
        cut
    } else {
        text
    }
}

fn table_exists(db: &Connection, table: &str) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )
}

/// The digest of a finished pulsation, or `None` while it is running.
pub fn derive(db: &Connection, pulsation_id: &str) -> rusqlite::Result<Option<PulsationDigestV1>> {
    let Some((project_id, feature, status, stage, created, updated, pipeline, reason, feedback)) =
        db.query_row(
            "SELECT project_id,feature_name,status,stage,created_at,updated_at,pipeline_version,
                    terminal_reason,revision_feedback
             FROM pulsations WHERE id=?1",
            [pulsation_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(None);
    };
    if !matches!(status.as_str(), "done" | "failed") {
        return Ok(None);
    }
    let mut text = format!("# Pulsation: {}\n\n", one_line(&feature));

    // Header
    let mut header = format!(
        "- Feature: {}\n- Reference: pulsation:{pulsation_id}\n- Started: {created}\n- Ended: {updated}\n- Last stage: {stage}\n- Outcome: {status}",
        one_line(&feature)
    );
    if let Some(version) = pipeline {
        let _ = write!(header, "\n- Pipeline: v{version}");
    }
    if let Some(reason) = &reason {
        let _ = write!(header, "\n- Ended because: {}", one_line(reason));
    }
    section(&mut text, "Header", &host_line(), &header);

    // Intent and criteria come from the Forge batch the developer approved;
    // decisions from the Specification approved at its checkpoint.
    let manifest: Option<Value> = if table_exists(db, "pulsation_batches")? {
        db.query_row(
            "SELECT manifest_json FROM pulsation_batches WHERE pulsation_id=?1",
            [pulsation_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|json| serde_json::from_str(&json).ok())
    } else {
        None
    };
    let specification: Option<String> = db
        .query_row(
            "SELECT content FROM artifacts WHERE pulsation_id=?1 AND producer_stage='SPECIFICATION'
               AND acceptance='accepted' ORDER BY output_position,id LIMIT 1",
            [pulsation_id],
            |row| row.get(0),
        )
        .optional()?;
    let forge = line(
        ExchangeKind::TaskScope,
        Producer::agent(AgentProducer::ForgeBatch),
        &[ApprovalGate::ForgeBatch],
    );
    let specified = line(
        ExchangeKind::Decision,
        Producer::agent(AgentProducer::SpecificationAuthor),
        &[ApprovalGate::Checkpoint(PulsationStage::Specification)],
    );
    let mut intent = String::new();
    if let Some(manifest) = &manifest {
        if let Some(title) = manifest["title"].as_str() {
            let _ = writeln!(intent, "- Batch: {}", one_line(title));
        }
        for feature in manifest["features"].as_array().into_iter().flatten() {
            let title = feature["title"].as_str().unwrap_or_default();
            let summary = feature["summary"]
                .as_str()
                .or_else(|| feature["description"].as_str())
                .unwrap_or_default();
            if !title.is_empty() {
                let _ = writeln!(intent, "- {}: {}", one_line(title), one_line(summary));
            }
        }
    }
    let (intent_line, intent) = if intent.is_empty() {
        let summary = specification
            .as_deref()
            .map(|content| first_paragraph(content, 600))
            .unwrap_or_default();
        (specified.clone(), summary)
    } else {
        (forge.clone(), intent)
    };
    section(&mut text, "Intent", &intent_line, &intent);

    let decisions = specification
        .as_deref()
        .map(|content| markdown_section(content, "decision", 40))
        .unwrap_or_default()
        .into_iter()
        .map(|line| {
            if line.starts_with('-') {
                line
            } else {
                format!("- {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    section(&mut text, "Decisions", &specified, &decisions);

    let mut criteria = String::new();
    if let Some(manifest) = &manifest {
        for task in manifest["tasks"].as_array().into_iter().flatten() {
            let title = task["draft"]["title"].as_str().unwrap_or_default();
            let _ = writeln!(criteria, "- {}", one_line(title));
            for criterion in task["draft"]["criteria"].as_array().into_iter().flatten() {
                let _ = writeln!(
                    criteria,
                    "  - {}: {}",
                    one_line(criterion["id"].as_str().unwrap_or_default()),
                    one_line(criterion["description"].as_str().unwrap_or_default())
                );
            }
        }
    }
    section(&mut text, "Criteria", &forge, &criteria);

    // Checkpoints: the host's record of each decision, and the developer's
    // own words under their own line.
    let mut checkpoints = String::new();
    let mut statement = db.prepare(
        "SELECT stage,COALESCE(purpose,kind,'review'),status,updated_at FROM checkpoints
         WHERE pulsation_id=?1 ORDER BY created_at,id",
    )?;
    let rows = statement.query_map([pulsation_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (stage, purpose, decision, at) = row?;
        let _ = writeln!(checkpoints, "- {stage} {purpose}: {decision} at {at}");
    }
    if let Some(feedback) = feedback.as_deref().filter(|text| !text.trim().is_empty()) {
        let _ = write!(
            checkpoints,
            "\nLatest revision feedback:\n{}\n{}",
            line(
                ExchangeKind::RevisionFeedback,
                Producer::Human(HumanSource::CheckpointFeedback),
                &[]
            ),
            one_line(feedback)
        );
    }
    section(&mut text, "Checkpoints", &host_line(), &checkpoints);

    // Stages: one line per stage job, with its attempts and observed model.
    let mut stages = String::new();
    let mut statement = db.prepare(
        "SELECT j.kind,j.status,
                (SELECT COUNT(*) FROM agent_runs a WHERE a.job_id=j.id),
                (SELECT a.model FROM agent_runs a WHERE a.job_id=j.id AND a.model IS NOT NULL
                   ORDER BY a.updated_at DESC,a.id LIMIT 1)
         FROM jobs j WHERE j.pulsation_id=?1 AND j.kind LIKE 'run_stage::%'
         ORDER BY j.created_at,j.id",
    )?;
    let rows = statement.query_map([pulsation_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    for row in rows {
        let (kind, job_status, attempts, model) = row?;
        let stage = kind.trim_start_matches("run_stage::");
        let _ = write!(stages, "- {stage}: {job_status}, {attempts} attempt(s)");
        if let Some(model) = model {
            let _ = write!(stages, ", model {}", one_line(&model));
        }
        stages.push('\n');
    }
    section(&mut text, "Stages", &host_line(), &stages);

    // Files: what the pulsation published, by its recorded bases.
    let mut paths = Vec::new();
    if table_exists(db, "pulsation_file_bases")? {
        let mut statement = db.prepare(
            "SELECT relative_path,state FROM pulsation_file_bases WHERE pulsation_id=?1
             ORDER BY relative_path",
        )?;
        let rows = statement.query_map([pulsation_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (path, state) = row?;
            let effect = if state == "absent" {
                FileEffect::Created
            } else {
                FileEffect::Modified
            };
            paths.push((path, effect));
        }
    }

    let mut risk = String::new();
    if table_exists(db, "pulsation_risk_assessments")? {
        let mut statement = db.prepare(
            "SELECT stage,level,input_revision FROM pulsation_risk_assessments WHERE pulsation_id=?1
             ORDER BY created_at,stage",
        )?;
        let rows = statement.query_map([pulsation_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (stage, level, revision) = row?;
            let _ = writeln!(risk, "- {stage} risk: {level} (input revision {revision})");
        }
    }
    if table_exists(db, "integrator_coverage")? {
        let coverage: Option<(String, i64)> = db
            .query_row(
                "SELECT level,pending FROM integrator_coverage WHERE pulsation_id=?1
                 ORDER BY input_revision DESC LIMIT 1",
                [pulsation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((level, pending)) = coverage {
            let _ = writeln!(risk, "- Integrator coverage: {level}, {pending} pending");
        }
    }

    let mut attestation = String::new();
    if table_exists(db, "attestation_seal_intents")? {
        let state: Option<String> = db
            .query_row(
                "SELECT state FROM attestation_seal_intents WHERE pulsation_id=?1",
                [pulsation_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(state) = state {
            let _ = writeln!(attestation, "- Seal: {state}");
        }
    }

    // The cap: files are listed last-first trimmed until the digest fits.
    let tail = |listed: usize| {
        let mut files = String::new();
        for (path, effect) in paths.iter().take(listed) {
            let _ = writeln!(files, "- {} `{}`", effect.as_str(), one_line(path));
        }
        if listed < paths.len() {
            let _ = writeln!(
                files,
                "- {} more files are not listed",
                paths.len() - listed
            );
        }
        let mut rest = String::new();
        section(&mut rest, "Files", &host_line(), &files);
        section(&mut rest, "Risk and coverage", &host_line(), &risk);
        section(&mut rest, "Attestation", &host_line(), &attestation);
        rest
    };
    let mut listed = paths.len();
    let mut rest = tail(listed);
    while text.len() + rest.len() > MAX_DIGEST_BYTES && listed > 0 {
        listed = listed.saturating_sub((listed / 4).max(1));
        rest = tail(listed);
    }
    text.push_str(&rest);
    if text.len() > MAX_DIGEST_BYTES {
        let mut cut = MAX_DIGEST_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    Ok(Some(PulsationDigestV1 {
        pulsation_id: pulsation_id.to_owned(),
        project_id,
        title: one_line(&feature),
        outcome: status,
        ended_at: updated,
        content: text,
        paths,
    }))
}

/// One section of a digest by its name, with its heading and provenance.
pub fn section_of(content: &str, name: &str) -> Option<String> {
    let heading = format!("## {name}\n");
    let start = content.find(&heading)?;
    let rest = &content[start..];
    let end = rest[heading.len()..]
        .find("\n## ")
        .map_or(rest.len(), |index| heading.len() + index + 1);
    Some(rest[..end].to_owned())
}

/// Derives and stores the digest of a finished pulsation: sanitized, with
/// its sanitization receipt, its paths and the exchange log item that
/// announces it, in one transaction. `false` when the pulsation is not
/// finished, or its stored digest is already this version's.
pub fn refresh(db: &Connection, pulsation_id: &str) -> Result<bool, PersistenceServiceError> {
    let runtime = PersistenceServiceError::Runtime;
    let Some(digest) = derive(db, pulsation_id)? else {
        return Ok(false);
    };
    let mut content = crate::secret_redactor::redact_secrets(&digest.content).map_err(runtime)?;
    // A redaction marker can be longer than what it replaced; the stored
    // digest keeps the cap.
    if content.text.len() > MAX_DIGEST_BYTES {
        let mut cut = MAX_DIGEST_BYTES;
        while !content.text.is_char_boundary(cut) {
            cut -= 1;
        }
        content.text.truncate(cut);
    }
    let title = crate::secret_redactor::redact_secrets(&digest.title).map_err(runtime)?;
    let sha256 = crate::lower_hex(&sha2::Sha256::digest(content.text.as_bytes()));
    let current: Option<(u32, String)> = db
        .query_row(
            "SELECT digest_version,content_sha256 FROM companion_pulsation_digests WHERE pulsation_id=?1",
            [pulsation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if current.as_ref() == Some(&(DIGEST_VERSION, sha256.clone())) {
        return Ok(false);
    }
    let policy = crate::sanitization::SanitizationStore::policy_on(db, &digest.project_id)
        .map_err(runtime)?;
    let now = chrono::Utc::now().to_rfc3339();
    crate::exchange::log::atomically(db, || -> Result<(), PersistenceServiceError> {
        db.execute(
            "INSERT INTO companion_pulsation_digests(project_id,pulsation_id,digest_version,title,content,outcome,ended_at,content_sha256,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)
             ON CONFLICT(pulsation_id) DO UPDATE SET digest_version=excluded.digest_version,title=excluded.title,
               content=excluded.content,outcome=excluded.outcome,ended_at=excluded.ended_at,
               content_sha256=excluded.content_sha256,updated_at=excluded.updated_at",
            params![
                digest.project_id,
                digest.pulsation_id,
                DIGEST_VERSION,
                title.text,
                content.text,
                digest.outcome,
                digest.ended_at,
                sha256,
                now
            ],
        )?;
        db.execute(
            "DELETE FROM companion_pulsation_paths WHERE pulsation_id=?1",
            [pulsation_id],
        )?;
        for (path, effect) in &digest.paths {
            db.execute(
                "INSERT OR IGNORE INTO companion_pulsation_paths(pulsation_id,relative_path,effect) VALUES(?1,?2,?3)",
                params![pulsation_id, path, effect.as_str()],
            )?;
        }
        let categories = serde_json::to_string(&content.categories)
            .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))?;
        db.execute(
            "INSERT INTO secret_sanitization_receipts(call_id,project_id,pulsation_id,policy_identity,input_redaction_count,categories_json,examined_bytes,duration_ms,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,0,?8) ON CONFLICT(call_id) DO NOTHING",
            params![
                format!("pulsation-digest:{pulsation_id}:{}", &sha256[..16]),
                digest.project_id,
                pulsation_id,
                policy.identity(),
                (content.count + title.count) as i64,
                categories,
                digest.content.len() as i64,
                now
            ],
        )?;
        use crate::exchange::log::{
            AppendRequest, LogRecordKind, PayloadRef, StreamKind, StreamRef,
        };
        crate::exchange::log::record(
            db,
            &AppendRequest::host(
                StreamRef::new(&digest.project_id, StreamKind::Pulsation, pulsation_id),
                LogRecordKind::PulsationDigestReady,
                HostComponent::ExchangeDigest,
                Some(PayloadRef {
                    table: "companion_pulsation_digests",
                    id: pulsation_id.to_owned(),
                }),
                format!(
                    "pulsation_digest:{pulsation_id}:v{DIGEST_VERSION}:{}",
                    &sha256[..16]
                ),
            ),
        )?;
        Ok(())
    })?;
    Ok(true)
}

/// Derives the missing digests of the project's newest finished pulsations,
/// at most `limit`; the first search of a project backfills them. A
/// pulsation that cannot be derived is skipped, so it never hides the others
/// from a search.
pub fn backfill(
    db: &Connection,
    project_id: &str,
    limit: usize,
) -> Result<usize, PersistenceServiceError> {
    let missing: Vec<String> = db
        .prepare(
            "SELECT p.id FROM pulsations p WHERE p.project_id=?1 AND p.status IN ('done','failed')
               AND NOT EXISTS(SELECT 1 FROM companion_pulsation_digests d WHERE d.pulsation_id=p.id)
             ORDER BY p.updated_at DESC,p.id LIMIT ?2",
        )
        .and_then(|mut statement| {
            statement
                .query_map(params![project_id, limit as i64], |row| row.get(0))?
                .collect()
        })?;
    let mut derived = 0;
    for pulsation_id in missing {
        match refresh(db, &pulsation_id) {
            Ok(true) => derived += 1,
            Ok(false) => {}
            Err(error) => eprintln!("pulsation digest skipped: {error}"),
        }
    }
    Ok(derived)
}

#[cfg(test)]
#[path = "digest_tests.rs"]
mod tests;
