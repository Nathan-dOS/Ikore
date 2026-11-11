use std::sync::atomic::{AtomicUsize, Ordering};

use ikore_core::llm::{
    ConnectionId, GatewayFuture, LlmError, LlmErrorKind, LlmGateway, LlmRequest, LlmResponse,
    LlmUsage, ProviderId, RouteId,
};
use serde_json::json;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// A controllable response boundary with no sleeps, network or credentials.
/// Tests await entry, change the host revision/cancel, then release the response.
pub struct ControlledLlmGateway {
    content: String,
    calls: AtomicUsize,
    entered: Semaphore,
    release: Semaphore,
}

impl ControlledLlmGateway {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            calls: AtomicUsize::new(0),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        }
    }

    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub async fn wait_until_called(&self) {
        self.entered
            .acquire()
            .await
            .expect("entry gate stays open")
            .forget();
    }

    pub fn release_next(&self) {
        self.release.add_permits(1);
    }
}

impl LlmGateway for ControlledLlmGateway {
    fn execute<'a>(
        &'a self,
        request: &'a LlmRequest,
        cancellation: &'a CancellationToken,
    ) -> GatewayFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.add_permits(1);
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    return Err(LlmError::new(LlmErrorKind::Cancelled, "Fixture cancelada."));
                }
                permit = self.release.acquire() => {
                    permit.expect("response gate stays open").forget();
                }
            }
            Ok(LlmResponse {
                execution_id: request.execution_id.clone(),
                generation_id: request.generation_id.clone(),
                route_id: RouteId::new("fixture-route"),
                connection_id: ConnectionId::new("fixture-connection"),
                provider_id: ProviderId::new("fixture-provider"),
                model: "fixture-model".into(),
                request_payload: json!({"stage": request.stage}),
                response_payload: json!({"fixture": true}),
                content: self.content.clone(),
                usage: LlmUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(5),
                },
                estimated_cost_usd: Some(0.0),
                finish_reason: Some("stop".into()),
            })
        })
    }
}
