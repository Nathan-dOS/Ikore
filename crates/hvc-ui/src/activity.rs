//! A small, unified "what just happened" feed for Home. `LogsModel.events` keeps a ring of raw
//! LLM/watcher events for diagnostics; this feed keeps one human-sized line per milestone
//! across every event family (runs finishing, LLM routes, Forge changes, Companion turns,
//! indexing, notices) so the developer can read the last few minutes at a glance.

use std::collections::VecDeque;
use std::time::SystemTime;

use gpui::SharedString;

use crate::flow::FlowTarget;
use crate::icons::Icon;

/// Bounded so a long session never grows the model; Home shows a handful anyway.
pub(crate) const ACTIVITY_CAP: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityKind {
    Pulsation,
    Agent,
    Forge,
    Chat,
    Notes,
    Indexing,
    Workspace,
    Notice,
}

impl ActivityKind {
    pub(crate) const fn icon(self) -> Icon {
        match self {
            Self::Pulsation => Icon::Activity,
            Self::Agent => Icon::Bot,
            Self::Forge => Icon::Hammer,
            Self::Chat => Icon::MessageSquare,
            Self::Notes => Icon::NotebookPen,
            Self::Indexing => Icon::Database,
            Self::Workspace => Icon::FolderOpen,
            Self::Notice => Icon::TriangleAlert,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ActivityEntry {
    pub at: SystemTime,
    pub kind: ActivityKind,
    pub summary: SharedString,
    /// Where a click should take the developer, when there is a natural place.
    pub target: Option<FlowTarget>,
    /// Failure-ish entries are tinted so they stand out in the list.
    pub attention: bool,
}

#[derive(Default)]
pub(crate) struct ActivityFeed {
    entries: VecDeque<ActivityEntry>,
}

impl ActivityFeed {
    pub(crate) fn push(&mut self, entry: ActivityEntry) {
        self.entries.push_front(entry);
        self.entries.truncate(ACTIVITY_CAP);
    }

    /// Newest first.
    pub(crate) fn recent(&self, limit: usize) -> impl Iterator<Item = &ActivityEntry> {
        self.entries.iter().take(limit)
    }

    /// Home only reads `recent`; the size accessors exist for the bound and reset tests.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// A project switch starts a fresh feed, like the notification history.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Collapses bursts: a workspace watcher can emit dozens of changes for one save, and the
    /// feed only needs to say "files changed" once per path within `window`.
    pub(crate) fn push_coalesced(
        &mut self,
        entry: ActivityEntry,
        window: std::time::Duration,
    ) -> bool {
        let duplicate = self.entries.front().is_some_and(|last| {
            last.kind == entry.kind
                && last.summary == entry.summary
                && entry
                    .at
                    .duration_since(last.at)
                    .is_ok_and(|elapsed| elapsed <= window)
        });
        if duplicate {
            return false;
        }
        self.push(entry);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn entry(summary: &str, at: SystemTime) -> ActivityEntry {
        ActivityEntry {
            at,
            kind: ActivityKind::Workspace,
            summary: summary.to_string().into(),
            target: None,
            attention: false,
        }
    }

    #[test]
    fn feed_is_bounded_and_newest_first() {
        let mut feed = ActivityFeed::default();
        let base = SystemTime::UNIX_EPOCH;
        for index in 0..(ACTIVITY_CAP + 5) {
            feed.push(entry(
                &format!("e{index}"),
                base + Duration::from_secs(index as u64),
            ));
        }
        assert_eq!(feed.len(), ACTIVITY_CAP);
        let recent: Vec<&str> = feed.recent(3).map(|e| e.summary.as_ref()).collect();
        assert_eq!(recent, ["e104", "e103", "e102"]);
        feed.clear();
        assert!(feed.is_empty());
    }

    #[test]
    fn coalescing_drops_repeats_inside_the_window_only() {
        let mut feed = ActivityFeed::default();
        let base = SystemTime::UNIX_EPOCH;
        assert!(feed.push_coalesced(entry("src/a.rs changed", base), Duration::from_secs(2)));
        assert!(!feed.push_coalesced(
            entry("src/a.rs changed", base + Duration::from_secs(1)),
            Duration::from_secs(2)
        ));
        assert!(feed.push_coalesced(
            entry("src/b.rs changed", base + Duration::from_secs(1)),
            Duration::from_secs(2)
        ));
        assert!(feed.push_coalesced(
            entry("src/a.rs changed", base + Duration::from_secs(10)),
            Duration::from_secs(2)
        ));
        assert_eq!(feed.len(), 3);
    }
}
