//! Local assessment, versioned human actions and bounded speculative jobs.
use super::*;
use crate::orchestration::*;
use coordination::invalid;
use rusqlite::TransactionBehavior;

#[cfg(test)]
#[path = "preparation_tests.rs"]
mod tests;

impl PersistenceService {
    pub fn get_preparation_settings(&self) -> Result<PreparationSettings, PersistenceServiceError> {
        let db = self.connection()?;
        let tx = db.unchecked_transaction()?;
        Self::preparation_settings(&tx)
    }

    pub(super) fn preparation_settings(
        db: &Connection,
    ) -> Result<PreparationSettings, PersistenceServiceError> {
        let mut settings = PreparationSettings::default();
        if let Some(value) = Self::read_runtime_setting(db, "orchestration.preparation_enabled")? {
            settings.preparation_enabled = value == "true";
        }
        if let Some(value) = Self::read_runtime_setting(db, "orchestration.learning_enabled")? {
            settings.learning_enabled = value == "true";
        }
        if let Some(value) =
            Self::read_runtime_setting(db, "orchestration.learning_refinement_mode")?
        {
            settings.learning_refinement_mode = LearningRefinementMode::parse(&value)
                .ok_or_else(|| invalid("Modo de refinamento Learning inválido."))?;
        }
        if let Some(value) = Self::read_runtime_setting(db, "orchestration.vagueness_threshold")? {
            settings.vagueness_threshold = value
                .parse::<u8>()
                .map_err(|_| invalid("Threshold inválido."))?
                .try_into()
                .map_err(|e: ContractError| invalid(e.to_string()))?;
        }
        if let Some(value) = Self::read_runtime_setting(db, "brownfield_llm_budget")? {
            settings.brownfield_llm_budget = value
                .parse::<u8>()
                .map_err(|_| invalid("Orçamento LLM brownfield inválido."))?;
        }
        if let Some(value) = Self::read_runtime_setting(db, "brownfield_expansion_radius")? {
            settings.brownfield_expansion_radius = value
                .parse::<u8>()
                .map_err(|_| invalid("Raio de expansão brownfield inválido."))?;
        }
        Self::validate_brownfield_settings(&settings)?;
        Ok(settings)
    }

    fn validate_brownfield_settings(
        settings: &PreparationSettings,
    ) -> Result<(), PersistenceServiceError> {
        if settings.brownfield_llm_budget > 8
            || !matches!(settings.brownfield_expansion_radius, 1 | 2)
        {
            return Err(invalid(
                "Brownfield exige orçamento entre 0 e 8 e raio 1 ou 2.",
            ));
        }
        Ok(())
    }

    pub fn update_preparation_settings(
        &self,
        settings: PreparationSettings,
    ) -> Result<PreparationSettings, PersistenceServiceError> {
        Self::validate_brownfield_settings(&settings)?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (key, value) in [
            (
                "orchestration.preparation_enabled",
                settings.preparation_enabled.to_string(),
            ),
            (
                "orchestration.vagueness_threshold",
                settings.vagueness_threshold.get().to_string(),
            ),
            (
                "orchestration.learning_enabled",
                settings.learning_enabled.to_string(),
            ),
            (
                "orchestration.learning_refinement_mode",
                settings.learning_refinement_mode.as_str().to_string(),
            ),
            (
                "brownfield_llm_budget",
                settings.brownfield_llm_budget.to_string(),
            ),
            (
                "brownfield_expansion_radius",
                settings.brownfield_expansion_radius.to_string(),
            ),
        ] {
            Self::upsert_runtime_setting(&tx, key, &value)?;
        }
        let mut cancelled = Self::configure_learning(
            &tx,
            settings.learning_enabled,
            settings.learning_refinement_mode,
        )?;
        if settings.brownfield_llm_budget == 0 {
            let enrichment_jobs = tx
                .prepare(
                    "SELECT id FROM jobs WHERE kind='brownfield_enrich' AND status IN ('pending','running','interrupted')",
                )?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            if !enrichment_jobs.is_empty() {
                let now = Self::now_iso();
                tx.execute(
                    "UPDATE jobs SET status='failed',cancellation_reason='superseded',last_error='Orçamento LLM brownfield configurado como zero.',finished_at=?1,updated_at=?1 WHERE kind='brownfield_enrich' AND status IN ('pending','running','interrupted')",
                    [&now],
                )?;
                tx.execute(
                    "UPDATE brownfield_enrichment_runs SET state='budget_exhausted',diagnostic='Orçamento LLM brownfield configurado como zero; nenhuma chamada realizada.',updated_at=?1 WHERE state IN ('pending','running')",
                    [&now],
                )?;
                cancelled.extend(enrichment_jobs);
            }
        }
        if !settings.preparation_enabled {
            let pulses = tx
                .prepare(
                    "SELECT pulsation_id FROM pulsation_preparation WHERE consumed_job_id IS NULL",
                )?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            for pulse in pulses {
                cancelled.extend(Self::invalidate_preparation(&tx, &pulse, "disabled")?);
            }
            // An anticipated Intake is read-only. Cancel and requeue it for normal admission,
            // without converting a pending description review into consent.
            let ids = tx.prepare("SELECT j.id FROM jobs j JOIN pulsations p ON p.id=j.pulsation_id WHERE j.job_class='background' AND j.kind='run_stage::Intake' AND j.status='running' AND p.id NOT IN (SELECT id FROM coordinated_pulsations)")?.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            for id in ids {
                tx.execute("UPDATE jobs SET status='pending',job_class='main',generation=generation+1,started_at=NULL WHERE id=?1", [&id])?;
                tx.execute("UPDATE agent_runs SET status='interrupted',updated_at=?2 WHERE job_id=?1 AND status='running'", params![id, Self::now_iso()])?;
                cancelled.push(id);
            }
        }
        tx.commit()?;
        self.signal_cancelled_jobs(&cancelled);
        self.wake_scheduler();
        Ok(settings)
    }

