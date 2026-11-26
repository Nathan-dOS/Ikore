//! Feature 25 (M2b): global pipelines, their boxes in each project, child
//! pipelines and the adoption of new versions (plan P1 to P4). A module of
//! `persistence_service`.

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use super::custom_pipeline_store::{
    definition_errors, ensure_project, has_active_run_in, limit_errors, load_view, name_taken,
    name_taken_error, now, required, resolved_for,
};
use super::{PersistenceService, PersistenceServiceError};
use crate::custom_pipelines::{PipelineValidationError, SaveCustomPipelineOutcome};

fn invalid(message: impl Into<String>) -> PersistenceServiceError {
    PersistenceServiceError::Validation(message.into())
}

fn refused(code: &str, message: impl Into<String>) -> SaveCustomPipelineOutcome {
    SaveCustomPipelineOutcome::Invalid {
        errors: vec![PipelineValidationError::new(code, None, message)],
    }
}

/// Only the limit errors with this code.
fn limit_error(
    tx: &Transaction<'_>,
    project_id: &str,
    scope: &str,
    code: &str,
) -> Result<Vec<PipelineValidationError>, PersistenceServiceError> {
    Ok(limit_errors(tx, project_id, scope)?
        .into_iter()
        .filter(|error| error.code == code)
        .collect())
}

/// What the host knows of a pipeline, whatever project asks.
pub(super) struct PipelineRow {
    pub(super) scope: String,
    pub(super) project_id: Option<String>,
    pub(super) parent_pipeline_id: Option<String>,
    pub(super) current_version: u32,
    pub(super) archived: bool,
    pub(super) name: String,
    pub(super) template_key: Option<String>,
}

