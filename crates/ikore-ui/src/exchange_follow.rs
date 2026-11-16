//! The UI side of the exchange log (ADR-037 M2): follows the open
//! project's doorbells, reads back what a gap or a lagged broadcast lost,
//! and takes a full snapshot only when the log says the cursor is too old.
//! A terminal sequence gap is repaired by a ranged re-read of the run.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use gpui::Context;
use ikore_core::exchange::log::{
    DoorbellAction, ExchangeDoorbellV1, ExchangeFollower, PAGE_LIMIT, Page, PageAction, StreamKind,
};

use crate::AppModel;
use crate::AppView;
use crate::agents::TerminalGap;

/// The doorbell counts as healthy this long after a reset.
pub(crate) const DOORBELL_RESET_GRACE: Duration = Duration::from_secs(60);
/// Pulsation polling while the doorbell is healthy (D7).
pub(crate) const HEALTHY_PULSATION_POLL: Duration = Duration::from_secs(5);
/// Pages a terminal gap repair reads before falling back to a reload.
const TERMINAL_REREAD_PAGES: usize = 20;
/// A burst of pipeline doorbells refreshes pulsations once, this much later.
const DOORBELL_REFRESH_DELAY: Duration = Duration::from_millis(250);

impl AppModel {
    fn open_project_id(&self, cx: &Context<Self>) -> Option<String> {
        self.workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string)
    }

    /// Doorbells drive refreshes while healthy: a follower exists and no
    /// reset happened in the last minute.
    pub(crate) fn exchange_healthy(&self) -> bool {
        self.exchange.is_some()
            && self
                .exchange_reset_at
                .is_none_or(|at| at.elapsed() >= DOORBELL_RESET_GRACE)
    }

    /// Whether the polling loop should refresh pulsations on this tick.
    pub(crate) fn pulsation_poll_due(&mut self) -> bool {
        if self.exchange_healthy()
            && self
                .last_pulsation_poll
                .is_some_and(|at| at.elapsed() < HEALTHY_PULSATION_POLL)
        {
            return false;
        }
        self.last_pulsation_poll = Some(Instant::now());
        true
    }

    pub(crate) fn handle_exchange_doorbell(
        &mut self,
        doorbell: ExchangeDoorbellV1,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.open_project_id(cx) else {
            return;
        };
        if doorbell.project_id != project {
            return;
        }
        // The project's snapshot covers what came before the first doorbell
        // this model sees, so the follower starts just behind it.
        if self
            .exchange
            .as_ref()
            .is_none_or(|follower| follower.project_id() != project)
        {
            self.exchange = Some(ExchangeFollower::new(project, doorbell.previous_global_seq));
        }
        let action = self
            .exchange
            .as_mut()
            .map_or(DoorbellAction::Ignore, |follower| {
                follower.on_doorbell(&doorbell)
            });
        match action {
            DoorbellAction::Apply => self.apply_exchange_streams(
                [(doorbell.stream_kind, doorbell.subject_id)],
                false,
                cx,
            ),
            DoorbellAction::CatchUp(after) => self.read_exchange_after(after, cx),
            DoorbellAction::Ignore => {}
        }
    }

    /// The broadcast dropped events. With a follower, read its log from the
    /// cursor (ephemeral streams repair themselves: workspace changes mark
    /// the vault stale, terminal and LLM gaps re-read their own sequences);
    /// without one, take the full snapshot as before.
    pub(crate) fn handle_event_lag(&mut self, cx: &mut Context<Self>) {
        let Some(follower) = self.exchange.as_mut() else {
            self.request_event_resync(cx);
            return;
        };
        let after = follower.on_lagged();
        self.artifact_read.invalidate();
        self.backend_read.invalidate();
        self.cleanup_read.invalidate();
        self.mark_vault_stale(cx);
        self.status = "Event history delayed; reading the exchange log".into();
        if let Some(after) = after {
            self.read_exchange_after(after, cx);
        }
    }

    fn read_exchange_after(&mut self, after: i64, cx: &mut Context<Self>) {
        let (Some(core), Some(project)) = (self.core.clone(), self.open_project_id(cx)) else {
            if let Some(follower) = self.exchange.as_mut() {
                follower.on_read_failed();
            }
            return;
        };
        let runtime = core.runtime_handle().clone();
        self.exchange_read_task = Some(cx.spawn(async move |this, cx| {
            let page = runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .exchange_read_after(&project, after, PAGE_LIMIT)
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|page| page);
            let _ = this.update(cx, |model, cx| match page {
                Ok(page) => model.apply_exchange_page(page, cx),
                Err(error) => {
                    if let Some(follower) = model.exchange.as_mut() {
                        follower.on_read_failed();
                    }
                    model.status = format!("Exchange log read failed: {error}").into();
                    model.request_event_resync(cx);
                }
            });
        }));
    }

    /// Dispatches a page read back from the log; a reset takes the full
    /// snapshot and drops the follower, which the next doorbell recreates.
    pub(crate) fn apply_exchange_page(&mut self, page: Page, cx: &mut Context<Self>) {
        let Some(follower) = self.exchange.as_mut() else {
            return;
        };
        match follower.on_page(&page) {
            PageAction::Snapshot => {
                self.exchange = None;
                self.exchange_reset_at = Some(Instant::now());
                self.request_event_resync(cx);
            }
            action => {
                let streams = page
                    .items
                    .into_iter()
                    .map(|item| (item.stream_kind, item.subject_id))
                    .collect::<BTreeSet<_>>();
                self.apply_exchange_streams(streams, true, cx);
                if let PageAction::Continue(after) = action {
                    self.read_exchange_after(after, cx);
                }
            }
        }
    }

    /// Refreshes what changed. In order, a doorbell only refreshes what has
    /// no broadcast of its own (pulsations and stage executions); read back
    /// after a loss, every stream refreshes its view.
    fn apply_exchange_streams(
        &mut self,
        streams: impl IntoIterator<Item = (StreamKind, String)>,
        recovered: bool,
        cx: &mut Context<Self>,
    ) {
        let project = self.open_project_id(cx);
        let mut seen = BTreeSet::new();
        for (kind, subject) in streams {
            let refresher = match kind {
                StreamKind::Pulsation | StreamKind::StageExecution => "pulsations",
                _ if !recovered => continue,
                StreamKind::TerminalRun => "agents",
                StreamKind::ChatConversation => {
                    if let Some(project) = project.clone() {
                        self.handle_canvas_authoring_changed(project, subject.clone(), cx);
                    }
                    "chat"
                }
                StreamKind::Forge => "forge",
                StreamKind::Notes => "notes",
                StreamKind::Drawings => "drawings",
                StreamKind::Docs => "docs",
            };
            if !seen.insert(refresher) {
                continue;
            }
            match refresher {
                "pulsations" => self.schedule_doorbell_refresh(cx),
                "agents" => {
                    self.backend_read.invalidate();
                    if self.active_view == AppView::Agents {
                        self.refresh_agents(cx);
                    } else {
                        self.refresh_agents_quiet(cx);
                    }
                }
                "chat" => {
                    let conversation = self
                        .chat
                        .read(cx)
                        .active_conversation
                        .as_ref()
                        .map(ToString::to_string);
                    if let Some(project) = project.clone() {
                        self.hydrate_chat_project(project, conversation, cx);
                    }
                }
                "forge" => self.refresh_forge(cx),
                "notes" => self.refresh_notes(cx),
                "drawings" => {
                    if !self.canvas_has_pending() {
                        self.refresh_canvas(cx);
                    }
                }
                _ => self.refresh_doc_drift(cx),
            }
        }
        cx.notify();
    }

    /// One pulsation refresh for a burst of pipeline doorbells: unit
    /// attempts alone write several items a second while a stage runs.
    fn schedule_doorbell_refresh(&mut self, cx: &mut Context<Self>) {
        if self.doorbell_refresh_task.is_some() {
            return;
        }
        let timer = cx.background_executor().timer(DOORBELL_REFRESH_DELAY);
        self.doorbell_refresh_task = Some(cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |model, cx| {
                model.doorbell_refresh_task = None;
                model.refresh_pulsations(cx);
            });
        }));
    }

    /// Re-reads the events a run skipped, page by page, and merges them; a
    /// failed or unfinished read falls back to reloading the panel.
    pub(crate) fn repair_terminal_gap(&mut self, gap: TerminalGap, cx: &mut Context<Self>) {
        let Some(core) = self.core.clone() else {
            self.reload_agents_after_gap(cx);
            return;
        };
        if self.terminal_gap_tasks.contains_key(&gap.run_id) {
            // A repair of this run is in flight; the panel's next event
            // shows whether a gap remains.
            return;
        }
        let runtime = core.runtime_handle().clone();
        let run_id = gap.run_id.clone();
        let key = gap.run_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let read = runtime
                .spawn_blocking(move || {
                    let terminals = core.terminals();
                    let mut events = Vec::new();
                    let mut after = gap.after;
                    for _ in 0..TERMINAL_REREAD_PAGES {
                        let page = terminals
                            .load_events(&gap.run_id, after, 500)
                            .map_err(|error| error.to_string())?;
                        let Some(last) = page.last().map(|event| event.sequence) else {
                            break;
                        };
                        after = last;
                        events.extend(page.into_iter().filter(|event| event.sequence < gap.before));
                        if after + 1 >= gap.before {
                            return Ok(events);
                        }
                    }
                    Err("the missing range did not come back".to_owned())
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|events| events);
            let _ = this.update(cx, |model, cx| {
                model.terminal_gap_tasks.remove(&run_id);
                match read {
                    Ok(events) => {
                        model
                            .agents
                            .update(cx, |agents, _| agents.merge_events(&run_id, events));
                        model.status = "Terminal gap filled from the run's history".into();
                        cx.notify();
                    }
                    Err(_) => model.reload_agents_after_gap(cx),
                }
            });
        });
        self.terminal_gap_tasks.insert(key, task);
    }

    fn reload_agents_after_gap(&mut self, cx: &mut Context<Self>) {
        self.needs_resync = true;
        self.agents_resync_pending = true;
        self.status = "Terminal gap detected; reloading persisted history".into();
        if self.active_view == AppView::Agents {
            self.refresh_agents(cx);
        } else {
            self.refresh_agents_quiet(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};
    use ikore_core::exchange::log::{LogItem, StreamKind};

    use super::*;

    fn bell(global_seq: i64, previous: i64) -> ExchangeDoorbellV1 {
        ExchangeDoorbellV1 {
            project_id: "project-1".into(),
            stream_id: "stream".into(),
            stream_kind: StreamKind::Pulsation,
            subject_id: "pulse".into(),
            kind: "pulsation_transition".into(),
            seq: 1,
            global_seq,
            previous_global_seq: previous,
        }
    }

    fn item(global_seq: i64, kind: StreamKind) -> LogItem {
        LogItem {
            global_seq,
            project_id: "project-1".into(),
            stream_id: "stream".into(),
            stream_kind: kind,
            subject_id: "subject".into(),
            seq: 1,
            kind: "forge_changed".into(),
            trust: ikore_core::exchange::log::TrustClass::Host,
            producer_ref: None,
            content_sha256: None,
            payload_table: None,
            payload_id: None,
            created_at: "now".into(),
        }
    }

    fn open_project(model: &mut AppModel, cx: &mut Context<AppModel>) {
        model.workspace.update(cx, |workspace, _| {
            workspace.project_id = Some("project-1".into());
        });
    }

    #[gpui::test]
    fn doorbells_in_order_move_the_cursor_without_a_snapshot(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            open_project(model, cx);
            // Another project's doorbell is not this model's business.
            model.handle_exchange_doorbell(
                ExchangeDoorbellV1 {
                    project_id: "project-2".into(),
                    ..bell(1, 0)
                },
                cx,
            );
            assert!(model.exchange.is_none());
            model.handle_exchange_doorbell(bell(7, 4), cx);
            model.handle_exchange_doorbell(bell(9, 7), cx);
            let follower = model.exchange.as_ref().unwrap();
            assert_eq!((follower.cursor(), follower.reading()), (9, false));
            assert!(!model.needs_resync());
            assert!(model.exchange_healthy());
        });
    }

    #[gpui::test]
    fn a_gap_reads_the_log_and_a_page_moves_the_cursor(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            open_project(model, cx);
            model.handle_exchange_doorbell(bell(7, 4), cx);
            // 8 was missed. A model without a core cannot read, so the
            // follower drops the read and the next doorbell retries it.
            model.handle_exchange_doorbell(bell(10, 8), cx);
            assert!(!model.exchange.as_ref().unwrap().reading());
            assert_eq!(model.exchange.as_ref().unwrap().cursor(), 7);

            // The read a core would run, delivered.
            let mut follower = model.exchange.take().unwrap();
            assert_eq!(follower.on_lagged(), Some(7));
            model.exchange = Some(follower);
            model.apply_exchange_page(
                Page {
                    items: vec![item(8, StreamKind::Forge), item(10, StreamKind::Pulsation)],
                    next_cursor: 10,
                    more: false,
                    reset: false,
                },
                cx,
            );
            let follower = model.exchange.as_ref().unwrap();
            assert_eq!((follower.cursor(), follower.reading()), (10, false));
            assert!(!model.needs_resync(), "a read back is not a snapshot");
        });
    }

    #[gpui::test]
    fn a_lagged_broadcast_reads_the_log_unless_there_is_no_cursor(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            open_project(model, cx);
            model.handle_event_lag(cx);
            assert!(model.needs_resync(), "without a cursor, the old snapshot");

            let model_with_cursor = &mut *model;
            model_with_cursor.event_resync_pending = false;
            model_with_cursor.needs_resync = false;
            model_with_cursor.handle_exchange_doorbell(bell(3, 0), cx);
            model_with_cursor.handle_event_lag(cx);
            assert!(!model_with_cursor.needs_resync(), "with a cursor, a read");
        });
    }

    #[gpui::test]
    fn a_reset_takes_the_snapshot_and_slows_trust_in_the_doorbell(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            open_project(model, cx);
            model.handle_exchange_doorbell(bell(3, 0), cx);
            assert!(model.pulsation_poll_due());
            assert!(!model.pulsation_poll_due(), "healthy: every five seconds");
            model.apply_exchange_page(
                Page {
                    reset: true,
                    ..Page::default()
                },
                cx,
            );
            assert!(model.needs_resync());
            assert!(model.exchange.is_none() && model.exchange_reset_at.is_some());
            assert!(!model.exchange_healthy());
            assert!(model.pulsation_poll_due() && model.pulsation_poll_due());
            // The next doorbell starts a new cursor.
            model.handle_exchange_doorbell(bell(5, 4), cx);
            assert_eq!(model.exchange.as_ref().unwrap().cursor(), 5);
        });
    }

    #[gpui::test]
    fn a_terminal_gap_without_a_history_reader_falls_back_to_a_reload(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            model.repair_terminal_gap(
                TerminalGap {
                    run_id: "run-1".into(),
                    after: 1,
                    before: 4,
                },
                cx,
            );
            assert!(model.needs_resync() && model.agents_resync_pending);
            assert!(model.terminal_gap_tasks.is_empty());
        });
    }
}
