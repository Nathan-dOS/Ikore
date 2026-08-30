//! BF3 component proposals, provenance and non-destructive background publication.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::TransactionBehavior;

use super::*;
use crate::orchestration::{
    ComponentProposalArtifactV1, ComponentProposalDraft, ContentSha256, SourceRevision,
};
use crate::repository_context::RepositoryContext;

pub const COMPONENT_GENERATOR_VERSION: &str = "brownfield-component-v1";

impl PersistenceService {
    pub(super) fn preflight_component_proposals(
        &self,
        pulse: &str,
    ) -> Result<HashSet<String>, PersistenceServiceError> {
        let db = self.connection()?;
        let rows = db
            .prepare(
                "SELECT a.id,a.content,p.project_id,project.workspace_path FROM artifacts a JOIN pulsations p ON p.id=a.pulsation_id JOIN projects project ON project.id=p.project_id WHERE a.pulsation_id=?1 AND a.kind='component_proposal' AND a.acceptance='candidate' AND a.input_revision=p.input_revision AND a.producer_stage IN ('Specification','SPECIFICATION') ORDER BY a.output_position,a.id",
            )?
            .query_map([pulse], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut fresh = HashSet::new();
        for (artifact_id, content, project_id, root) in rows {
            let Ok(artifact) = serde_json::from_str::<ComponentProposalArtifactV1>(&content) else {
                continue;
            };
            if artifact.validate().is_err() {
                continue;
            }
            let repository = RepositoryContext::new(Path::new(&root))
                .map_err(|error| coordination::invalid(error.to_string()))?;
            let exact = artifact.proposal.source_files.iter().all(|source| {
                repository
                    .capture_code_source(&project_id, &source.relative_path)
                    .is_ok_and(|snapshot| {
                        snapshot.revision().content_sha256 == source.content_sha256
                    })
            });
            if exact {
                fresh.insert(artifact_id);
            }
        }
        Ok(fresh)
    }

    pub(super) fn materialize_component_proposals(
        tx: &Connection,
        current: &Pulsation,
        fresh_artifacts: &HashSet<String>,
    ) -> Result<(), PersistenceServiceError> {
        let consent = Self::read_runtime_setting(
            tx,
            &format!("brownfield.onboarding_consent.{}", current.project_id),
        )?
        .as_deref()
            == Some("true");
        if !consent {
            return Ok(());
        }
        let (input_revision, state_version): (i64, i64) = tx.query_row(
            "SELECT input_revision,state_version FROM pulsations WHERE id=?1",
            [&current.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let proposals = tx
            .prepare(
                "SELECT artifact.id,artifact.content,job.kind FROM artifacts artifact JOIN jobs job ON job.id=artifact.producer_job_id WHERE artifact.pulsation_id=?1 AND artifact.kind='component_proposal' AND artifact.acceptance='accepted' AND artifact.input_revision=?2 AND artifact.producer_stage IN ('Specification','SPECIFICATION') ORDER BY artifact.output_position,artifact.id",
            )?
            .query_map(params![current.id, input_revision], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if proposals.is_empty() {
            return Ok(());
        }

        let mut decoded = Vec::new();
        for (artifact_id, content, job_kind) in proposals {
            let artifact: ComponentProposalArtifactV1 = serde_json::from_str(&content)
                .map_err(|error| coordination::invalid(error.to_string()))?;
            artifact
                .validate()
                .map_err(|error| coordination::invalid(error.to_string()))?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM brownfield_component_notes WHERE project_id=?1 AND component_id=?2)",
                params![current.project_id, artifact.proposal.component_id],
                |row| row.get(0),
            )?;
            if !exists {
                let generator = if job_kind == "brownfield_enrich" {
                    "brownfield_enrichment"
                } else {
                    "specification"
                };
                decoded.push((artifact_id, artifact.proposal, generator));
            }
        }
        if decoded.is_empty() {
            return Ok(());
        }

        let enrichment_seeds = decoded
            .iter()
            .filter(|(artifact_id, _, _)| fresh_artifacts.contains(artifact_id))
            .flat_map(|(_, proposal, _)| {
                proposal
                    .source_files
                    .iter()
                    .map(|source| source.relative_path.clone())
            })
            .collect::<HashSet<_>>();
        Self::enqueue_brownfield_enrichment_tx(tx, current, &enrichment_seeds)?;

        let known = tx
            .prepare(
                "SELECT component_id FROM brownfield_component_notes WHERE project_id=?1 AND publication_state IN ('pending','publishing','published')",
            )?
            .query_map([&current.project_id], |row| row.get::<_, String>(0))?
            .collect::<Result<HashSet<_>, _>>()?;
        let batch = decoded
            .iter()
            .filter(|(artifact_id, _, _)| fresh_artifacts.contains(artifact_id))
            .map(|(_, proposal, _)| proposal.component_id.clone())
            .collect::<HashSet<_>>();
        let now = Self::now_iso();
        let publish_job = (!batch.is_empty()).then(|| Uuid::new_v4().to_string());
        if let Some(job) = &publish_job {
            tx.execute(
                "INSERT INTO jobs(id,pulsation_id,kind,status,attempts,created_at,updated_at,started_at,finished_at,project_id,input_revision,generation,state_version,job_class,idempotency_key) VALUES(?1,?2,'brownfield_component_publish','done',1,?3,?3,?3,?3,?4,?5,1,?6,'background',?7)",
                params![job,current.id,now,current.project_id,input_revision,state_version,format!("component-publish:{job}")],
            )?;
        }

        let mut publication_position = 0_u32;
        for (proposal_artifact, proposal, generator) in decoded {
            let fresh = fresh_artifacts.contains(&proposal_artifact);
            let note_id = Uuid::new_v4().to_string();
            let relative_path =
                format!("knowledge/obsidian/components/{}.md", proposal.component_id);
            let publication_artifact = fresh.then(|| Uuid::new_v4().to_string());
            if let (Some(artifact_id), Some(job)) =
                (publication_artifact.as_ref(), publish_job.as_ref())
            {
                let content = Self::render_component_note(&proposal, generator, &known, &batch)?;
                let hash = ContentSha256::of(content.as_bytes());
                let source_revisions = proposal
                    .source_files
                    .iter()
                    .map(|source| SourceRevision {
                        project_id: current.project_id.clone(),
                        relative_path: source.relative_path.clone(),
                        content_sha256: source.content_sha256.clone(),
                    })
                    .collect::<Vec<_>>();
                tx.execute(
                    "INSERT INTO artifacts(id,pulsation_id,name,kind,relative_path,content_sha256,created_at,updated_at,content,producer_job_id,producer_stage,generation,input_revision,producer_state_version,output_position,acceptance,publication_state,source_revisions_json) VALUES(?1,?2,?3,'component',?4,?5,?6,?6,?7,?8,'Specification',1,?9,?10,?11,'accepted','pending',?12)",
                    params![artifact_id,current.id,proposal.title,relative_path,hash.as_str(),now,content,job,input_revision,state_version,publication_position,serde_json::to_string(&source_revisions).map_err(|error| coordination::invalid(error.to_string()))?],
                )?;
                tx.execute(
                    "INSERT INTO artifact_publications(artifact_id,project_id,pulsation_id,relative_path,expected_sha256,desired_sha256,state,updated_at,job_class) VALUES(?1,?2,?3,?4,NULL,?5,'pending',?6,'background')",
                    params![artifact_id,current.project_id,current.id,relative_path,hash.as_str(),now],
                )?;
                publication_position += 1;
            }
            tx.execute(
                "INSERT INTO brownfield_component_notes(id,project_id,pulsation_id,proposal_artifact_id,publication_artifact_id,component_id,title,relative_path,mapping_status,publication_state,generator,generator_version,confidence,diagnostic,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?15)",
                params![note_id,current.project_id,current.id,proposal_artifact,publication_artifact,proposal.component_id,proposal.title,relative_path,if fresh{"proposed"}else{"stale"},if fresh{"pending"}else{"not_scheduled"},generator,COMPONENT_GENERATOR_VERSION,proposal.confidence,if fresh{None}else{Some("Proposta rejeitada para publicação: revisão da fonte mudou antes da aprovação.")},now],
            )?;
            Self::insert_component_sources(tx, &note_id, &current.project_id, &proposal)?;
        }
        Ok(())
    }

    fn insert_component_sources(
        tx: &Connection,
        note_id: &str,
        project_id: &str,
        proposal: &ComponentProposalDraft,
    ) -> Result<(), PersistenceServiceError> {
        for source in &proposal.source_files {
            tx.execute(
                "INSERT INTO brownfield_component_sources(note_id,project_id,relative_path,observed_revision_sha256) VALUES(?1,?2,?3,?4)",
                params![note_id,project_id,source.relative_path.as_str(),source.content_sha256.as_str()],
            )?;
        }
        Ok(())
    }

    fn render_component_note(
        proposal: &ComponentProposalDraft,
        generator: &str,
        known: &HashSet<String>,
        batch: &HashSet<String>,
    ) -> Result<String, PersistenceServiceError> {
        let yaml_string = |value: &str| {
            serde_json::to_string(value).map_err(|error| coordination::invalid(error.to_string()))
        };
        let mut note =
            String::from("---\ntype: component\nmapping-status: proposed\nsource-files:\n");
        for source in &proposal.source_files {
            note.push_str(&format!(
                "  - {}\n",
                yaml_string(source.relative_path.as_str())?
            ));
        }
        note.push_str("observed-revision:\n");
        for source in &proposal.source_files {
            note.push_str(&format!(
                "  {}: {}\n",
                yaml_string(source.relative_path.as_str())?,
                yaml_string(source.content_sha256.as_str())?
            ));
        }
        note.push_str(&format!(
            "generator: {}\ngenerator-version: {}\nconfidence: {}\n---\n\n# {}\n\n{}\n",
            yaml_string(generator)?,
            yaml_string(COMPONENT_GENERATOR_VERSION)?,
            proposal.confidence,
            proposal.title.replace(['\r', '\n'], " "),
            proposal.summary.replace("[[", "〔〔").replace("]]", "〕〕")
        ));
        if !proposal.related_components.is_empty() {
            note.push_str("\n## Componentes relacionados\n\n");
            for related in &proposal.related_components {
                if known.contains(related) || batch.contains(related) {
                    note.push_str(&format!("- [[knowledge/obsidian/components/{related}]]\n"));
                } else {
                    note.push_str(&format!(
                        "- Pendente: componente `{related}` ainda não possui nota.\n"
                    ));
                }
            }
        }
        Ok(note)
    }

    pub(super) fn reconcile_component_publication(
        &self,
        shutdown: &CancellationToken,
    ) -> Result<bool, PersistenceServiceError> {
        if shutdown.is_cancelled() {
            return Ok(false);
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id: Option<String> = tx
            .query_row(
                "SELECT publication.artifact_id FROM artifact_publications publication JOIN brownfield_component_notes note ON note.publication_artifact_id=publication.artifact_id JOIN jobs job ON job.id=(SELECT producer_job_id FROM artifacts WHERE id=publication.artifact_id) JOIN runtime_settings consent ON consent.key='brownfield.onboarding_consent.'||note.project_id AND consent.value='true' WHERE publication.job_class='background' AND publication.state IN ('pending','applying') AND note.publication_state IN ('pending','publishing') AND note.mapping_status='proposed' AND job.status='done' AND NOT EXISTS(SELECT 1 FROM artifact_publications main WHERE main.job_class='main' AND main.state IN ('pending','applying')) ORDER BY publication.updated_at,publication.artifact_id LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(id) = id else {
            tx.commit()?;
            return Ok(false);
        };
        let token = format!("component-publish:{}", Uuid::new_v4());
        let project: String = tx.query_row(
            "SELECT project_id FROM artifact_publications WHERE artifact_id=?1",
            [&id],
            |row| row.get(0),
        )?;
        if !Self::reserve_slot(
            &tx,
            crate::orchestration::JobClass::Background,
            &token,
            Some(&project),
            None,
        )? {
            return Ok(false);
        }
        tx.execute(
            "UPDATE artifact_publications SET state='applying' WHERE artifact_id=?1",
            [&id],
        )?;
        tx.execute(
            "UPDATE artifacts SET publication_state='applying' WHERE id=?1",
            [&id],
        )?;
        tx.execute(
            "UPDATE brownfield_component_notes SET publication_state='publishing',updated_at=?2 WHERE publication_artifact_id=?1",
            params![id,Self::now_iso()],
        )?;
        tx.commit()?;
        let _slot = self.slot_guard(token);
        if shutdown.is_cancelled() {
            return Ok(false);
        }
        if let Err(error) = self.apply_publication_unit(&id) {
            let recorded: bool = self.connection()?.query_row(
                "SELECT state='conflict' FROM artifact_publications WHERE artifact_id=?1",
                [&id],
                |row| row.get(0),
            )?;
            if !recorded {
                return Err(error);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;
    use crate::persistence_service::serial::tests::Fixture;

    fn specification_with_component(
        fixture: &Fixture,
        source_content: &str,
        related: &[&str],
    ) -> StageExecutionResult {
        fs::create_dir_all(fixture.workspace.path().join("src")).unwrap();
        fs::write(fixture.workspace.path().join("src/lib.rs"), source_content).unwrap();
        let hash = ContentSha256::of(source_content.as_bytes());
        let mut output = fixture.output("checkpoint_ready", None, "Specification aprovada");
        output.structured_output["componentProposals"] = json!([{
            "componentId": "core-runtime",
            "title": "Core Runtime",
            "summary": "Coordena o fluxo observado sem aceitar [[links inventados]].",
            "confidence": 84,
            "sourceFiles": [{"relativePath":"src/lib.rs","contentSha256":hash.as_str()}],
            "relatedComponents": related,
        }]);
        output.raw_output = output.structured_output.to_string();
        output
    }

    fn accepted_component_fixture(source_content: &str, related: &[&str]) -> Fixture {
        let fixture = Fixture::new(PulsationStage::Specification);
        fixture
            .service
            .set_brownfield_onboarding_consent(&fixture.pulse.project_id, true)
            .unwrap();
        let output = specification_with_component(&fixture, source_content, related);
        fixture.accept(&output).unwrap();
        fixture
            .service
            .apply_hitl_decision(fixture.decision(HitlDecision::Approve))
            .unwrap();
        fixture
    }

    #[test]
    fn valid_proposal_publishes_provenance_and_never_invents_missing_wikilink() {
        let fixture = accepted_component_fixture("pub fn run() {}\n", &["missing-api"]);
        assert!(
            fixture
                .service
                .reconcile_component_publication(&CancellationToken::new())
                .unwrap()
        );
        let note_path = fixture
            .workspace
            .path()
            .join("knowledge/obsidian/components/core-runtime.md");
        let note = fs::read_to_string(note_path).unwrap();
        assert!(note.contains("type: component"));
        assert!(note.contains("mapping-status: proposed"));
        assert!(note.contains("source-files:"));
        assert!(note.contains("observed-revision:"));
        assert!(note.contains("generator: \"specification\""));
        assert!(note.contains("generator-version:"));
        assert!(note.contains("confidence: 84"));
        assert!(note.contains("Pendente: componente `missing-api`"));
        assert!(!note.contains("[[knowledge/obsidian/components/missing-api]]"));
        assert!(!note.contains("[[links inventados]]"));
        let state: String = fixture
            .service
            .connection()
            .unwrap()
            .query_row(
                "SELECT publication_state FROM brownfield_component_notes WHERE component_id='core-runtime'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "published");
    }

    #[test]
    fn changed_source_before_approval_records_stale_without_publication() {
        let fixture = Fixture::new(PulsationStage::Specification);
        fixture
            .service
            .set_brownfield_onboarding_consent(&fixture.pulse.project_id, true)
            .unwrap();
        let output = specification_with_component(&fixture, "pub fn old() {}\n", &[]);
        fixture.accept(&output).unwrap();
        fs::write(
            fixture.workspace.path().join("src/lib.rs"),
            "pub fn changed() {}\n",
        )
        .unwrap();
        fixture
            .service
            .record_workspace_change(
                fixture.workspace.path(),
                &crate::workspace_service::WorkspaceChangedEvent {
                    kind: "modify".into(),
                    relative_path: "src/lib.rs".into(),
                    timestamp: PersistenceService::now_iso(),
                },
            )
            .unwrap();
        fixture
            .service
            .apply_hitl_decision(fixture.decision(HitlDecision::Approve))
            .unwrap();
        let (mapping, publication, artifact): (String, String, Option<String>) = fixture
            .service
            .connection()
            .unwrap()
            .query_row(
                "SELECT mapping_status,publication_state,publication_artifact_id FROM brownfield_component_notes WHERE component_id='core-runtime'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(mapping, "stale");
        assert_eq!(publication, "not_scheduled");
        assert_eq!(artifact, None);
        assert!(
            !fixture
                .workspace
                .path()
                .join("knowledge/obsidian/components/core-runtime.md")
                .exists()
        );
    }

    #[test]
    fn existing_human_note_is_preserved_as_a_recorded_conflict() {
        let fixture = accepted_component_fixture("pub fn run() {}\n", &[]);
        let target = fixture
            .workspace
            .path()
            .join("knowledge/obsidian/components/core-runtime.md");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "# Human-owned note\n").unwrap();
        assert!(
            fixture
                .service
                .reconcile_component_publication(&CancellationToken::new())
                .unwrap()
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "# Human-owned note\n");
        let state: String = fixture
            .service
            .connection()
            .unwrap()
            .query_row(
                "SELECT publication_state FROM brownfield_component_notes WHERE component_id='core-runtime'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "conflict");
    }

    #[test]
    fn zero_budget_is_durable_and_performs_no_llm_call() {
        let fixture = Fixture::new(PulsationStage::Specification);
        fixture
            .service
            .set_brownfield_onboarding_consent(&fixture.pulse.project_id, true)
            .unwrap();
        let mut settings = fixture.service.get_preparation_settings().unwrap();
        settings.brownfield_llm_budget = 0;
        fixture
            .service
            .update_preparation_settings(settings)
            .unwrap();
        let output = specification_with_component(&fixture, "pub fn run() {}\n", &[]);
        fixture.accept(&output).unwrap();
        fixture
            .service
            .apply_hitl_decision(fixture.decision(HitlDecision::Approve))
            .unwrap();
        let (state, calls, job_status): (String, u8, String) = fixture
            .service
            .connection()
            .unwrap()
            .query_row(
                "SELECT run.state,run.budget_calls,job.status FROM brownfield_enrichment_runs run JOIN jobs job ON job.id=run.job_id",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let routes: u32 = fixture
            .service
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM llm_route_executions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            (state.as_str(), calls, job_status.as_str()),
            ("budget_exhausted", 0, "done")
        );
        assert_eq!(routes, 0);
    }

    #[tokio::test]
    async fn later_source_hash_change_marks_published_note_stale_without_regeneration() {
        let fixture = accepted_component_fixture("pub fn run() {}\n", &[]);
        fixture
            .service
            .reconcile_component_publication(&CancellationToken::new())
            .unwrap();
        let note_path = fixture
            .workspace
            .path()
            .join("knowledge/obsidian/components/core-runtime.md");
        let original_note = fs::read_to_string(&note_path).unwrap();
        fixture
            .service
            .connection()
            .unwrap()
            .execute(
                "UPDATE jobs SET status='done' WHERE job_class='main' AND status='pending'",
                [],
            )
            .unwrap();
        let map = fixture
            .service
            .enqueue_brownfield_mapping(&fixture.pulse.project_id)
            .unwrap();
        assert!(
            fixture
                .service
                .process_brownfield_mapping(&CancellationToken::new())
                .await
                .unwrap()
        );
        let status: String = fixture
            .service
            .connection()
            .unwrap()
            .query_row("SELECT status FROM jobs WHERE id=?1", [map], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "done");
        fs::write(
            fixture.workspace.path().join("src/lib.rs"),
            "pub fn changed() {}\n",
        )
        .unwrap();
        fixture
            .service
            .record_workspace_change(
                fixture.workspace.path(),
                &crate::workspace_service::WorkspaceChangedEvent {
                    kind: "modify".into(),
                    relative_path: "src/lib.rs".into(),
                    timestamp: PersistenceService::now_iso(),
                },
            )
            .unwrap();
        fixture
            .service
            .enqueue_brownfield_mapping(&fixture.pulse.project_id)
            .unwrap();
        fixture
            .service
            .process_brownfield_mapping(&CancellationToken::new())
            .await
            .unwrap();
        let mapping: String = fixture
            .service
            .connection()
            .unwrap()
            .query_row(
                "SELECT mapping_status FROM brownfield_component_notes WHERE component_id='core-runtime'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(mapping, "stale");
        assert_eq!(fs::read_to_string(note_path).unwrap(), original_note);
    }
}
