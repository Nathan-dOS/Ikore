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
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Notice => "Info",
            Self::Warning => "Warning",
            Self::Error => "Error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeaderNoticeAction {
    pub(crate) label: SharedString,
    pub(crate) pulsation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeaderNotice {
    pub(crate) id: u64,
    pub(crate) kind: HeaderNoticeKind,
    pub(crate) message: String,
    pub(crate) action: Option<HeaderNoticeAction>,
    pub(crate) project: Option<String>,
    pub(crate) source: String,
    pub(crate) created: std::time::SystemTime,
    pub(crate) repeats: usize,
    pub(crate) read: bool,
    pending: bool,
}

#[derive(Debug, Default)]
pub(crate) struct HeaderNoticeQueue {
    entries: VecDeque<HeaderNotice>,
    project: Option<String>,
    next_id: u64,
    pub(crate) pinned: Option<u64>,
}

impl HeaderNoticeQueue {
    pub(crate) fn set_project(&mut self, project: Option<String>) {
        if self.project != project {
            self.pinned = None;
        }
        self.project = project;
    }

    #[cfg(test)]
    #[track_caller]
    pub(crate) fn push(&mut self, kind: HeaderNoticeKind, message: impl Into<String>) {
        self.push_actionable(kind, message, None);
    }

    #[track_caller]
    pub(crate) fn push_actionable(
        &mut self,
        kind: HeaderNoticeKind,
        message: impl Into<String>,
        action: Option<HeaderNoticeAction>,
    ) {
        let caller = std::panic::Location::caller();
        self.push_from(
            format!("{}:{}", caller.file(), caller.line()),
            kind,
            message.into(),
            action,
        );
    }

    pub(crate) fn push_from(
        &mut self,
        source: String,
        kind: HeaderNoticeKind,
        message: String,
        action: Option<HeaderNoticeAction>,
    ) {
        if message.trim().is_empty() {
            return;
        }
        if let Some(index) = self.entries.iter().rposition(|n| {
            n.project == self.project
                && n.source == source
                && n.kind == kind
                && n.message == message
                && n.action == action
        }) {
            let existing = &mut self.entries[index];
            existing.repeats += 1;
            existing.read = false;
            existing.created = std::time::SystemTime::now();
            if !existing.pending {
                let mut repeated = self.entries.remove(index).unwrap();
                repeated.pending = true;
                self.entries.push_back(repeated);
            }
            return;
        }
        self.next_id += 1;
        self.entries.push_back(HeaderNotice {
            id: self.next_id,
            kind,
            message,
            action,
            project: self.project.clone(),
            source,
            created: std::time::SystemTime::now(),
            repeats: 1,
            read: false,
            pending: true,
        });
        if self.entries.len() > 100
            && let Some(index) = self.entries.iter().position(|n| Some(n.id) != self.pinned)
        {
            self.entries.remove(index);
        }
    }

    /// Records a notice in the history (unread) without raising a toast — for messages the
    /// surface in front of the developer already shows, like the Settings status line.
    pub(crate) fn push_quiet(&mut self, source: String, kind: HeaderNoticeKind, message: String) {
        if message.trim().is_empty() {
            return;
        }
        if let Some(existing) = self.entries.iter_mut().rev().find(|n| {
            n.project == self.project
                && n.source == source
                && n.kind == kind
                && n.message == message
                && n.action.is_none()
        }) {
            existing.repeats += 1;
            existing.read = false;
            existing.created = std::time::SystemTime::now();
            return;
        }
        self.next_id += 1;
        self.entries.push_back(HeaderNotice {
            id: self.next_id,
            kind,
            message,
            action: None,
            project: self.project.clone(),
            source,
            created: std::time::SystemTime::now(),
            repeats: 1,
            read: false,
            pending: false,
        });
        if self.entries.len() > 100
            && let Some(index) = self.entries.iter().position(|n| Some(n.id) != self.pinned)
        {
            self.entries.remove(index);
        }
    }

    pub(crate) fn current(&self) -> Option<&HeaderNotice> {
        self.entries
            .iter()
            .find(|n| n.project == self.project && n.pending)
    }

    pub(crate) fn dismiss(&mut self) -> Option<HeaderNotice> {
        let n = self
            .entries
            .iter_mut()
            .find(|n| n.project == self.project && n.pending)?;
        n.pending = false;
        self.pinned = None;
        Some(n.clone())
    }

