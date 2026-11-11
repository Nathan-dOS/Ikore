//! The shell's icon vocabulary. One variant per vector glyph the UI uses; `path()` is the asset
//! key served by `assets::HvcAssets`. Rendered through `widgets::icon` / `widgets::icon_button`.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Icon {
    /// Home view.
    House,
    /// New work (capture overlay).
    Plus,
    /// Forge view.
    Hammer,
    /// Pulsations view; pipeline card.
    Activity,
    /// Vault view.
    Vault,
    /// Agents view.
    Bot,
    /// Logs view.
    ScrollText,
    /// Chat view; "Answer Feature Agent" step.
    MessageSquare,
    /// Workspace view; "Open project" step.
    FolderOpen,
    /// Settings view.
    Settings,
    /// Notifications.
    Bell,
    /// System health readout in the header.
    HeartPulse,
    /// Collapse the context rail.
    PanelRightClose,
    /// Expand the context rail.
    PanelRightOpen,
    /// Run / compose / follow the pipeline.
    Play,
    /// Approve; completed pipeline stage.
    Check,
    /// Reject; close.
    X,
    /// Request revision; refresh.
    RotateCw,
    /// Indexing paused.
    Pause,
    /// HITL reviews and stage failures waiting on the developer.
    TriangleAlert,
    /// New feature.
    Sparkles,
    /// Backlog / add tasks.
    ListTodo,
    /// Plan waves.
    Waves,
    /// Review now.
    Eye,
    /// Open artifacts.
    Layers,
    /// CLI backends.
    Cpu,
    /// Embeddings / indexing.
    Database,
    /// Token usage.
    Zap,
    /// Footer clock.
    Clock,
    /// Embedding model pull.
    Download,
    /// Review brief: a decision the agent took.
    Signpost,
    /// Review brief: left out of scope.
    Ban,
    /// Review brief: acceptance criteria and their proof.
    Target,
    /// Review brief: what the host already verified.
    ShieldCheck,
    /// Review brief: the work-unit plan.
    Network,
    /// Review brief: a criterion proven by reading only.
    CircleHelp,
    /// Select triggers; expanded disclosure groups.
    ChevronDown,
    /// Collapsed disclosure groups.
    ChevronRight,
    /// A file row in the Workspace and Vault trees.
    FileText,
    /// A directory row in the Workspace and Vault trees.
    Folder,
    /// Save a form or document.
    Save,
    /// Send a message or prompt.
    Send,
    /// Stop a running process.
    Square,
    /// Delete.
    Trash,
    /// Copy to the clipboard.
    Copy,
    /// Zoom out; review brief items removed since the previous revision.
    Minus,
    /// Fit the graph to its panel.
    Maximize,
    /// Search settings.
    Search,
    /// Keyboard shortcuts page.
    Keyboard,
    /// About page.
    Info,
    /// Labs (experimental settings).
    FlaskConical,
    /// LLM provider connections.
    Plug,
    /// LLM routes.
    Route,
    /// Second Brain navigation.
    Brain,
    /// Token usage.
    ChartColumn,
    /// General settings.
    SlidersHorizontal,
    /// Stage composition.
    Workflow,
    /// Back to a list from its detail.
    ChevronLeft,
    /// Expand a docked panel (terminal) upwards.
    ChevronUp,
    /// Durations (runs, pulsations).
    Timer,
    /// Review brief: an item flagged for revision.
    Flag,
    /// Review brief: what changed since the previous revision.
    GitCompare,
    /// Review brief: moved since the previous revision.
    ArrowRight,
    /// Review brief: changed since the previous revision.
    Pencil,
    /// Notes view (ADR-029).
    NotebookPen,
    /// Voice dictation settings (ADR-029).
    Mic,
    /// Canvas selection pointer.
    Cursor,
    /// Canvas pan tool.
    Hand,
    /// Canvas decision shape.
    Diamond,
    /// Canvas ellipse shape.
    Ellipse,
    /// Canvas straight line tool.
    Line,
    /// Canvas plain text tool.
    Type,
    /// Canvas erase tool.
    Eraser,
    /// Keep a drawing tool active.
    Lock,
    /// Return to selection after drawing.
    LockOpen,
}

