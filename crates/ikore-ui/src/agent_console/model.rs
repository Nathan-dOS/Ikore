//! The state of each console surface (ADR-040, M2): the list it scrolls,
//! whether it follows the newest block, what is open and selected. A run's
//! projection is shared by every surface that shows the run.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{App, FocusHandle, ListAlignment, ListOffset, ListState, ScrollHandle, px};
use ikore_core::terminals::{TerminalEvent, ToolKind};

use super::projection::{BlockBody, ConsoleBlock, ConsoleFilter, Projector};

/// Pixels the list renders beyond its viewport.
const OVERDRAW: f32 = 512.0;
/// How long `Copied` stays after a copy.
pub(crate) const COPIED_FOR: Duration = Duration::from_millis(1_500);
/// Blocks a live projection keeps before it starts over from the buffer.
const MAX_PROJECTED_BLOCKS: usize = 1_000;
/// Group keys start with this, so a group never shares its first action's key.
const GROUP_PREFIX: &str = "group:";

/// How a finished run shows its actions (PD4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub(crate) enum RunDisplay {
    /// Consecutive actions collapse into one row per stretch.
    #[default]
    Grouped,
    /// Commands stay visible; the other actions collapse.
    CommandsFirst,
    /// Every action on its own row.
    Every,
}

impl RunDisplay {
    pub(crate) const ALL: [Self; 3] = [Self::Grouped, Self::CommandsFirst, Self::Every];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Grouped => "Grouped",
            Self::CommandsFirst => "Commands first",
            Self::Every => "Every action",
        }
    }

    /// The value a choice list carries.
    pub(crate) const fn value(self) -> &'static str {
        match self {
            Self::Grouped => "grouped",
            Self::CommandsFirst => "commands",
            Self::Every => "every",
        }
    }
}

/// Consecutive actions shown as one row once the run ended (PD4).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ActionGroup {
    /// `group:` and the first action's key: stable while the run's blocks are.
    pub(crate) key: String,
    pub(crate) actions: Vec<Rc<ConsoleBlock>>,
    pub(crate) failed: usize,
}

impl ActionGroup {
    /// `5 actions · 3 read · 2 searched`.
    pub(crate) fn label(&self) -> String {
        let mut families: Vec<(&'static str, usize)> = Vec::new();
        for action in &self.actions {
            if let BlockBody::Action(action) = &action.body {
                let family = family(action.kind);
                match families.iter_mut().find(|(name, _)| *name == family) {
                    Some((_, count)) => *count += 1,
                    None => families.push((family, 1)),
                }
            }
        }
        let mut label = format!("{} actions", self.actions.len());
        for (family, count) in families {
            label.push_str(&format!(" · {count} {family}"));
        }
        if self.failed > 0 {
            label.push_str(&format!(" · {} failed", self.failed));
        }
        label
    }
}

fn family(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Read | ToolKind::List => "read",
        ToolKind::Search => "searched",
        ToolKind::Write | ToolKind::Edit => "changed",
        ToolKind::Shell => "commands",
        ToolKind::Web => "web",
        ToolKind::Task => "tasks",
        ToolKind::Plan => "plan",
        ToolKind::Mcp | ToolKind::Other => "tools",
    }
}

/// One row of a console list.
#[derive(Debug)]
pub(crate) enum ConsoleRow {
    Block(Rc<ConsoleBlock>),
    Group(Rc<ActionGroup>),
}

impl ConsoleRow {
    pub(crate) fn key(&self) -> &str {
        match self {
            Self::Block(block) => &block.key,
            Self::Group(group) => &group.key,
        }
    }

    /// The text search and copy read.
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Block(block) => block.text(),
            Self::Group(group) => {
                let mut text = group.label();
                for action in &group.actions {
                    text.push('\n');
                    text.push_str(&action.text());
                }
                text
            }
        }
    }

    /// The same allocation, or the same content after a projection started
    /// over: either way the list keeps the row's measured height.
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Block(left), Self::Block(right)) => Rc::ptr_eq(left, right) || left == right,
            (Self::Group(left), Self::Group(right)) => Rc::ptr_eq(left, right) || left == right,
            _ => false,
        }
    }
}

