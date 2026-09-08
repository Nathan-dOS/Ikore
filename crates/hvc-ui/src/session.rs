use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use gpui::SharedString;
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
                .map_err(|error| format!("persistence worker interrupted: {error}"))?
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

/// A one-click action attached to a notice (currently only the "review(s) waiting" notice uses
/// this — see the UI flow plan Part 4.1). Kept narrow and independent of `flow::FlowTarget` so
/// `HeaderNotice` can keep deriving `PartialEq`/`Eq` for the existing dedup logic below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeaderNoticeAction {
    pub(crate) label: SharedString,
    pub(crate) pulsation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeaderNotice {
    pub(crate) kind: HeaderNoticeKind,
    pub(crate) message: String,
    pub(crate) action: Option<HeaderNoticeAction>,
}

#[derive(Debug, Default)]
pub(crate) struct HeaderNoticeQueue {
    entries: VecDeque<HeaderNotice>,
}

impl HeaderNoticeQueue {
    pub(crate) fn push(&mut self, kind: HeaderNoticeKind, message: impl Into<String>) {
        self.push_actionable(kind, message, None);
    }

    pub(crate) fn push_actionable(
        &mut self,
        kind: HeaderNoticeKind,
        message: impl Into<String>,
        action: Option<HeaderNoticeAction>,
    ) {
        let message = message.into();
        if message.trim().is_empty()
            || self
                .entries
                .iter()
                .any(|entry| entry.kind == kind && entry.message == message)
        {
            return;
        }
        let notice = HeaderNotice {
            kind,
            message,
            action,
        };
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
        queue.push(HeaderNoticeKind::Notice, "Workspace saved");
        queue.push(HeaderNoticeKind::Warning, "Database unavailable");
        queue.push(HeaderNoticeKind::Error, "Runtime failure");

        assert_eq!(queue.current().unwrap().message, "Runtime failure");
        queue.dismiss();
        assert_eq!(queue.current().unwrap().message, "Database unavailable");
        queue.dismiss();
        assert_eq!(queue.current().unwrap().message, "Workspace saved");
    }
}