pub(super) fn pipeline_row(
    tx: &Transaction<'_>,
    pipeline_id: &str,
) -> Result<PipelineRow, PersistenceServiceError> {
    tx.query_row(
        "SELECT scope,project_id,parent_pipeline_id,current_version,archived,name,template_key
           FROM custom_pipelines WHERE id=?1",
        [pipeline_id],
        |row| {
            Ok(PipelineRow {
                scope: row.get(0)?,
                project_id: row.get(1)?,
                parent_pipeline_id: row.get(2)?,
                current_version: row.get(3)?,
                archived: row.get(4)?,
                name: row.get(5)?,
                template_key: row.get(6)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| PersistenceServiceError::NotFound("pipeline".into()))
}

/// Moves a project's box of a pipeline to the versions it adopts now: the
/// pipeline's current version and, for a child, its parent's; its state
/// follows from what that resolves to (P2, P4).
pub(super) fn adopt(
    tx: &Transaction<'_>,
    project_id: &str,
    pipeline_id: &str,
    now: &str,
) -> Result<(), PersistenceServiceError> {
    let row = pipeline_row(tx, pipeline_id)?;
    let parent_version: Option<u32> = match &row.parent_pipeline_id {
        Some(parent) => Some(pipeline_row(tx, parent)?.current_version),
        None => None,
    };
    let (definition, conflicts) = resolved_for(
        tx,
        pipeline_id,
        row.current_version,
        row.parent_pipeline_id.as_deref().zip(parent_version),
    )?;
    let mut reasons = conflicts;
    reasons.extend(
        definition_errors(tx, &definition)?
            .into_iter()
            .map(|error| error.message),
    );
    let reason = (!reasons.is_empty()).then(|| reasons.join(" "));
    tx.execute(
        "UPDATE custom_pipeline_activations
            SET adopted_version=?3,adopted_parent_version=?4,
                state=CASE WHEN ?5 IS NULL THEN 'ready' ELSE 'needs_edit' END,
                needs_edit_reason=?5,updated_at=?6
          WHERE project_id=?1 AND pipeline_id=?2",
        params![
            project_id,
            pipeline_id,
            row.current_version,
            parent_version,
            reason,
            now
        ],
    )?;
    Ok(())
}

/// Adopts a pipeline and its children in one project: where an edit was made
/// (P2: the open project adopts at once).
pub(super) fn adopt_with_children(
    tx: &Transaction<'_>,
    project_id: &str,
    pipeline_id: &str,
    now: &str,
) -> Result<(), PersistenceServiceError> {
    adopt(tx, project_id, pipeline_id, now)?;
    let children = tx
        .prepare(
            "SELECT a.pipeline_id FROM custom_pipeline_activations a
               JOIN custom_pipelines p ON p.id=a.pipeline_id
              WHERE a.project_id=?1 AND p.parent_pipeline_id=?2",
        )?
        .query_map(params![project_id, pipeline_id], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for child in children {
        adopt(tx, project_id, &child, now)?;
    }
    Ok(())
}

impl PersistenceService {
    /// A project opens: its boxes adopt the versions their pipelines and
    /// parents have now (P2). A closed project keeps what it adopted.
    pub fn adopt_custom_pipelines(&self, project_id: &str) -> Result<(), PersistenceServiceError> {
        let project_id = required(project_id, "Project")?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now();
        let boxes = tx
            .prepare(
                "SELECT a.pipeline_id FROM custom_pipeline_activations a
                   JOIN custom_pipelines p ON p.id=a.pipeline_id
                  WHERE a.project_id=?1 AND p.archived=0",
            )?
            .query_map([project_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for pipeline_id in boxes {
            adopt(&tx, project_id, &pipeline_id, &now)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// `Make global`: a project's pipeline serves every project (R4a). Refused
    /// for a child whose parent belongs to a project (P3).
    pub fn make_custom_pipeline_global(
        &self,
        project_id: &str,
        pipeline_id: &str,
    ) -> Result<SaveCustomPipelineOutcome, PersistenceServiceError> {
        let project_id = required(project_id, "Project")?;
        let pipeline_id = required(pipeline_id, "Pipeline")?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = pipeline_row(&tx, pipeline_id)?;
        if row.scope != "project" || row.project_id.as_deref() != Some(project_id) {
            return Err(PersistenceServiceError::NotFound("pipeline".into()));
        }
        if row.archived {
            return Ok(refused("pipeline_archived", "Restore the pipeline first."));
        }
        if let Some(parent) = &row.parent_pipeline_id
            && pipeline_row(&tx, parent)?.scope == "project"
        {
            return Ok(refused(
                "parent_in_project",
                "A child of a project's pipeline stays in that project.",
            ));
        }
        let errors = limit_error(&tx, project_id, "global", "global_limit")?;
        if !errors.is_empty() {
            return Ok(SaveCustomPipelineOutcome::Invalid { errors });
        }
        tx.execute(
            "UPDATE custom_pipelines SET scope='global',project_id=NULL,updated_at=?2 WHERE id=?1",
            params![pipeline_id, now()],
        )?;
        tx.commit()?;
        Ok(SaveCustomPipelineOutcome::Saved {
            pipeline: Box::new(load_view(&self.connection()?, project_id, pipeline_id)?),
        })
    }

    /// `Move to this project`: a global pipeline only this project uses
    /// becomes the project's own.
    pub fn move_custom_pipeline_to_project(
        &self,
        project_id: &str,
        pipeline_id: &str,
    ) -> Result<SaveCustomPipelineOutcome, PersistenceServiceError> {
        let project_id = required(project_id, "Project")?;
        let pipeline_id = required(pipeline_id, "Pipeline")?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = pipeline_row(&tx, pipeline_id)?;
        if row.scope != "global" {
            return Err(PersistenceServiceError::NotFound("global pipeline".into()));
        }
        if row.template_key.is_some() {
            return Ok(refused(
                "builtin_immutable",
                "Default agent pipelines stay global. Create a child pipeline instead.",
            ));
        }
        let elsewhere: u32 = tx.query_row(
            "SELECT count(*) FROM custom_pipeline_activations WHERE pipeline_id=?1 AND project_id<>?2",
            params![pipeline_id, project_id],
            |row| row.get(0),
        )?;
        if elsewhere > 0 {
            return Ok(refused(
                "active_elsewhere",
                "Other projects use this pipeline. Remove it from them first.",
            ));
        }
        let foreign_children: u32 = tx.query_row(
            "SELECT count(*) FROM custom_pipelines
              WHERE parent_pipeline_id=?1 AND (scope='global' OR project_id<>?2)",
            params![pipeline_id, project_id],
            |row| row.get(0),
        )?;
        if foreign_children > 0 {
            return Ok(refused(
                "children_elsewhere",
                "Global pipelines or other projects build on this one. Detach them first.",
            ));
        }
        if !row.archived {
            let errors = limit_error(&tx, project_id, "project", "project_limit")?;
            if !errors.is_empty() {
                return Ok(SaveCustomPipelineOutcome::Invalid { errors });
            }
        }
        tx.execute(
            "UPDATE custom_pipelines SET scope='project',project_id=?2,updated_at=?3 WHERE id=?1",
            params![pipeline_id, project_id, now()],
        )?;
        tx.commit()?;
        Ok(SaveCustomPipelineOutcome::Saved {
            pipeline: Box::new(load_view(&self.connection()?, project_id, pipeline_id)?),
        })
    }

    /// `Detach`: a child becomes an independent pipeline with the definition
    /// it resolves to in this project now.
    pub fn detach_custom_pipeline(
        &self,
        project_id: &str,
        pipeline_id: &str,
    ) -> Result<SaveCustomPipelineOutcome, PersistenceServiceError> {
        let project_id = required(project_id, "Project")?;
        let pipeline_id = required(pipeline_id, "Pipeline")?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = pipeline_row(&tx, pipeline_id)?;
        let Some(parent) = row.parent_pipeline_id.clone() else {
            return Ok(refused(
                "not_a_child",
                "This pipeline is not based on another.",
            ));
        };
        let adopted_parent: Option<u32> = tx
            .query_row(
                "SELECT adopted_parent_version FROM custom_pipeline_activations
                  WHERE project_id=?1 AND pipeline_id=?2",
                params![project_id, pipeline_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let parent_version = match adopted_parent {
            Some(version) => version,
            None => pipeline_row(&tx, &parent)?.current_version,
        };
        let (definition, _) = resolved_for(
            &tx,
            pipeline_id,
            row.current_version,
            Some((parent.as_str(), parent_version)),
        )?;
        let json =
            serde_json::to_string(&definition).map_err(|error| invalid(error.to_string()))?;
        let sha = crate::exchange::render::sha256_hex(&json);
        let next = row.current_version + 1;
        let now = now();
        tx.execute(
            "INSERT INTO custom_pipeline_versions(pipeline_id,version,definition_json,
               definition_sha256,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![pipeline_id, next, json, sha, now],
        )?;
        tx.execute(
            "UPDATE custom_pipelines SET parent_pipeline_id=NULL,current_version=?2,updated_at=?3
              WHERE id=?1",
            params![pipeline_id, next, now],
        )?;
        adopt(&tx, project_id, pipeline_id, &now)?;
        tx.commit()?;
        Ok(SaveCustomPipelineOutcome::Saved {
            pipeline: Box::new(load_view(&self.connection()?, project_id, pipeline_id)?),
        })
    }

    /// `Add global pipeline…`: a global pipeline gets a box in this project,
    /// within the limits and with a free name.
    pub fn activate_custom_pipeline(
        &self,
        project_id: &str,
        pipeline_id: &str,
    ) -> Result<SaveCustomPipelineOutcome, PersistenceServiceError> {
        let project_id = required(project_id, "Project")?;
        let pipeline_id = required(pipeline_id, "Pipeline")?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_project(&tx, project_id)?;
        let row = pipeline_row(&tx, pipeline_id)?;
        if row.scope != "global" || row.archived {
            return Err(PersistenceServiceError::NotFound("global pipeline".into()));
        }
        let active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM custom_pipeline_activations
              WHERE project_id=?1 AND pipeline_id=?2)",
            params![project_id, pipeline_id],
            |row| row.get(0),
        )?;
        if !active {
            let mut errors = limit_error(&tx, project_id, "global", "active_limit")?;
            if name_taken(&tx, project_id, &row.name, Some(pipeline_id))? {
                errors.push(name_taken_error());
            }
            if !errors.is_empty() {
                return Ok(SaveCustomPipelineOutcome::Invalid { errors });
            }
            let now = now();
            tx.execute(
                "INSERT INTO custom_pipeline_activations(project_id,pipeline_id,position,
                   adopted_version,state,created_at,updated_at)
                 VALUES(?1,?2,(SELECT COALESCE(max(position)+1,0)
                                 FROM custom_pipeline_activations WHERE project_id=?1),
                        ?3,'ready',?4,?4)",
                params![project_id, pipeline_id, row.current_version, now],
            )?;
            adopt(&tx, project_id, pipeline_id, &now)?;
        }
        tx.commit()?;
        Ok(SaveCustomPipelineOutcome::Saved {
            pipeline: Box::new(load_view(&self.connection()?, project_id, pipeline_id)?),
        })
    }

    /// `Remove from this project`: the box goes; the global pipeline and its
    /// runs stay.
    pub fn deactivate_custom_pipeline(
        &self,
        project_id: &str,
        pipeline_id: &str,
    ) -> Result<(), PersistenceServiceError> {
        let project_id = required(project_id, "Project")?;
        let pipeline_id = required(pipeline_id, "Pipeline")?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = pipeline_row(&tx, pipeline_id)?;
        if row.scope != "global" {
            return Err(invalid(
                "A project's own pipeline is archived or deleted, not removed.",
            ));
        }
        if has_active_run_in(&tx, pipeline_id, Some(project_id))? {
            return Err(invalid(
                "This pipeline has a run in progress here. Cancel it first.",
            ));
        }
        tx.execute(
            "DELETE FROM custom_pipeline_activations WHERE project_id=?1 AND pipeline_id=?2",
            params![project_id, pipeline_id],
        )?;
        tx.commit()?;
        Ok(())
    }
}