/// What one surface renders, in order.
#[derive(Debug, Default)]
pub(crate) struct ConsoleRows {
    pub(crate) rows: Vec<ConsoleRow>,
    /// Actions in the run, and how many failed: the run header's counts.
    pub(crate) actions: usize,
    pub(crate) failed: usize,
}

/// The view of one console the renderer needs, read in one borrow.
#[derive(Debug)]
pub(crate) struct ConsoleView {
    pub(crate) list: ListState,
    pub(crate) rows: Rc<ConsoleRows>,
    pub(crate) following: bool,
    pub(crate) unseen: usize,
    pub(crate) expanded: Rc<HashSet<String>>,
    pub(crate) selected: Option<usize>,
    pub(crate) copied: Option<String>,
    pub(crate) display: RunDisplay,
}

#[derive(Debug)]
pub(crate) struct SurfaceState {
    pub(crate) list: ListState,
    run_id: Option<String>,
    filter: ConsoleFilter,
    grouped: Option<RunDisplay>,
    /// Rows that arrived while the developer read earlier ones.
    pub(crate) unseen: usize,
    /// Block and group keys shown in full.
    pub(crate) expanded: HashSet<String>,
    /// The row the keyboard is on.
    pub(crate) selected: Option<usize>,
    pub(crate) copied: Option<(String, Instant)>,
    pub(crate) display: RunDisplay,
    /// Where the console's keys go; made on the first render.
    focus: Option<FocusHandle>,
    synced_revision: u64,
    rows: Rc<ConsoleRows>,
}

impl Default for SurfaceState {
    fn default() -> Self {
        Self {
            list: ListState::new(0, ListAlignment::Bottom, px(OVERDRAW)),
            run_id: None,
            filter: ConsoleFilter::All,
            grouped: None,
            unseen: 0,
            expanded: HashSet::new(),
            selected: None,
            copied: None,
            display: RunDisplay::Grouped,
            focus: None,
            synced_revision: 0,
            rows: Rc::default(),
        }
    }
}

impl SurfaceState {
    pub(crate) fn rows(&self) -> Rc<ConsoleRows> {
        Rc::clone(&self.rows)
    }

    /// The row whose `Copied` chip still shows.
    pub(crate) fn copied_row(&self) -> Option<String> {
        self.copied
            .as_ref()
            .filter(|(_, at)| at.elapsed() < COPIED_FOR)
            .map(|(key, _)| key.clone())
    }

    /// Whether the newest row is in view. A list aligned at the bottom goes
    /// back to following on its own when the developer scrolls to the end.
    pub(crate) fn following(&self) -> bool {
        self.list.logical_scroll_top().item_ix >= self.list.item_count()
    }

    /// Resumes following: the newest row comes into view.
    pub(crate) fn follow(&mut self) {
        self.unseen = 0;
        let count = self.list.item_count();
        self.list.scroll_to(ListOffset {
            item_ix: count,
            offset_in_item: px(0.),
        });
    }

    /// Opens or closes a block or group, and re-measures what changed.
    pub(crate) fn toggle_expanded(&mut self, key: &str) {
        if !self.expanded.insert(key.to_owned()) {
            self.expanded.remove(key);
        }
        if key.starts_with(GROUP_PREFIX) {
            // A group's actions join or leave the list: lay it out again.
            self.synced_revision = 0;
        } else if let Some(position) = self.position(key) {
            self.list.splice(position..position + 1, 1);
        }
    }

    pub(crate) fn position(&self, key: &str) -> Option<usize> {
        self.rows.rows.iter().position(|row| row.key() == key)
    }

    /// Moves the keyboard selection by `step` rows and brings it into view.
    pub(crate) fn select_step(&mut self, step: isize) {
        let count = self.rows.rows.len();
        if count == 0 {
            self.selected = None;
            return;
        }
        let next = match self.selected {
            None if step < 0 => count - 1,
            None => 0,
            Some(current) => current.saturating_add_signed(step).min(count - 1),
        };
        self.selected = Some(next);
        self.reveal(next);
    }

    pub(crate) fn reveal(&mut self, position: usize) {
        self.list.scroll_to_reveal_item(position);
    }

    pub(crate) fn set_display(&mut self, display: RunDisplay) {
        self.display = display;
        self.synced_revision = 0;
    }
}