    pub(super) fn assess_description(
        db: &Connection,
        pulse: &str,
        description: &str,
        revision: u64,
    ) -> Result<(), PersistenceServiceError> {
        let settings = Self::preparation_settings(db)?;
        let assessment = task_refine::assess(description, revision, settings.vagueness_threshold);
        let review = if settings.preparation_enabled
            && assessment.score.get() >= assessment.threshold.get()
        {
            "pending"
        } else {
            "not_required"
        };
        let json = serde_json::to_string(&assessment).map_err(|e| invalid(e.to_string()))?;
        let now = Self::now_iso();
        db.execute("INSERT INTO pulsation_description_revisions(pulsation_id,input_revision,description,assessment_json,review_state,created_at) VALUES(?1,?2,?3,?4,?5,?6)", params![pulse,revision,description,json,review,now])?;
        db.execute("INSERT INTO pulsation_preparation(pulsation_id,input_revision,assessment_json,review_state,prespec_state,skip_reason,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(pulsation_id) DO UPDATE SET input_revision=excluded.input_revision,assessment_json=excluded.assessment_json,review_state=excluded.review_state,prespec_state=excluded.prespec_state,skip_reason=excluded.skip_reason,context_artifact_id=NULL,predecessor_id=NULL,job_id=NULL,consumed_job_id=NULL,context_disposition=NULL,diagnostic=NULL,updated_at=excluded.updated_at",params![pulse,revision,json,review,if settings.preparation_enabled {"not_scheduled"} else {"skipped"},if settings.preparation_enabled {None} else {Some("disabled")},now])?;
        Ok(())
    }

