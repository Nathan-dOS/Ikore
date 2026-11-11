use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use ikore_contracts::timeout::{AgentTimeoutPolicy, CliTimeoutBudget, TimeoutReason};

#[cfg(test)]
use ikore_contracts::llm::AgentRole;

impl WatchdogState {
    /// The silence the run may currently sit in without being killed.
    fn silence_window(&self) -> Duration {
        if self.reasoning {
            self.policy.reasoning_silence_window()
        } else {
            self.policy.inactivity_window()
        }
    }
}

#[derive(Debug)]
struct WatchdogState {
    policy: AgentTimeoutPolicy,
    _started_at: Instant,
    last_activity_at: Instant,
    current_deadline: Instant,
    max_deadline: Instant,
    progress_count: u64,
    /// Set while the provider has told us the model is thinking and cleared by
    /// the next real delivery.
    reasoning: bool,
}

/// Monitor de progresso que ajusta dinamicamente os prazos conforme saídas do processo são entregues.
#[derive(Debug, Clone)]
pub struct ProgressWatchdog {
    state: Arc<Mutex<WatchdogState>>,
    activity_notify: Arc<tokio::sync::Notify>,
}

impl ProgressWatchdog {
    pub fn new(policy: AgentTimeoutPolicy) -> Self {
        let now = Instant::now();
        let current_deadline = now + policy.base_timeout();
        let max_deadline = now + policy.max_timeout();
        Self {
            state: Arc::new(Mutex::new(WatchdogState {
                policy,
                _started_at: now,
                last_activity_at: now,
                current_deadline,
                max_deadline,
                progress_count: 0,
                reasoning: false,
            })),
            activity_notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Registra progresso (entrega de tokens de pensamento, frames de stream, eventos de saída).
    /// Estende o prazo corrente em `extension_increment`, respeitando `max_timeout`, e reinicia
    /// o relógio de inatividade.
    pub fn record_progress(&self) {
        self.record_activity();
        self.set_reasoning(false);
    }

    /// Only transport reads renew liveness. Auxiliary bytes do not end reasoning.
    pub fn record_activity(&self) {
        let now = Instant::now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.last_activity_at = now;
        state.progress_count = state.progress_count.saturating_add(1);
        let extended = state.current_deadline + state.policy.extension_increment();
        state.current_deadline = extended.min(state.max_deadline);
        drop(state);
        self.activity_notify.notify_waiters();
    }

    /// Registra que o provedor informou o início de um turno de raciocínio.
    ///
    /// Conta como progresso - o processo está vivo - e amplia a janela de
    /// silêncio até a próxima entrega real, porque nada será emitido enquanto
    /// o modelo pensa.
    pub fn record_reasoning(&self) {
        self.record_activity();
        self.set_reasoning(true);
    }

    /// Apply a recognized provider transition at receipt, before queueing for audit.
    /// It is not another heartbeat and must not extend the deadline twice.
    pub fn set_reasoning(&self, reasoning: bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.reasoning = reasoning;
        drop(state);
        self.activity_notify.notify_waiters();
    }

    /// Bounded timing metadata only; never includes provider output or prompts.
    pub fn diagnostic(&self) -> String {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        format!(
            "base_ms={} ceiling_ms={} silence_ms={} reasoning={} last_read_age_ms={} progress_reads={} elapsed_ms={}",
            state.policy.base_timeout_ms,
            state.policy.max_timeout_ms,
            state.silence_window().as_millis(),
            state.reasoning,
            now.saturating_duration_since(state.last_activity_at)
                .as_millis(),
            state.progress_count,
            now.saturating_duration_since(state._started_at).as_millis()
        )
    }

    /// Retorna a contagem de eventos de progresso registrados.
    pub fn progress_count(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .progress_count
    }

    /// Retorna o prazo corrente ativo.
    pub fn current_deadline(&self) -> Instant {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .current_deadline
    }

    /// Retorna o teto máximo absoluto.
    pub fn max_deadline(&self) -> Instant {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .max_deadline
    }

    /// Aguarda assincronamente até que ocorra inatividade excessiva ou o teto máximo seja atingido.
    pub async fn wait_for_timeout(&self) -> TimeoutReason {
        loop {
            let (target_instant, reason_if_fired) = {
                let state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let now = Instant::now();
                if now >= state.max_deadline {
                    return TimeoutReason::MaxCeiling;
                }
                let inactivity_deadline = state.last_activity_at + state.silence_window();
                if now >= inactivity_deadline {
                    return TimeoutReason::Inactivity;
                }
                if now >= state.current_deadline {
                    return TimeoutReason::Inactivity;
                }
                let next_deadline = state
                    .current_deadline
                    .min(inactivity_deadline)
                    .min(state.max_deadline);
                let reason = if next_deadline >= state.max_deadline {
                    TimeoutReason::MaxCeiling
                } else {
                    TimeoutReason::Inactivity
                };
                (next_deadline, reason)
            };

            tokio::select! {
                _ = self.activity_notify.notified() => {
                    // Novo progresso recebido, recalcula o timer
                    continue;
                }
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(target_instant)) => {
                    let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let now = Instant::now();
                    if now >= state.max_deadline {
                        return TimeoutReason::MaxCeiling;
                    }
                    if now >= state.last_activity_at + state.silence_window() || now >= state.current_deadline {
                        return TimeoutReason::Inactivity;
                    }
                    if reason_if_fired == TimeoutReason::MaxCeiling {
                        return TimeoutReason::MaxCeiling;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_budget_separates_initial_timeout_from_absolute_ceiling() {
        let before = Instant::now();
        let budget = CliTimeoutBudget::for_role(AgentRole::TaskSpecifier, 180_000);
        assert_eq!(budget.policy.base_timeout_ms, 240_000);
        assert_eq!(budget.policy.max_timeout_ms, 900_000);
        assert!(budget.deadline >= before + Duration::from_secs(900));
        let bounded = CliTimeoutBudget::for_role(AgentRole::IntakeRouter, u64::MAX);
        assert_eq!(
            bounded.policy.base_timeout_ms,
            bounded.policy.max_timeout_ms
        );
    }

    #[tokio::test]
    async fn a_progressing_cli_exceeds_base_but_still_hits_absolute_ceiling() {
        let watchdog = ProgressWatchdog::new(AgentTimeoutPolicy::new(80, 250, 350, 80));
        let progress = watchdog.clone();
        let task = tokio::spawn(async move {
            loop {
                progress.record_activity();
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        let reason = watchdog.wait_for_timeout().await;
        task.abort();
        assert_eq!(reason, TimeoutReason::MaxCeiling);
        assert!(watchdog.progress_count() > 1);
    }

    #[test]
    fn test_role_default_policies() {
        let feature = AgentTimeoutPolicy::for_role(AgentRole::FeatureAgent);
        assert_eq!(feature.base_timeout_ms, 300_000);
        assert_eq!(feature.inactivity_window_ms, 90_000);
        assert_eq!(feature.max_timeout_ms, 900_000);
        assert_eq!(feature.extension_increment_ms, 30_000);

        let backend = AgentTimeoutPolicy::for_role(AgentRole::BackendAgent);
        assert_eq!(backend.base_timeout_ms, 360_000);
        assert_eq!(backend.max_timeout_ms, 1_200_000);

        let intake = AgentTimeoutPolicy::for_role(AgentRole::IntakeRouter);
        assert_eq!(intake.base_timeout_ms, 180_000);
        assert_eq!(intake.inactivity_window_ms, 90_000);
    }

    #[test]
    fn test_progress_extension_within_limits() {
        let policy = AgentTimeoutPolicy::new(10_000, 2_000, 20_000, 5_000);
        let watchdog = ProgressWatchdog::new(policy);
        let initial_deadline = watchdog.current_deadline();

        watchdog.record_progress();
        assert_eq!(watchdog.progress_count(), 1);
        let updated = watchdog.current_deadline();
        assert!(updated > initial_deadline);

        // Multiple extensions should not exceed max_deadline
        for _ in 0..10 {
            watchdog.record_progress();
        }
        assert!(watchdog.current_deadline() <= watchdog.max_deadline());
    }

    #[tokio::test]
    async fn test_watchdog_inactivity_timeout() {
        let policy = AgentTimeoutPolicy::new(10_000, 50, 20_000, 100);
        let watchdog = ProgressWatchdog::new(policy);
        let reason = watchdog.wait_for_timeout().await;
        assert_eq!(reason, TimeoutReason::Inactivity);
    }

    /// Field incident, 2026-09-10: two Integrator runs were killed at exactly
    /// 90s each, right after `OpenCode` emitted `step_start`. The model was
    /// reasoning - it later reported 8,877 reasoning tokens on that turn - and
    /// a reasoning turn streams nothing at all until it is done.
    #[tokio::test]
    async fn a_reasoning_turn_is_given_a_wider_silence_window_than_a_stuck_process() {
        let policy = AgentTimeoutPolicy::for_role(AgentRole::Integrator);
        assert_eq!(policy.inactivity_window_ms, 90_000);
        assert_eq!(policy.reasoning_silence_window_ms, 270_000);

        // A silence the plain window would have killed survives once the
        // provider says the model is thinking.
        let policy = AgentTimeoutPolicy::new(10_000, 40, 20_000, 5);
        let watchdog = ProgressWatchdog::new(policy);
        watchdog.record_reasoning();
        assert!(
            tokio::time::timeout(Duration::from_millis(90), watchdog.wait_for_timeout())
                .await
                .is_err(),
            "a thinking turn must not be killed on the ordinary window"
        );

        // The next real delivery puts the ordinary window back.
        watchdog.record_progress();
        assert_eq!(
            watchdog.wait_for_timeout().await,
            TimeoutReason::Inactivity,
            "once output resumes, silence is measured normally again"
        );
    }

    #[tokio::test]
    async fn test_watchdog_active_streaming_survives() {
        let policy = AgentTimeoutPolicy::new(100, 80, 500, 60);
        let watchdog = ProgressWatchdog::new(policy);
        let watchdog_clone = watchdog.clone();

        let progress_task = tokio::spawn(async move {
            for _ in 0..4 {
                tokio::time::sleep(Duration::from_millis(40)).await;
                watchdog_clone.record_progress();
            }
        });

        progress_task.await.unwrap();
        assert_eq!(watchdog.progress_count(), 4);
    }
}