/// A console of plain lines (the API viewer): newest at the bottom, and
/// following like a run's console.
#[derive(Debug, Default)]
struct LinesState {
    scroll: ScrollHandle,
    /// The line count and the last line's length at the last render.
    mark: Option<(usize, usize)>,
    unseen: usize,
}

/// What the renderer needs of a console of lines.
#[derive(Debug)]
pub(crate) struct LinesView {
    pub(crate) scroll: ScrollHandle,
    pub(crate) following: bool,
    pub(crate) unseen: usize,
}

fn at_bottom(scroll: &ScrollHandle) -> bool {
    let max = scroll.max_offset().height;
    max <= px(1.) || scroll.offset().y <= -max + px(2.)
}

/// Every console's state, and the projections of the runs they show.
#[derive(Debug, Default)]
pub(crate) struct Consoles {
    projections: RefCell<HashMap<String, Projector>>,
    surfaces: RefCell<HashMap<String, SurfaceState>>,
    lines: RefCell<HashMap<String, LinesState>>,
}

impl Consoles {
    /// A run's buffer changed beyond an append (a merged snapshot, a re-read
    /// range, another run's history): its projection starts over.
    pub(crate) fn invalidate(&mut self, run_id: &str) {
        self.drop_projection(run_id);
    }

    /// One live event appended to a run's buffer.
    pub(crate) fn append(&mut self, run_id: &str, event: &TerminalEvent) {
        let Some(projector) = self.projections.get_mut().get_mut(run_id) else {
            return;
        };
        if projector.blocks().len() >= MAX_PROJECTED_BLOCKS {
            // Rebuilt from the buffer's 500 events on the next render.
            self.drop_projection(run_id);
        } else {
            projector.extend(std::iter::once(event));
        }
    }

    /// Drops the projections of runs no buffer holds any more.
    pub(crate) fn retain_runs(&mut self, keep: impl Fn(&str) -> bool) {
        let gone = self
            .projections
            .get_mut()
            .keys()
            .filter(|run_id| !keep(run_id))
            .cloned()
            .collect::<Vec<_>>();
        for run_id in gone {
            self.drop_projection(&run_id);
        }
    }

    /// A new projection numbers its revisions from the start again, so every
    /// surface showing the run syncs with it in full.
    fn drop_projection(&mut self, run_id: &str) {
        self.projections.get_mut().remove(run_id);
        for surface in self.surfaces.get_mut().values_mut() {
            if surface.run_id.as_deref() == Some(run_id) {
                surface.synced_revision = 0;
            }
        }
    }

    pub(crate) fn surface_mut(&mut self, key: &str) -> &mut SurfaceState {
        self.surfaces.get_mut().entry(key.to_owned()).or_default()
    }

    /// How `key`'s console shows a finished run's actions.
    pub(crate) fn display(&self, key: &str) -> RunDisplay {
        self.surfaces
            .borrow()
            .get(key)
            .map(|surface| surface.display)
            .unwrap_or_default()
    }

    /// The handle that takes `key`'s console keys.
    pub(crate) fn focus(&self, key: &str, cx: &App) -> FocusHandle {
        self.surfaces
            .borrow_mut()
            .entry(key.to_owned())
            .or_default()
            .focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone()
    }

    /// Brings `key`'s list in step with `run_id`'s events, and returns what it
    /// renders. While a run goes on, only the rows from the first one that
    /// changed are spliced; a finished run may group its actions (PD4).
    pub(crate) fn view(
        &self,
        key: &str,
        run_id: Option<&str>,
        run_finished: bool,
        events: &[TerminalEvent],
        filter: ConsoleFilter,
    ) -> ConsoleView {
        let mut surfaces = self.surfaces.borrow_mut();
        let surface = surfaces.entry(key.to_owned()).or_default();
        match run_id {
            None => {
                if surface.run_id.is_some() {
                    let count = surface.list.item_count();
                    surface.list.splice(0..count, 0);
                    surface.rows = Rc::default();
                    surface.run_id = None;
                    surface.synced_revision = 0;
                }
            }
            Some(run_id) => self.sync(surface, run_id, run_finished, events, filter),
        }
        if surface.following() {
            // Scrolled back to the end: nothing waits unseen.
            surface.unseen = 0;
        }
        ConsoleView {
            list: surface.list.clone(),
            rows: surface.rows(),
            following: surface.following(),
            unseen: surface.unseen,
            expanded: Rc::new(surface.expanded.clone()),
            selected: surface.selected,
            copied: surface.copied_row(),
            display: surface.display,
        }
    }