    pub fn revise_pulsation_description(
        &self,
        request: RevisePulsationDescriptionRequest,
    ) -> Result<Pulsation, PersistenceServiceError> {
        request.validate().map_err(|e| invalid(e.to_string()))?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::check_description_action(
            &tx,
            &request.pulsation_id,
            request.expected_input_revision,
        )?;
        let current = Self::find_pulsation_by_id(&tx, &request.pulsation_id)?
            .ok_or_else(|| invalid("Pulsation missing."))?;
        let pulse = &current.id;
        let cancelled = tx.prepare("SELECT id FROM jobs WHERE pulsation_id=?1 AND status IN ('pending','running','interrupted')")?.query_map([pulse], |r| r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        tx.execute("UPDATE jobs SET status='failed',cancellation_reason='superseded',finished_at=?2,updated_at=?2 WHERE pulsation_id=?1 AND status IN ('pending','running','interrupted')", params![pulse,Self::now_iso()])?;
        // Editing a request cannot silently roll back already published legacy Intake output.
        let effects: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM artifacts WHERE pulsation_id=?1 AND publication_state IN ('applied','applying','conflict'))",[pulse],|r|r.get(0))?;
        if effects {
            return Err(invalid(
                "Há efeitos publicados. Cancele/reconcilie esta pulsação e crie outro pedido.",
            ));
        }
        tx.execute("UPDATE artifact_publications SET state='cancelled' WHERE pulsation_id=?1 AND state='pending'",[pulse])?;
        tx.execute(
            "UPDATE artifacts SET acceptance='superseded' WHERE pulsation_id=?1",
            [pulse],
        )?;
        tx.execute(
            "UPDATE pulsations SET feature_name=?2,input_revision=input_revision+1 WHERE id=?1",
            params![pulse, request.description],
        )?;
        let updated = if current.status == PulsationStatus::Planning {
            tx.execute("UPDATE pulsations SET stage='Intake',state_version=state_version+1,updated_at=?2 WHERE id=?1",params![pulse,Self::now_iso()])?;
            tx.execute("INSERT INTO pulse_events(id,pulsation_id,event_type,from_status,to_status,from_stage,to_stage,message,created_at) VALUES(?1,?2,'description_revised','planning','planning',?3,'Intake','Pedido editado; geração anterior superada.',?4)",params![Uuid::new_v4().to_string(),pulse,current.stage.as_db_value(),Self::now_iso()])?;
            Self::find_pulsation_by_id(&tx, pulse)?.ok_or_else(|| invalid("Pulsation missing."))?
        } else {
            Self::transition_pulsation(
                &tx,
                &current,
                PulsationStatus::Planning,
                PulsationStage::Intake,
                "description_revised",
                Some("Pedido editado; geração anterior superada."),
            )?
        };
        Self::assess_description(
            &tx,
            pulse,
            &request.description,
            request.expected_input_revision + 1,
        )?;
        Self::enqueue_stage_job(&tx, pulse, PulsationStage::Intake)?;
        tx.commit()?;
        self.signal_cancelled_jobs(&cancelled);
        self.wake_scheduler();
        Ok(Pulsation {
            feature_name: request.description,
            ..updated
        })
    }

