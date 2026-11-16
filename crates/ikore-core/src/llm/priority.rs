//! Per-connection demand covers queued/running foreground work, not only semaphore permits.
use super::Clock;
use chrono::{DateTime, Utc};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

struct Demand {
    foreground: usize,
    last_foreground: DateTime<Utc>,
    learning: Option<(String, CancellationToken)>,
}
pub(crate) struct ConnectionPriority {
    demands: Mutex<HashMap<String, Demand>>,
    clock: Arc<dyn Clock>,
    started_at: DateTime<Utc>,
}
pub(crate) struct DemandGuard {
    priority: Arc<ConnectionPriority>,
    connection: String,
    learning_id: Option<String>,
}
impl ConnectionPriority {
    pub fn new(clock: Arc<dyn Clock>) -> Arc<Self> {
        Arc::new(Self {
            started_at: clock.now(),
            clock,
            demands: Mutex::new(HashMap::new()),
        })
    }
    pub fn ready(&self, connection: &str) -> bool {
        let demands = self
            .demands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let quiet = demands
            .get(connection)
            .map_or(self.started_at, |d| d.last_foreground);
        demands
            .get(connection)
            .is_none_or(|d| d.foreground == 0 && d.learning.is_none())
            && self.clock.now().signed_duration_since(quiet) >= chrono::Duration::seconds(5)
    }
    pub fn foreground(self: &Arc<Self>, connection: &str) -> DemandGuard {
        let mut demands = self
            .demands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let demand = demands.entry(connection.into()).or_insert_with(|| Demand {
            foreground: 0,
            last_foreground: self.started_at,
            learning: None,
        });
        demand.foreground += 1;
        demand.last_foreground = self.clock.now();
        if let Some((_, stop)) = &demand.learning {
            stop.cancel();
        }
        DemandGuard {
            priority: self.clone(),
            connection: connection.into(),
            learning_id: None,
        }
    }
    pub fn learning(
        self: &Arc<Self>,
        connection: &str,
        id: &str,
        stop: CancellationToken,
    ) -> Option<DemandGuard> {
        let mut demands = self
            .demands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let demand = demands.entry(connection.into()).or_insert_with(|| Demand {
            foreground: 0,
            last_foreground: self.started_at,
            learning: None,
        });
        if demand.foreground != 0
            || demand.learning.is_some()
            || self
                .clock
                .now()
                .signed_duration_since(demand.last_foreground)
                < chrono::Duration::seconds(5)
        {
            return None;
        }
        demand.learning = Some((id.into(), stop));
        Some(DemandGuard {
            priority: self.clone(),
            connection: connection.into(),
            learning_id: Some(id.into()),
        })
    }
}
impl Drop for DemandGuard {
    fn drop(&mut self) {
        let mut demands = self
            .priority
            .demands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(demand) = demands.get_mut(&self.connection) {
            if let Some(id) = &self.learning_id {
                if demand
                    .learning
                    .as_ref()
                    .is_some_and(|(current, _)| current == id)
                {
                    demand.learning = None;
                }
            } else {
                demand.foreground = demand.foreground.saturating_sub(1);
                demand.last_foreground = self.priority.clock.now();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Manual(Mutex<DateTime<Utc>>);
    impl Clock for Manual {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
    }
    #[test]
    fn foreground_preempts_and_five_second_quiet_period_is_connection_scoped() {
        let clock = Arc::new(Manual(Mutex::new(Utc::now())));
        let priority = ConnectionPriority::new(clock.clone());
        assert!(!priority.ready("a"));
        *clock.0.lock().unwrap() += chrono::Duration::seconds(5);
        let stop = CancellationToken::new();
        let learning = priority.learning("a", "job", stop.clone()).unwrap();
        let main = priority.foreground("a");
        assert!(stop.is_cancelled());
        assert!(!priority.ready("a"));
        assert!(priority.ready("b"));
        drop(learning);
        *clock.0.lock().unwrap() += chrono::Duration::seconds(60);
        assert!(!priority.ready("a"));
        drop(main);
        *clock.0.lock().unwrap() += chrono::Duration::milliseconds(4999);
        assert!(!priority.ready("a"));
        *clock.0.lock().unwrap() += chrono::Duration::milliseconds(1);
        assert!(priority.ready("a"));
    }
}