    pub(crate) fn history(&self) -> Vec<HeaderNotice> {
        self.entries
            .iter()
            .rev()
            .filter(|n| n.project == self.project)
            .cloned()
            .collect()
    }

    pub(crate) fn unread(&self) -> usize {
        self.history().iter().filter(|n| !n.read).count()
    }
    pub(crate) fn mark_read(&mut self) {
        self.pinned = None;
        for n in &mut self.entries {
            if n.project == self.project {
                n.read = true;
                n.pending = false;
            }
        }
    }

    pub(crate) fn has_pending_global_error(&self, message: &str) -> bool {
        self.entries.iter().any(|n| {
            n.project == self.project
                && n.pending
                && n.kind == HeaderNoticeKind::Error
                && n.message == message
                && n.source.contains(".rs:")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_notice_reaches_the_history_without_a_toast() {
        let mut queue = HeaderNoticeQueue::default();
        queue.push_quiet(
            "Settings".into(),
            HeaderNoticeKind::Notice,
            "Route saved.".into(),
        );
        assert!(queue.current().is_none());
        assert_eq!(queue.unread(), 1);
        queue.push_quiet(
            "Settings".into(),
            HeaderNoticeKind::Notice,
            "Route saved.".into(),
        );
        let history = queue.history();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].repeats, 2);
        assert!(queue.current().is_none());
    }

    #[test]
    fn repeated_dismissed_events_rejoin_the_queue_without_replacing_the_current_card() {
        let mut q = HeaderNoticeQueue::default();
        q.push_from(
            "save".into(),
            HeaderNoticeKind::Error,
            "Failed".into(),
            None,
        );
        q.dismiss();
        q.push_from(
            "load".into(),
            HeaderNoticeKind::Notice,
            "Loaded".into(),
            None,
        );
        q.push_from(
            "save".into(),
            HeaderNoticeKind::Error,
            "Failed".into(),
            None,
        );
        assert_eq!(q.current().unwrap().message, "Loaded");
        q.dismiss();
        assert_eq!(q.current().unwrap().repeats, 2);
        assert_eq!(q.history().len(), 2);
        q.pinned = q.current().map(|n| n.id);
        for i in 0..110 {
            q.push(HeaderNoticeKind::Notice, format!("Event {i}"));
        }
        assert_eq!(q.current().unwrap().message, "Failed");
        assert_eq!(q.history().len(), 100);
    }
    #[test]
    fn notifications_are_fifo_and_dismissal_preserves_history() {
        let mut q = HeaderNoticeQueue::default();
        q.push(HeaderNoticeKind::Notice, "Saved");
        q.push(HeaderNoticeKind::Error, "Failed");
        assert_eq!(q.current().unwrap().message, "Saved");
        q.dismiss();
        assert_eq!(q.current().unwrap().message, "Failed");
        assert_eq!(q.history().len(), 2);
        assert_eq!(q.unread(), 2);
        q.mark_read();
        assert_eq!(q.unread(), 0);
        assert!(q.current().is_none());
    }
    #[test]
    fn history_is_bounded_and_isolated_and_deduplicates_by_destination() {
        let mut q = HeaderNoticeQueue::default();
        q.set_project(Some("a".into()));
        for _ in 0..3 {
            q.push_from(
                "save".into(),
                HeaderNoticeKind::Notice,
                "Saved".into(),
                None,
            );
        }
        assert_eq!(q.history()[0].repeats, 3);
        for id in ["one", "two"] {
            q.push_from(
                "review".into(),
                HeaderNoticeKind::Warning,
                "Review".into(),
                Some(HeaderNoticeAction {
                    label: "Review now".into(),
                    pulsation_id: id.into(),
                }),
            );
        }
        assert_eq!(q.history().len(), 3);
        q.set_project(Some("b".into()));
        assert!(q.current().is_none());
        for i in 0..110 {
            q.push(HeaderNoticeKind::Notice, format!("Event {i}"));
        }
        assert_eq!(q.history().len(), 100);
        assert_eq!(q.history().last().unwrap().message, "Event 10");
        q.set_project(Some("a".into()));
        assert!(q.history().is_empty());
    }
}
