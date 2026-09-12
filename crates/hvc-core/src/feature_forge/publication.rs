//! Durable plan-note publication, sharing the host's canonical path and hash protocol.
use super::{service::*, *};
use crate::{
    orchestration::{ExpectedFileContent, publication},
    persistence_service::PersistenceServiceError as Error,
};
use rusqlite::{Connection, TransactionBehavior, params};

pub(crate) fn queue_plan(db: &Connection, feature: &Feature) -> Result<(), Error> {
    let b = &feature.brief;
    let mut content = format!(
        "---\ntype: feature_plan\nstatus: approved_plan\nfeature-id: {}\nrevision: {}\n---\n\n# Feature plan\n\nThis is approved planning scope, not evidence of implemented behavior.\n\n## Objective\n\n{}\n\n## Audience\n\n{}\n\n## Behavior\n\n{}\n\n## Scope\n\n{}\n\n## Exclusions\n\n{}\n\n## Acceptance criteria\n\n",
        feature.id, feature.revision, b.objective, b.audience, b.behavior, b.scope, b.exclusions
    );
    for criterion in &b.criteria {
        content.push_str(&format!("- {}: {}\n", criterion.id, criterion.description));
    }
    content.push_str("\n## Task proposals\n\n");
    for task in &b.tasks {
        content.push_str(&format!("### {}\n\n{}\n\n", task.title, task.description));
        for c in &task.criteria {
            content.push_str(&format!("- {}: {}\n", c.id, c.description));
        }
    }
    content.push_str("\n## Decisions\n\n");
    for decision in &b.decisions {
        content.push_str(&format!("- {decision}\n"));
    }
    // Validate authored prose before adding opaque source paths.
    crate::agent_output_language::validate_english_prose("feature plan", &content)
        .map_err(invalid)?;
    content.push_str("\n## Observed sources\n\n");
    for s in &b.sources {
        content.push_str(&format!(
            "- `{}` (`{}`)\n",
            s.relative_path.as_str(),
            s.content_sha256.as_str()
        ));
    }
    let path = format!(
        "knowledge/obsidian/product/features/{}/revision-{}.md",
        feature.id, feature.revision
    );
    db.execute("INSERT OR IGNORE INTO forge_publications(feature_id,revision,relative_path,content,state) VALUES(?1,?2,?3,?4,'pending')",params![feature.id,feature.revision,path,content])?;
    Ok(())
}
impl FeatureForgeService {
    /// Retry the same absent-or-identical publication after the developer resolves a conflict.
    pub fn retry_plan_publications(&self, project: &str) -> Result<(), Error> {
        self.persistence.connection()?.execute("UPDATE forge_publications SET state='pending' WHERE state='conflict' AND feature_id IN (SELECT id FROM forge_features WHERE project_id=?1)",[project])?;
        self.reconcile_plan_publications(project)
    }
    pub fn reconcile_plan_publications(&self, project: &str) -> Result<(), Error> {
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Serialize against the host's main publisher and defer during active integration.
        let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM project_pipeline_state WHERE project_id=?1 AND (active_pulsation_id IS NOT NULL OR base_state<>'ready'))",[project],|r|r.get(0))?;
        if busy {
            return Ok(());
        }
        let root: String = tx.query_row(
            "SELECT workspace_path FROM projects WHERE id=?1",
            [project],
            |r| r.get(0),
        )?;
        let pending=tx.prepare("SELECT p.feature_id,p.revision,p.relative_path,p.content FROM forge_publications p JOIN forge_features f ON f.id=p.feature_id WHERE f.project_id=?1 AND p.state='pending' ORDER BY p.feature_id,p.revision LIMIT 10")?.query_map([project],|r|Ok((r.get::<_,String>(0)?,r.get::<_,u64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?.collect::<Result<Vec<_>,_>>()?;
        for (feature, revision, path, content) in pending {
            match publication::publish(
                std::path::Path::new(&root),
                &path,
                &ExpectedFileContent::Absent,
                &content,
            ) {
                Ok(()) => {
                    tx.execute("UPDATE forge_publications SET state='applied',error=NULL WHERE feature_id=?1 AND revision=?2",params![feature,revision])?;
                }
                Err(error) => {
                    tx.execute("UPDATE forge_publications SET state='conflict',error=?3 WHERE feature_id=?1 AND revision=?2",params![feature,revision,error.to_string()])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub fn publication_issues(&self, project: &str) -> Result<Vec<String>, Error> {
        Ok(self.persistence.connection()?.prepare("SELECT p.relative_path||': '||COALESCE(p.error,'Publication pending') FROM forge_publications p JOIN forge_features f ON f.id=p.feature_id WHERE f.project_id=?1 AND p.state<>'applied'")?.query_map([project],|r|r.get(0))?.collect::<Result<Vec<_>,_>>()?)
    }
}
