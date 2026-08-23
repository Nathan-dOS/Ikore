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