    fn sync(
        &self,
        surface: &mut SurfaceState,
        run_id: &str,
        run_finished: bool,
        events: &[TerminalEvent],
        filter: ConsoleFilter,
    ) {
        let mut projections = self.projections.borrow_mut();
        let projector = projections.entry(run_id.to_owned()).or_insert_with(|| {
            let mut projector = Projector::default();
            projector.rebuild(events);
            projector
        });
        // Events that reached the buffer without the live append.
        if events
            .last()
            .is_some_and(|event| event.sequence > projector.last_sequence())
        {
            projector.extend(events);
        }
        let new_run = surface.run_id.as_deref() != Some(run_id);
        // A finished run groups its actions; one the developer is reading
        // higher up waits until they come back to the end.
        let grouped = (run_finished
            && surface.display != RunDisplay::Every
            && (new_run || surface.grouped.is_some() || surface.following()))
        .then_some(surface.display);
        if new_run || surface.filter != filter || surface.grouped != grouped {
            let count = surface.list.item_count();
            surface.list.splice(0..count, 0);
            surface.rows = Rc::default();
            surface.synced_revision = 0;
            surface.selected = None;
            if new_run {
                surface.unseen = 0;
                surface.expanded.clear();
            }
            surface.run_id = Some(run_id.to_owned());
            surface.filter = filter;
            surface.grouped = grouped;
        }
        if surface.synced_revision == projector.revision() && surface.synced_revision != 0 {
            return;
        }
        let blocks = projector
            .blocks()
            .iter()
            .filter(|block| filter.accepts(&block.body))
            .cloned()
            .collect::<Vec<_>>();
        let (actions, failed) = projector
            .blocks()
            .iter()
            .filter_map(|block| match &block.body {
                BlockBody::Action(action) => Some(action.failed()),
                _ => None,
            })
            .fold((0, 0), |(actions, failed), is_failed| {
                (actions + 1, failed + usize::from(is_failed))
            });
        let rows = match grouped {
            Some(display) => group_rows(blocks, display, &surface.expanded),
            None => blocks.into_iter().map(ConsoleRow::Block).collect(),
        };
        let old = &surface.rows.rows;
        let from = old
            .iter()
            .zip(&rows)
            .position(|(before, after)| !before.same(after))
            .unwrap_or_else(|| old.len().min(rows.len()));
        let added = rows.len().saturating_sub(old.len());
        // Read before the splice: where the developer was looking.
        let following = surface.following();
        surface.list.splice(from..old.len(), rows.len() - from);
        surface.unseen = if following {
            0
        } else {
            surface.unseen.saturating_add(added)
        };
        surface.rows = Rc::new(ConsoleRows {
            rows,
            actions,
            failed,
        });
        surface.synced_revision = projector.revision().max(1);
    }

    /// Brings a console of `len` lines in step: it stays at the end while it
    /// follows, and counts what arrives while the developer reads above.
    pub(crate) fn lines_view(&self, key: &str, len: usize, last_len: usize) -> LinesView {
        let mut lines = self.lines.borrow_mut();
        let state = lines.entry(key.to_owned()).or_default();
        let following = at_bottom(&state.scroll);
        let mark = (len, last_len);
        if state.mark != Some(mark) {
            if following {
                state.scroll.scroll_to_bottom();
            } else if let Some((old_len, _)) = state.mark {
                state.unseen = state
                    .unseen
                    .saturating_add(len.saturating_sub(old_len).max(1));
            }
            state.mark = Some(mark);
        }
        if following {
            state.unseen = 0;
        }
        LinesView {
            scroll: state.scroll.clone(),
            following,
            unseen: state.unseen,
        }
    }

    /// Resumes following a console of lines.
    pub(crate) fn follow_lines(&mut self, key: &str) {
        if let Some(state) = self.lines.get_mut().get_mut(key) {
            state.scroll.scroll_to_bottom();
            state.unseen = 0;
        }
    }
}

