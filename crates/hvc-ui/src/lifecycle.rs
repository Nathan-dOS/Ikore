/// Coalesces immutable revision reads without cancelling a worker already in flight.
#[derive(Debug, Default)]
pub(crate) struct RevisionRead {
    desired: Option<String>,
    running: Option<String>,
    loaded: Option<String>,
    invalidated: bool,
}

impl RevisionRead {
    pub fn want(&mut self, key: String) {
        self.desired = Some(key);
    }

    pub fn invalidate(&mut self) {
        self.loaded = None;
        self.invalidated = true;
    }

    pub fn begin(&mut self) -> Option<String> {
        if self.running.is_some() || self.loaded == self.desired {
            return None;
        }
        let key = self.desired.clone()?;
        self.invalidated = false;
        self.running = Some(key.clone());
        Some(key)
    }

    pub fn finish(&mut self, key: &str, success: bool) -> bool {
        if self.running.as_deref() != Some(key) {
            return false;
        }
        self.running = None;
        let current = self.desired.as_deref() == Some(key) && !self.invalidated;
        if success && current {
            self.loaded = Some(key.to_owned());
        }
        current
    }
}

#[derive(Debug, Default)]
pub(crate) struct LifecycleState {
    disposed: bool,
    poll_in_flight: bool,
    completed_polls: u64,
    listener_count: usize,
    polling_active: bool,
}

impl LifecycleState {
    pub(crate) fn start(&mut self, listener_count: usize) {
        self.disposed = false;
        self.listener_count = listener_count;
        self.polling_active = true;
    }

    pub(crate) fn try_begin_poll(&mut self) -> bool {
        if self.disposed || !self.polling_active || self.poll_in_flight {
            return false;
        }
        self.poll_in_flight = true;
        true
    }

    pub(crate) fn finish_poll(&mut self) {
        if self.poll_in_flight {
            self.poll_in_flight = false;
            self.completed_polls = self.completed_polls.saturating_add(1);
        }
    }

    pub(crate) fn dispose(&mut self) {
        self.disposed = true;
        self.poll_in_flight = false;
        self.listener_count = 0;
        self.polling_active = false;
    }

    pub(crate) fn is_disposed(&self) -> bool {
        self.disposed
    }

    #[cfg(test)]
    pub(crate) fn completed_polls(&self) -> u64 {
        self.completed_polls
    }

    #[cfg(test)]
    pub(crate) fn registered_resources(&self) -> usize {
        self.listener_count + usize::from(self.polling_active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_reads_coalesce_navigation_and_discard_outdated_results() {
        let mut read = RevisionRead::default();
        read.want("project-a/pulse-1/r1".into());
        let old = read.begin().unwrap();
        read.want("project-b/pulse-2/r2".into());
        assert!(read.begin().is_none());
        assert!(!read.finish(&old, true));
        let current = read.begin().unwrap();
        assert!(read.finish(&current, true));
        assert!(read.begin().is_none());
        read.invalidate();
        let stale = read.begin().unwrap();
        read.invalidate();
        assert!(!read.finish(&stale, true));
        assert!(read.begin().is_some());
    }

    #[test]
    fn polls_without_overlap_and_disposes_resources() {
        let mut lifecycle = LifecycleState::default();
        lifecycle.start(1);

        assert_eq!(lifecycle.registered_resources(), 2);
        assert!(lifecycle.try_begin_poll());
        assert!(!lifecycle.try_begin_poll());
        lifecycle.finish_poll();
        assert_eq!(lifecycle.completed_polls(), 1);
        assert!(lifecycle.try_begin_poll());

        lifecycle.dispose();
        assert!(lifecycle.is_disposed());
        assert_eq!(lifecycle.registered_resources(), 0);
        assert!(!lifecycle.try_begin_poll());
    }
}
