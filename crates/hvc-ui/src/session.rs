use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use hvc_core::AppCore;
use hvc_core::persistence_service::SaveUiSessionRequest;

pub(crate) const SESSION_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(600);

pub(crate) type SessionSaveFuture =
    Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'static>>;

pub(crate) trait SessionPersistence: Send + Sync {
    fn save(&self, request: SaveUiSessionRequest) -> SessionSaveFuture;
}

pub(crate) struct CoreSessionPersistence {
    core: Arc<AppCore>,
    serialization: Arc<Mutex<()>>,
}

impl CoreSessionPersistence {
    pub(crate) fn new(core: Arc<AppCore>) -> Self {
        Self {
            core,
            serialization: Arc::new(Mutex::new(())),
        }
    }
}

impl SessionPersistence for CoreSessionPersistence {
    fn save(&self, request: SaveUiSessionRequest) -> SessionSaveFuture {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        let serialization = Arc::clone(&self.serialization);
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    let _guard = serialization
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    core.persistence()
                        .save_ui_session(request)
                        .map(|response| response.updated_at)
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| format!("worker de persistência interrompido: {error}"))?
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeaderNoticeKind {
    Notice,
    Warning,
    Error,
}

impl HeaderNoticeKind {
    fn priority(self) -> u8 {
        match self {
            Self::Notice => 0,
            Self::Warning => 1,
            Self::Error => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeaderNotice {
    pub(crate) kind: HeaderNoticeKind,
    pub(crate) message: String,
}

#[derive(Debug, Default)]
pub(crate) struct HeaderNoticeQueue {
    entries: VecDeque<HeaderNotice>,
}

impl HeaderNoticeQueue {
    pub(crate) fn push(&mut self, kind: HeaderNoticeKind, message: impl Into<String>) {
        let message = message.into();
        if message.trim().is_empty()
            || self
                .entries
                .iter()
                .any(|entry| entry.kind == kind && entry.message == message)
        {
            return;
        }
        let notice = HeaderNotice { kind, message };
        let index = self
            .entries
            .iter()
            .position(|entry| entry.kind.priority() < kind.priority())
            .unwrap_or(self.entries.len());
        self.entries.insert(index, notice);
    }

    pub(crate) fn current(&self) -> Option<&HeaderNotice> {
        self.entries.front()
    }

    pub(crate) fn dismiss(&mut self) -> Option<HeaderNotice> {
        self.entries.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queues_header_errors_warnings_and_notices_by_priority() {
        let mut queue = HeaderNoticeQueue::default();
        queue.push(HeaderNoticeKind::Notice, "Workspace salvo");
        queue.push(HeaderNoticeKind::Warning, "Banco indisponível");
        queue.push(HeaderNoticeKind::Error, "Falha no runtime");

        assert_eq!(queue.current().unwrap().message, "Falha no runtime");
        queue.dismiss();
        assert_eq!(queue.current().unwrap().message, "Banco indisponível");
        queue.dismiss();
        assert_eq!(queue.current().unwrap().message, "Workspace salvo");
    }
}