    fn check_description_action(
        db: &Connection,
        pulse: &str,
        revision: u64,
    ) -> Result<(), PersistenceServiceError> {
        let valid: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pulsations WHERE id=?1 AND input_revision=?2 AND specification_started=0 AND status IN ('planning','running') AND stage IN ('Intake','SPECIFICATION'))",params![pulse,revision],|r|r.get(0))?;
        if !valid {
            return Err(invalid(
                "Revisão obsoleta ou Specification já iniciada. Atualize; mudanças no pedido após o início exigem nova pulsação.",
            ));
        }
        Ok(())
    }

    pub fn confirm_description_review(
        &self,
        request: ConfirmDescriptionReviewRequest,
    ) -> Result<(), PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::check_description_action(
            &tx,
            &request.pulsation_id,
            request.expected_input_revision,
        )?;
        if tx.execute("UPDATE pulsation_preparation SET review_state='confirmed',updated_at=?3 WHERE pulsation_id=?1 AND input_revision=?2 AND review_state='pending'",params![request.pulsation_id,request.expected_input_revision,Self::now_iso()])? != 1 { return Err(invalid("Revisão já confirmada ou indisponível.")); }
        tx.execute("UPDATE pulsation_description_revisions SET review_state='confirmed',confirmed_at=?3 WHERE pulsation_id=?1 AND input_revision=?2",params![request.pulsation_id,request.expected_input_revision,Self::now_iso()])?;
        tx.commit()?;
        self.wake_scheduler();
        Ok(())
    }

    pub(super) fn signal_cancelled_jobs(&self, ids: &[String]) {
        let tokens = self
            .runtime_control
            .cancellations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for id in ids {
            if let Some(token) = tokens.get(id) {
                token.cancel();
            }
        }
    }

    pub(super) fn invalidate_preparation(
        db: &Connection,
        pulse: &str,
        reason: &str,
    ) -> Result<Vec<String>, PersistenceServiceError> {
        let ids = db.prepare("SELECT id FROM jobs WHERE pulsation_id=?1 AND kind='prepare_context' AND status IN ('pending','running','interrupted')")?.query_map([pulse],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        db.execute("UPDATE jobs SET status='failed',cancellation_reason='superseded',finished_at=?2,updated_at=?2 WHERE pulsation_id=?1 AND kind='prepare_context' AND status IN ('pending','running','interrupted')",params![pulse,Self::now_iso()])?;
        db.execute("UPDATE pulsation_preparation SET prespec_state='cancelled',skip_reason=?2,updated_at=?3 WHERE pulsation_id=?1 AND consumed_job_id IS NULL AND (prespec_state IN ('pending','running') OR ?2='disabled')",params![pulse,reason,Self::now_iso()])?;
        Ok(ids)
    }

    #[cfg(test)]
    pub(super) fn coordination_details(
        db: &Connection,
        project: &str,
    ) -> Result<Vec<PulsationCoordinationSnapshot>, PersistenceServiceError> {
        Self::coordination_details_for(db, project, None)
    }

    /// Rich coordination data contains per-pulsation artifacts, Learning and composition reads.
    /// Runtime chrome needs it only for the active pulsation; the unfiltered wrapper remains for
    /// explicit persistence callers and test fixtures.
    pub(super) fn coordination_details_for(
        db: &Connection,
        project: &str,
        pulsation_id: Option<&str>,
    ) -> Result<Vec<PulsationCoordinationSnapshot>, PersistenceServiceError> {
        let rows = db.prepare("SELECT p.id,p.queue_sequence,p.pipeline_version,p.input_revision,p.state_version,p.stage,p.status,p.specification_started,r.assessment_json,r.review_state,r.prespec_state,r.context_artifact_id,r.skip_reason,r.context_disposition,r.diagnostic FROM pulsations p LEFT JOIN pulsation_preparation r ON r.pulsation_id=p.id AND r.input_revision=p.input_revision WHERE p.project_id=?1 AND (?2 IS NULL OR p.id=?2) ORDER BY p.queue_sequence")?.query_map(params![project, pulsation_id],|r| Ok((r.get::<_,String>(0)?,r.get::<_,u64>(1)?,r.get::<_,u8>(2)?,r.get::<_,u64>(3)?,r.get::<_,u64>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,bool>(7)?,r.get::<_,Option<String>>(8)?,r.get::<_,Option<String>>(9)?,r.get::<_,Option<String>>(10)?,r.get::<_,Option<String>>(11)?,r.get::<_,Option<String>>(12)?,r.get::<_,Option<String>>(13)?,r.get::<_,Option<String>>(14)?)))?.collect::<Result<Vec<_>,_>>()?;
        let mut result = Vec::new();
        for (
            pulse,
            sequence,
            version,
            revision,
            state,
            stage,
            status,
            started,
            assessment,
            review,
            preparation,
            artifact,
            skip,
            disposition,
            diagnostic,
        ) in rows
        {
            let preparation: PreSpecState = serde_json::from_value(serde_json::json!(
                preparation.as_deref().unwrap_or("not_scheduled")
            ))
            .map_err(|e| invalid(e.to_string()))?;
            let description_review: DescriptionReviewState = serde_json::from_value(
                serde_json::json!(review.as_deref().unwrap_or("not_required")),
            )
            .map_err(|e| invalid(e.to_string()))?;
            let skip_reason = skip
                .map(|s| serde_json::from_value(serde_json::json!(s)))
                .transpose()
                .map_err(|e| invalid(e.to_string()))?;
            let stage = catalog::parse_stage(&stage).ok_or_else(|| invalid("Estágio inválido."))?;
            let status = PulsationStatus::from_db_value(&status)
                .ok_or_else(|| invalid("Status inválido."))?;
            let artifacts = db
                .prepare("SELECT id FROM artifacts WHERE pulsation_id=?1 ORDER BY created_at,id")?
                .query_map([&pulse], |r| r.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            let (learning, learning_run) = Self::learning_snapshot(db, &pulse)?;
            let stage_execution = if crate::orchestration::PipelineVersion::from_db(version)
                .is_some_and(|v| v.supports_composition())
            {
                match Self::latest_execution_for_pulsation(db, &pulse, None)? {
                    Some(execution) => Some(Self::execution_snapshot(db, &execution)?),
                    None => None,
                }
            } else {
                None
            };
            result.push(PulsationCoordinationSnapshot {
                description_editable: !started
                    && matches!(status, PulsationStatus::Planning | PulsationStatus::Running),
                context_disposition: disposition,
                diagnostic,
                pulsation_id: pulse.clone(),
                project_id: project.into(),
                queue_sequence: sequence,
                pipeline_version: version,
                input_revision: revision,
                state_version: state,
                stage,
                status,
                blocking_reason: if description_review == DescriptionReviewState::Pending {
                    Some("aguardando revisão do pedido".into())
                } else {
                    None
                },
                preparation: PreparationSnapshot {
                    assessment: assessment
                        .map(|s| serde_json::from_str(&s))
                        .transpose()
                        .map_err(|e| invalid(e.to_string()))?,
                    description_review,
                    prespec_state: preparation,
                    context_artifact_id: artifact,
                    skip_reason,
                },
                questions: Self::pending_questions(db, &pulse)?,
                learning,
                learning_run,
                artifact_ids: artifacts,
                stage_execution,
            });
        }
        Ok(result)
    }
}
