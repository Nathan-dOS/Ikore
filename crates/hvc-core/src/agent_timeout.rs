use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::llm::AgentRole;

/// How much longer a reasoning turn may stay silent than an ordinary one. The
/// absolute ceiling still applies, so this widens the window without ever
/// letting a genuinely stuck process run indefinitely.
const REASONING_SILENCE_FACTOR: u64 = 3;

/// Política de timeout configurada por papel de agente com suporte a streaming progressivo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTimeoutPolicy {
    /// Prazo inicial concedido antes do primeiro retorno ou atividade.
    pub base_timeout_ms: u64,
    /// Janela máxima de silêncio (inatividade) permitida entre eventos de saída.
    pub inactivity_window_ms: u64,
    /// Teto máximo absoluto de segurança que a execução jamais pode ultrapassar.
    pub max_timeout_ms: u64,
    /// Incremento adicionado ao prazo ativo a cada entrega/evento de progresso recebido.
    pub extension_increment_ms: u64,
    /// Janela de silêncio tolerada enquanto o modelo está raciocinando.
    ///
    /// Modelos de raciocínio não emitem nada entre o início do turno e a
    /// primeira palavra da resposta: o processo está vivo e gastando tokens,
    /// mas o stream fica mudo. Medir esse silêncio com a mesma régua de um
    /// processo travado matou execuções saudáveis exatamente na janela padrão.
    pub reasoning_silence_window_ms: u64,
}

impl Default for AgentTimeoutPolicy {
    fn default() -> Self {
        Self::new(
            180_000, // 3 min base
            90_000,  // 90s janela de inatividade
            900_000, // 15 min teto máximo
            30_000,  // +30s por entrega
        )
    }
}

impl AgentTimeoutPolicy {
    pub const fn new(
        base_timeout_ms: u64,
        inactivity_window_ms: u64,
        max_timeout_ms: u64,
        extension_increment_ms: u64,
    ) -> Self {
        Self {
            base_timeout_ms,
            inactivity_window_ms,
            max_timeout_ms,
            extension_increment_ms,
            reasoning_silence_window_ms: inactivity_window_ms
                .saturating_mul(REASONING_SILENCE_FACTOR),
        }
    }

    pub fn base_timeout(&self) -> Duration {
        Duration::from_millis(self.base_timeout_ms)
    }

    pub fn inactivity_window(&self) -> Duration {
        Duration::from_millis(self.inactivity_window_ms)
    }

    pub fn reasoning_silence_window(&self) -> Duration {
        Duration::from_millis(self.reasoning_silence_window_ms)
    }

    pub fn max_timeout(&self) -> Duration {
        Duration::from_millis(self.max_timeout_ms)
    }

    pub fn extension_increment(&self) -> Duration {
        Duration::from_millis(self.extension_increment_ms)
    }

    /// Retorna a política padrão recomendada para o papel do agente informado.
    pub fn for_role(role: AgentRole) -> Self {
        match role {
            AgentRole::FeatureAgent => Self::new(
                300_000, // 5 min base (planejamento denso, esquemas grandes e thinking tokens)
                90_000,  // 90s silêncio máximo
                900_000, // 15 min teto máximo
                30_000,  // +30s a cada frame/delta recebido
            ),
            AgentRole::BackendAgent => Self::new(
                360_000,   // 6 min base (geração de código)
                90_000,    // 90s silêncio
                1_200_000, // 20 min teto máximo
                30_000,
            ),
            AgentRole::TestAgent => Self::new(
                360_000,   // 6 min base (geração de suítes de testes)
                90_000,    // 90s silêncio
                1_200_000, // 20 min teto máximo
                30_000,
            ),
            AgentRole::TaskSpecifier => Self::new(
                240_000, // 4 min base (especificação técnica de lote)
                90_000,  // 90s silêncio
                900_000, // 15 min teto máximo
                30_000,
            ),
            AgentRole::ContractAgent => Self::new(
                240_000, // 4 min base (contratos e interfaces)
                90_000,  // 90s silêncio
                900_000, // 15 min teto máximo
                30_000,
            ),
            AgentRole::Integrator => Self::new(
                240_000, // 4 min base (reconciliação e publicação)
                90_000,  // 90s silêncio
                900_000, // 15 min teto máximo
                30_000,
            ),
            AgentRole::LearningAgent => Self::new(
                240_000, // 4 min base (síntese do grafo de conhecimento)
                90_000,  // 90s silêncio
                900_000, // 15 min teto máximo
                30_000,
            ),
            AgentRole::IntakeRouter => Self::new(
                180_000, // 3 min base (triagem inicial)
                90_000, // 90s silêncio (alinhado aos demais papéis; CLIs retentam 5xx upstream com backoff que pode passar de 60s)
                600_000, // 10 min teto máximo
                30_000,
            ),
        }
    }

    /// Política para interações do Companion (Chat).
    pub fn companion() -> Self {
        Self::new(
            120_000, // 2 min base
            60_000,  // 60s silêncio
            600_000, // 10 min teto máximo
            30_000,
        )
    }

    /// Política para execuções manuais ou FreeForm.
    pub fn manual() -> Self {
        Self::new(
            1_800_000, // 30 min base
            120_000,   // 2 min silêncio
            3_600_000, // 60 min teto máximo
            60_000,
        )
    }
}

/// Razão específica de encerramento por timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutReason {
    /// O processo ficou em silêncio além da janela permitida sem produzir saída.
    Inactivity,
    /// O processo excedeu o limite máximo absoluto permitido para o papel do agente.
    MaxCeiling,
}

impl TimeoutReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inactivity => "cli_inactivity_timeout",
            Self::MaxCeiling => "cli_timeout",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Inactivity => "O processo CLI ficou inativo além do limite sem produzir saída.",
            Self::MaxCeiling => {
                "O processo CLI excedeu o tempo limite máximo permitido para a operação."
            }
        }
    }
}

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
        let now = Instant::now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.last_activity_at = now;
        state.progress_count = state.progress_count.saturating_add(1);
        state.reasoning = false;
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
        let now = Instant::now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.last_activity_at = now;
        state.progress_count = state.progress_count.saturating_add(1);
        state.reasoning = true;
        let extended = state.current_deadline + state.policy.extension_increment();
        state.current_deadline = extended.min(state.max_deadline);
        drop(state);
        self.activity_notify.notify_waiters();
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
    /// 90s each, right after OpenCode emitted `step_start`. The model was
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