/// Collapses each stretch of consecutive actions into one group row (PD4);
/// an open group keeps its actions on their own rows after it. Under
/// `CommandsFirst`, commands break the stretch and stay visible.
fn group_rows(
    blocks: Vec<Rc<ConsoleBlock>>,
    display: RunDisplay,
    expanded: &HashSet<String>,
) -> Vec<ConsoleRow> {
    let mut rows = Vec::new();
    let mut stretch: Vec<Rc<ConsoleBlock>> = Vec::new();
    let flush = |stretch: &mut Vec<Rc<ConsoleBlock>>, rows: &mut Vec<ConsoleRow>| match stretch
        .len()
    {
        0 => {}
        1 => rows.push(ConsoleRow::Block(stretch.remove(0))),
        _ => {
            let actions = std::mem::take(stretch);
            let failed = actions
                .iter()
                .filter(|block| matches!(&block.body, BlockBody::Action(action) if action.failed()))
                .count();
            let group = Rc::new(ActionGroup {
                key: format!("{GROUP_PREFIX}{}", actions[0].key),
                actions,
                failed,
            });
            let open = expanded.contains(&group.key);
            rows.push(ConsoleRow::Group(Rc::clone(&group)));
            if open {
                rows.extend(group.actions.iter().cloned().map(ConsoleRow::Block));
            }
        }
    };
    for block in blocks {
        let groupable = match &block.body {
            BlockBody::Action(action) => {
                !(display == RunDisplay::CommandsFirst && action.kind == ToolKind::Shell)
            }
            _ => false,
        };
        if groupable {
            stretch.push(block);
        } else {
            flush(&mut stretch, &mut rows);
            rows.push(ConsoleRow::Block(block));
        }
    }
    flush(&mut stretch, &mut rows);
    rows
}

