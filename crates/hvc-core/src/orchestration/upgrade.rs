//! Upgrade helpers run under AppCore's existing exclusive data-directory lock.
use super::{ContentSha256, publication};
use crate::persistence_service::PersistenceServiceError as Error;
use rusqlite::{Connection, params};
use std::path::Path;

pub(crate) fn backup_before_upgrade(db: &Connection, directory: &Path) -> Result<(), Error> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='refinery_schema_history')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(());
    }
    let version: i64 = db.query_row(
        "SELECT COALESCE(MAX(version),0) FROM refinery_schema_history",
        [],
        |r| r.get(0),
    )?;
    if version >= 10 {
        return Ok(());
    }
    // VACUUM INTO captures a consistent SQLite snapshot, including WAL contents.
    // A unique destination never overwrites an earlier recovery point.
    let backup = directory.join(format!(
        "hard-vibe-code.pre-v10.{}.db",
        uuid::Uuid::new_v4()
    ));
    db.execute("VACUUM INTO ?1", params![backup.to_string_lossy()])?;
    let check = Connection::open_with_flags(&backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let integrity: String = check.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err(Error::Validation(
            "Backup anterior à V10 não passou na verificação de integridade.".into(),
        ));
    }
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(backup)?
        .sync_all()?;
    Ok(())
}

pub(crate) fn adapt_legacy(db: &mut Connection) -> Result<(), Error> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    // Conflicting aliases remain visible in Settings; route resolution fails closed.
    tx.execute("DELETE FROM provider_route_bindings AS a WHERE a.stage='Specification' AND EXISTS(SELECT 1 FROM provider_route_bindings c WHERE c.project_id=a.project_id AND c.agent_role=a.agent_role AND c.stage='SPECIFICATION' AND c.route_id=a.route_id AND c.enabled=a.enabled)", [])?;
    tx.execute("UPDATE provider_route_bindings AS a SET stage='SPECIFICATION' WHERE a.stage='Specification' AND NOT EXISTS(SELECT 1 FROM provider_route_bindings c WHERE c.project_id=a.project_id AND c.agent_role=a.agent_role AND c.stage='SPECIFICATION')", [])?;
    let artifacts = tx.prepare("SELECT a.id,a.pulsation_id,a.name,a.kind,a.relative_path,a.content_sha256,p.workspace_path FROM artifacts a JOIN pulsations u ON u.id=a.pulsation_id JOIN projects p ON p.id=u.project_id WHERE a.producer_job_id IS NULL AND a.legacy_adaptation IS NULL")?
        .query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,String>(6)?)))?.collect::<Result<Vec<_>,_>>()?;
    for (id, pulse, name, kind, path, old_hash, root) in artifacts {
        let outputs = tx.prepare("SELECT o.structured_output,j.id,j.kind FROM agent_run_outputs o JOIN jobs j ON j.id=o.job_id JOIN agent_runs r ON r.id=o.agent_run_id WHERE o.pulsation_id=?1 AND j.status='done' AND r.status='done' AND o.validation_error IS NULL")?
            .query_map([&pulse], |r| Ok((r.get::<_,Option<String>>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<Result<Vec<_>,_>>()?;
        let mut matches = Vec::new();
        for (json, job, job_kind) in outputs {
            let Some(value) = json.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            else {
                continue;
            };
            if !matches!(
                value["statusUpdate"].as_str(),
                Some("advance" | "checkpoint_ready")
            ) {
                continue;
            }
            for (position, entry) in value["artifacts"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                // The V1 runtime trimmed artifact bytes before hashing/writing. Preserve that behavior ONLY here.
                let content = entry["content"].as_str().unwrap_or("").trim();
                let hash = ContentSha256::of(content.as_bytes());
                if !content.is_empty()
                    && entry["name"].as_str().map(str::trim) == Some(name.as_str())
                    && entry["kind"].as_str().map(str::trim) == Some(kind.as_str())
                    && entry["relativePath"].as_str().map(str::trim) == path.as_deref()
                    && old_hash.as_deref().is_none_or(|h| h == hash.as_str())
                    && let Some(stage) = job_kind
                        .strip_prefix("run_stage::")
                        .and_then(super::catalog::parse_stage)
                {
                    matches.push((job.clone(), stage, position, content.to_owned(), hash));
                }
            }
        }
        if matches.len() != 1 {
            tx.execute(
                "UPDATE artifacts SET legacy_adaptation='unavailable_or_ambiguous' WHERE id=?1",
                [&id],
            )?;
            continue;
        }
        let (job, stage, position, content, hash) = matches.remove(0);
        let stage_name = stage.as_db_value();
        let approved: bool = !stage.requires_checkpoint() || tx.query_row("SELECT EXISTS(SELECT 1 FROM checkpoints WHERE pulsation_id=?1 AND stage IN (?2,?3) AND status='approved')",params![pulse,stage_name,if stage_name=="SPECIFICATION" {"Specification"} else {stage_name}],|r|r.get(0))?;
        let publication = match path.as_deref() {
            None => "not_required",
            Some(path)
                if publication::observed_hash(Path::new(&root), path)
                    .ok()
                    .flatten()
                    .as_ref()
                    == Some(&hash) =>
            {
                "applied"
            }
            Some(_) => "conflict",
        };
        tx.execute("UPDATE artifacts SET content=?1,content_sha256=?2,producer_job_id=?3,producer_stage=?4,generation=1,input_revision=1,producer_state_version=1,output_position=?5,acceptance=?6,publication_state=?7,legacy_adaptation='v1_trimmed_exact' WHERE id=?8",
            params![content,hash.as_str(),job,stage_name,position,if approved {"accepted"} else {"candidate"},publication,id])?;
    }
    // Repair the old crash window: status checkpoint committed before its row.
    tx.execute("INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,state_version) SELECT 'recovered:'||p.id,p.id,p.stage,'waiting',p.updated_at,p.updated_at,p.state_version FROM pulsations p WHERE p.status='checkpoint' AND NOT EXISTS(SELECT 1 FROM checkpoints c WHERE c.pulsation_id=p.id AND c.status='waiting')", [])?;
    tx.commit()?;
    Ok(())
}