impl Icon {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 75] = [
        Self::House,
        Self::Plus,
        Self::Hammer,
        Self::Activity,
        Self::Vault,
        Self::Bot,
        Self::ScrollText,
        Self::MessageSquare,
        Self::FolderOpen,
        Self::Settings,
        Self::Bell,
        Self::HeartPulse,
        Self::PanelRightClose,
        Self::PanelRightOpen,
        Self::Play,
        Self::Check,
        Self::X,
        Self::RotateCw,
        Self::Pause,
        Self::TriangleAlert,
        Self::Sparkles,
        Self::ListTodo,
        Self::Waves,
        Self::Eye,
        Self::Layers,
        Self::Cpu,
        Self::Database,
        Self::Zap,
        Self::Clock,
        Self::Download,
        Self::Signpost,
        Self::Plug,
        Self::Ban,
        Self::Target,
        Self::ShieldCheck,
        Self::Network,
        Self::CircleHelp,
        Self::ChevronDown,
        Self::ChevronRight,
        Self::FileText,
        Self::Folder,
        Self::Save,
        Self::Send,
        Self::Square,
        Self::Trash,
        Self::Copy,
        Self::Minus,
        Self::Maximize,
        Self::Search,
        Self::Keyboard,
        Self::Info,
        Self::FlaskConical,
        Self::Route,
        Self::Brain,
        Self::ChartColumn,
        Self::SlidersHorizontal,
        Self::Workflow,
        Self::ChevronLeft,
        Self::ChevronUp,
        Self::Timer,
        Self::Flag,
        Self::GitCompare,
        Self::ArrowRight,
        Self::Pencil,
        Self::NotebookPen,
        Self::Mic,
        Self::Cursor,
        Self::Hand,
        Self::Diamond,
        Self::Ellipse,
        Self::Line,
        Self::Type,
        Self::Eraser,
        Self::Lock,
        Self::LockOpen,
    ];

    /// Asset path served by `HvcAssets`; `icons/<lucide-name>.svg`.
    pub(crate) const fn path(self) -> &'static str {
        match self {
            Self::House => "icons/house.svg",
            Self::Plus => "icons/plus.svg",
            Self::Hammer => "icons/hammer.svg",
            Self::Activity => "icons/activity.svg",
            Self::Vault => "icons/vault.svg",
            Self::Bot => "icons/bot.svg",
            Self::ScrollText => "icons/scroll-text.svg",
            Self::MessageSquare => "icons/message-square.svg",
            Self::FolderOpen => "icons/folder-open.svg",
            Self::Settings => "icons/settings.svg",
            Self::Bell => "icons/bell.svg",
            Self::HeartPulse => "icons/heart-pulse.svg",
            Self::PanelRightClose => "icons/panel-right-close.svg",
            Self::PanelRightOpen => "icons/panel-right-open.svg",
            Self::Play => "icons/play.svg",
            Self::Check => "icons/check.svg",
            Self::X => "icons/x.svg",
            Self::RotateCw => "icons/rotate-cw.svg",
            Self::Pause => "icons/pause.svg",
            Self::TriangleAlert => "icons/triangle-alert.svg",
            Self::Sparkles => "icons/sparkles.svg",
            Self::ListTodo => "icons/list-todo.svg",
            Self::Waves => "icons/waves.svg",
            Self::Eye => "icons/eye.svg",
            Self::Layers => "icons/layers.svg",
            Self::Cpu => "icons/cpu.svg",
            Self::Database => "icons/database.svg",
            Self::Zap => "icons/zap.svg",
            Self::Clock => "icons/clock.svg",
            Self::Download => "icons/download.svg",
            Self::Signpost => "icons/signpost.svg",
            Self::Plug => "icons/plug.svg",
            Self::Ban => "icons/ban.svg",
            Self::Target => "icons/target.svg",
            Self::ShieldCheck => "icons/shield-check.svg",
            Self::Network => "icons/network.svg",
            Self::CircleHelp => "icons/circle-help.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::FileText => "icons/file-text.svg",
            Self::Folder => "icons/folder.svg",
            Self::Timer => "icons/timer.svg",
            Self::ChevronUp => "icons/chevron-up.svg",
            Self::ChevronLeft => "icons/chevron-left.svg",
            Self::Workflow => "icons/workflow.svg",
            Self::SlidersHorizontal => "icons/sliders-horizontal.svg",
            Self::ChartColumn => "icons/chart-column.svg",
            Self::Brain => "icons/brain.svg",
            Self::Route => "icons/route.svg",
            Self::FlaskConical => "icons/flask-conical.svg",
            Self::Info => "icons/info.svg",
            Self::Keyboard => "icons/keyboard.svg",
            Self::Search => "icons/search.svg",
            Self::Maximize => "icons/maximize.svg",
            Self::Minus => "icons/minus.svg",
            Self::Copy => "icons/copy.svg",
            Self::Trash => "icons/trash-2.svg",
            Self::Square => "icons/square.svg",
            Self::Send => "icons/send.svg",
            Self::Save => "icons/save.svg",
            Self::Flag => "icons/flag.svg",
            Self::GitCompare => "icons/git-compare.svg",
            Self::ArrowRight => "icons/arrow-right.svg",
            Self::Pencil => "icons/pencil.svg",
            Self::NotebookPen => "icons/notebook-pen.svg",
            Self::Mic => "icons/mic.svg",
            Self::Cursor => "icons/canvas-cursor.svg",
            Self::Hand => "icons/canvas-hand.svg",
            Self::Diamond => "icons/canvas-diamond.svg",
            Self::Ellipse => "icons/canvas-ellipse.svg",
            Self::Line => "icons/canvas-line.svg",
            Self::Type => "icons/canvas-type.svg",
            Self::Eraser => "icons/canvas-eraser.svg",
            Self::Lock => "icons/canvas-lock.svg",
            Self::LockOpen => "icons/canvas-lock-open.svg",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_paths_are_unique_and_under_the_icons_folder() {
        let mut paths: Vec<&str> = Icon::ALL.iter().map(|icon| icon.path()).collect();
        assert_eq!(paths.len(), Icon::ALL.len());
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(
            paths.len(),
            Icon::ALL.len(),
            "two Icon variants share a path"
        );
        for path in paths {
            assert!(
                path.starts_with("icons/") && path.ends_with(".svg"),
                "{path}"
            );
        }
    }
}
