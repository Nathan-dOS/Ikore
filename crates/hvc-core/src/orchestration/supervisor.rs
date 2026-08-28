//! One AppCore-owned supervisor; futures are joined, never detached lane tasks.
use super::*;
use crate::orchestration::JobClass;

impl PersistenceService {
    pub async fn run_job_worker_loop(&self) {
        self.run_job_worker_until(CancellationToken::new()).await;
    }

    pub async fn run_job_worker_until(&self, cancellation: CancellationToken) {
        tokio::join!(
            self.run_execution_lane(JobClass::Main, &cancellation),
            self.run_execution_lane(JobClass::Background, &cancellation),
        );
    }

    async fn run_execution_lane(&self, class: JobClass, shutdown: &CancellationToken) {
        loop {
            // Register BEFORE inspection so a release between inspection and await is not lost.
            let notified = self.runtime_control.wakeup.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if shutdown.is_cancelled() {
                break;
            }
            let result = match class {
                JobClass::Main => self.process_next_pending_job_until(Some(shutdown)).await,
                JobClass::Background => self.process_anticipation(shutdown).await,
            };
            match result {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => eprintln!("supervisor {:?}: {error}", class),
            }
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = &mut notified => {},
                _ = sleep(Duration::from_secs(1)) => {},
            }
        }
    }

    #[cfg(test)]
    pub(super) async fn process_next_pending_job(&self) -> Result<(), PersistenceServiceError> {
        self.process_next_pending_job_until(None).await.map(|_| ())
    }

    async fn process_next_pending_job_until(
        &self,
        shutdown: Option<&CancellationToken>,
    ) -> Result<bool, PersistenceServiceError> {
        // The blocking host publication is awaited even during shutdown. It checks cancellation
        // between bounded atomic units; aborting spawn_blocking would not stop filesystem writes.
        let service = self.clone();
        let stop = shutdown.cloned();
        let published = tokio::task::spawn_blocking(move || {
            service.reconcile_publications_until(stop.as_ref())
        })
        .await
        .map_err(|e| coordination::invalid(format!("Publicador interrompido: {e}")))??;
        if shutdown.is_some_and(CancellationToken::is_cancelled) {
            return Ok(false);
        }
        let service = self.clone();
        let claim = tokio::task::spawn_blocking(move || service.claim_next_pending_job())
            .await
            .map_err(|e| coordination::invalid(format!("Claim interrompido: {e}")))??;
        let Some((job, cancellation)) = claim else {
            return Ok(published);
        };
        let _slot = self.slot_guard(Self::job_slot_token(&job));
        if shutdown.is_some_and(CancellationToken::is_cancelled) {
            cancellation.cancel();
        }
        let result = self.process_claimed_job(&job, &cancellation).await;
        self.unregister_job_cancellation(&job.id);
        if let Err(error) = &result {
            self.close_abandoned_claim(&job, &error.to_string())?;
        }
        result.map(|_| true)
    }
}