/// Positions of the rows whose text holds `query`, ignoring case.
pub(crate) fn search_matches(rows: &[ConsoleRow], query: Option<&str>) -> Vec<usize> {
    let Some(query) = query.map(str::trim).filter(|query| !query.is_empty()) else {
        return Vec::new();
    };
    let query = query.to_lowercase();
    rows.iter()
        .enumerate()
        .filter(|(_, row)| row.text().to_lowercase().contains(&query))
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ikore_core::terminals::{ActionStatus, EventDetailV1, TerminalEventKind};

    fn event(sequence: u64, kind: TerminalEventKind, content: &str) -> TerminalEvent {
        TerminalEvent {
            id: format!("event-{sequence}"),
            run_id: "run-1".into(),
            sequence,
            kind,
            stream: None,
            content: content.into(),
            redaction_count: 0,
            redaction_categories: Vec::new(),
            created_at: "2026-11-15T00:00:00Z".into(),
            detail: None,
            observed_at: None,
        }
    }

    fn action(sequence: u64, ordinal: u32, kind: ToolKind, name: &str) -> [TerminalEvent; 2] {
        [
            TerminalEvent {
                detail: Some(EventDetailV1::ActionCall {
                    v: 1,
                    ordinal,
                    kind,
                    name: name.into(),
                }),
                ..event(sequence, TerminalEventKind::ToolUse, &format!("{name} · x"))
            },
            TerminalEvent {
                detail: Some(EventDetailV1::ActionResult {
                    v: 1,
                    ordinal,
                    status: ActionStatus::Ok,
                    exit_code: None,
                    lines: None,
                    bytes: None,
                    duration_ms: None,
                }),
                ..event(
                    sequence + 1,
                    TerminalEventKind::ToolResult,
                    &format!("{name} · ok"),
                )
            },
        ]
    }

    fn run_events() -> Vec<TerminalEvent> {
        let mut events = vec![event(1, TerminalEventKind::System, "Run starting.")];
        events.extend(action(2, 1, ToolKind::Read, "Read"));
        events.extend(action(4, 2, ToolKind::Search, "Grep"));
        events.extend(action(6, 3, ToolKind::Shell, "Bash"));
        events.push(event(8, TerminalEventKind::Message, "Done."));
        events
    }

    #[test]
    fn a_finished_run_groups_its_actions_and_commands_first_keeps_commands() {
        let consoles = Consoles::default();
        let events = run_events();
        let running = consoles.view("agent", Some("run-1"), false, &events, ConsoleFilter::All);
        assert_eq!(running.rows.rows.len(), 5);
        assert_eq!(running.rows.actions, 3);

        let finished = consoles.view("agent", Some("run-1"), true, &events, ConsoleFilter::All);
        let ConsoleRow::Group(group) = &finished.rows.rows[1] else {
            panic!("{:?}", finished.rows.rows);
        };
        assert_eq!(
            group.label(),
            "3 actions · 1 read · 1 searched · 1 commands"
        );
        assert!(group.key.starts_with(GROUP_PREFIX));
        assert_eq!(finished.rows.rows.len(), 3);

        let mut consoles = consoles;
        consoles
            .surface_mut("agent")
            .set_display(RunDisplay::CommandsFirst);
        let commands = consoles.view("agent", Some("run-1"), true, &events, ConsoleFilter::All);
        assert!(
            matches!(&commands.rows.rows[1], ConsoleRow::Group(group) if group.actions.len() == 2)
        );
        assert!(matches!(&commands.rows.rows[2], ConsoleRow::Block(block)
            if matches!(&block.body, BlockBody::Action(action) if action.kind == ToolKind::Shell)));
    }

    #[test]
    fn opening_a_group_shows_its_actions_after_it() {
        let mut consoles = Consoles::default();
        let events = run_events();
        let view = consoles.view("agent", Some("run-1"), true, &events, ConsoleFilter::All);
        let key = view.rows.rows[1].key().to_owned();
        consoles.surface_mut("agent").toggle_expanded(&key);
        let open = consoles.view("agent", Some("run-1"), true, &events, ConsoleFilter::All);
        assert_eq!(open.rows.rows.len(), 6);
        assert!(open.expanded.contains(&key));
        // The first action's own key is not the group's: it stays closed.
        assert!(!open.expanded.contains(open.rows.rows[2].key()));
    }

    #[test]
    fn a_projection_that_starts_over_keeps_equal_rows_and_shows_new_ones() {
        let mut consoles = Consoles::default();
        let mut events = run_events();
        for event in &mut events {
            event.sequence += 10;
        }
        let before = consoles.view("agent", Some("run-1"), false, &events, ConsoleFilter::All);
        let count = before.list.item_count();
        consoles.invalidate("run-1");
        let after = consoles.view("agent", Some("run-1"), false, &events, ConsoleFilter::All);
        assert_eq!(after.list.item_count(), count);
        assert!(
            before
                .rows
                .rows
                .iter()
                .zip(&after.rows.rows)
                .all(|(left, right)| left.same(right))
        );
        // A re-read range brings an event before the last one: the new
        // projection may count the same revisions, and still reaches the list.
        events.insert(0, event(5, TerminalEventKind::Warning, "late"));
        consoles.invalidate("run-1");
        let refreshed = consoles.view("agent", Some("run-1"), false, &events, ConsoleFilter::All);
        assert_eq!(refreshed.rows.rows.len(), count + 1);
        assert_eq!(refreshed.list.item_count(), count + 1);
    }

    #[test]
    fn appended_events_count_as_unseen_only_when_paused() {
        let mut consoles = Consoles::default();
        let mut events = run_events();
        consoles.view("agent", Some("run-1"), false, &events, ConsoleFilter::All);
        events.push(event(9, TerminalEventKind::Warning, "slow"));
        let view = consoles.view("agent", Some("run-1"), false, &events, ConsoleFilter::All);
        assert!(view.following);
        assert_eq!(view.unseen, 0);

        // Reading the first row pauses the console.
        consoles.surface_mut("agent").list.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
        events.push(event(10, TerminalEventKind::Error, "failed"));
        events.push(event(11, TerminalEventKind::Message, "Retrying."));
        let paused = consoles.view("agent", Some("run-1"), false, &events, ConsoleFilter::All);
        assert!(!paused.following);
        assert_eq!(paused.unseen, 2);

        consoles.surface_mut("agent").follow();
        let resumed = consoles.view("agent", Some("run-1"), false, &events, ConsoleFilter::All);
        assert!(resumed.following);
        assert_eq!(resumed.unseen, 0);
    }
}
