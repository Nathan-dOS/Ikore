//! Settings search: one static index of every setting the modal shows, shared with the page
//! renderers so the text a search matches is the text on screen.
//!
//! Matching follows a simple, predictable rule instead of fuzzy scoring: the query and the
//! searchable text are normalized (lowercase, punctuation to spaces); a query of three or more
//! characters matches anywhere, a shorter one only at the start of a word ("ro" finds Routes but
//! "ou" finds nothing).

use std::sync::OnceLock;

use crate::settings::SettingsPage;

/// Where an entry lives inside its page: the list, or the detail of the selected item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryScope {
    Page,
    ConnectionDetail,
    RouteDetail,
}

#[derive(Debug)]
pub(crate) struct SettingsEntry {
    /// Row id: the row renders as `settings-row:{id}` and search results as
    /// `settings-result:{id}`.
    pub(crate) id: &'static str,
    pub(crate) page: SettingsPage,
    /// Title of the section the row sits in; also the scroll anchor of a deep link.
    pub(crate) section: &'static str,
    pub(crate) title: &'static str,
    pub(crate) description: &'static str,
    pub(crate) keywords: &'static str,
    pub(crate) scope: EntryScope,
}

const fn entry(
    id: &'static str,
    page: SettingsPage,
    section: &'static str,
    title: &'static str,
    description: &'static str,
    keywords: &'static str,
) -> SettingsEntry {
    SettingsEntry {
        id,
        page,
        section,
        title,
        description,
        keywords,
        scope: EntryScope::Page,
    }
}

const fn scoped(mut entry: SettingsEntry, scope: EntryScope) -> SettingsEntry {
    entry.scope = scope;
    entry
}

use EntryScope::{ConnectionDetail, RouteDetail};
use SettingsPage as P;

pub(crate) static ENTRIES: &[SettingsEntry] = &[
    // General
    entry(
        "auto-open-review",
        P::General,
        "Workflow",
        "Open reviews automatically",
        "Jump to a pulsation's Review as soon as a checkpoint appears, unless you are writing to the Companion or an overlay is open.",
        "checkpoint hitl review auto open",
    ),
    entry(
        "context-rail",
        P::General,
        "Layout",
        "Show the context rail",
        "The right-hand column with the pipeline, what is waiting on you and runtime health.",
        "sidebar right panel rail ctrl-b",
    ),
    entry(
        "reduced-motion",
        P::General,
        "Accessibility",
        "Reduced motion",
        "Follows the Windows animation effects setting; pulses and fades are skipped when Windows turns animations off.",
        "animation motion accessibility fade",
    ),
    // Usage
    entry(
        "usage-dashboard",
        P::Usage,
        "Token usage",
        "Token usage",
        "Generation usage recorded by Hard Vibe Code, kept when conversations are deleted.",
        "tokens cost input output chart model pulsation",
    ),
    entry(
        "project-deletion",
        P::Projects,
        "Local projects",
        "Delete a local project",
        "Permanently removes the repository directory, its Git files, pulsations, Companion conversations and project data. Token usage remains attributed in separate history.",
        "delete repository folder disk permanent project companion pulsations usage",
    ),
    // Keyboard shortcuts
    entry(
        "shortcuts-navigation",
        P::Shortcuts,
        "Navigation",
        "Navigation",
        "Views, Settings, sections, the suggested next step and the context rail.",
        "ctrl home forge pulsations vault agents logs companion workspace settings section rail",
    ),
    entry(
        "shortcuts-review",
        P::Shortcuts,
        "Review & failure",
        "Review & failure",
        "Approve, revise or reject a checkpoint, and retry, revise or abort a failed stage.",
        "approve reject revise retry abort enter hitl",
    ),
    entry(
        "shortcuts-editing",
        P::Shortcuts,
        "Editor & text",
        "Editor & text",
        "Undo, redo, selection and clipboard in the editor and text fields.",
        "undo redo copy paste cut select",
    ),
    entry(
        "shortcuts-overlays",
        P::Shortcuts,
        "Overlays & menus",
        "Overlays & menus",
        "Closing overlays, choosing in menus and searching Settings.",
        "escape esc menu dialog close",
    ),
    // Connections
    entry(
        "cli-connections",
        P::Connections,
        "CLI connections",
        "CLI connections and plan usage",
        "Installed CLIs, login status and provider-reported subscription limits, including 5-hour and weekly windows when available.",
        "cli codex claude gemini opencode antigravity login plan quota usage 5h weekly subscription reset",
    ),
    entry(
        "connections-list",
        P::Connections,
        "Providers",
        "Provider connections",
        "API endpoints, default models and keys kept in the Windows credential store.",
        "provider api key openai anthropic ollama gemini openrouter credential keyring",
    ),
    scoped(
        entry(
            "connection-provider",
            P::Connections,
            "Connection",
            "Provider",
            "The API protocol and profile this connection uses.",
            "profile protocol",
        ),
        ConnectionDetail,
    ),
    scoped(
        entry(
            "connection-name",
            P::Connections,
            "Connection",
            "Name",
            "How the connection is listed in routes and pickers.",
            "display name label",
        ),
        ConnectionDetail,
    ),
    scoped(
        entry(
            "connection-model",
            P::Connections,
            "Connection",
            "Default model",
            "Used when a route target does not name a model.",
            "model id",
        ),
        ConnectionDetail,
    ),
    scoped(
        entry(
            "connection-endpoint",
            P::Connections,
            "Connection",
            "Endpoint",
            "Base URL, for profiles that allow a custom one.",
            "url base host",
        ),
        ConnectionDetail,
    ),
    scoped(
        entry(
            "connection-api-key",
            P::Connections,
            "Connection",
            "API key",
            "Stored in the Windows credential store and never shown again. Leave empty to keep the saved key.",
            "secret token keyring credential",
        ),
        ConnectionDetail,
    ),
    scoped(
        entry(
            "connection-test",
            P::Connections,
            "Connection test",
            "Round trip to the default model",
            "Sends a short request with the default model and reports what happened and how long it took.",
            "test ping latency health probe reachable",
        ),
        ConnectionDetail,
    ),
    scoped(
        entry(
            "connection-api-catalog",
            P::Connections,
            "API model catalog",
            "Models from the API",
            "Refresh the provider's model list, then pick one to fill the default model. Routes and conversations are unchanged.",
            "models list refresh reasoning catalog",
        ),
        ConnectionDetail,
    ),
    scoped(
        entry(
            "connection-delete",
            P::Connections,
            "Danger zone",
            "Delete connection",
            "Removes the connection and its stored key.",
            "remove delete",
        ),
        ConnectionDetail,
    ),
    // Routes
    entry(
        "routing-mode",
        P::Routes,
        "Routing engine",
        "Routing v2",
        "Off keeps the legacy route, Shadow evaluates the new router without using it, On routes every call through it.",
        "router shadow experimental preview",
    ),
    entry(
        "routes-list",
        P::Routes,
        "Routes",
        "Routes",
        "Ordered fallbacks across connections, with retry and time limits.",
        "fallback failover priority targets",
    ),
    entry(
        "resilience",
        P::Routes,
        "Resilience",
        "Clear cooldowns",
        "Providers that failed recently are skipped for a while; clearing makes them eligible again.",
        "cooldown circuit breaker rate limit reset resilience",
    ),
    scoped(
        entry(
            "route-name",
            P::Routes,
            "Route",
            "Name",
            "How the route is listed in bindings and pickers.",
            "display name label",
        ),
        RouteDetail,
    ),
    scoped(
        entry(
            "route-strategy",
            P::Routes,
            "Route",
            "Strategy",
            "priority tries targets in order, lkgp starts with the last one that worked, score ranks them by recent health.",
            "priority lkgp score order",
        ),
        RouteDetail,
    ),
    scoped(
        entry(
            "route-limits",
            P::Routes,
            "Route",
            "Limits",
            "Attempts per call and the time allowed per attempt and per route.",
            "attempts timeout retries",
        ),
        RouteDetail,
    ),
    scoped(
        entry(
            "route-target-connection",
            P::Routes,
            "Targets",
            "Target connection",
            "The connection Save adds to the route, or updates when it is already a target.",
            "target connection provider",
        ),
        RouteDetail,
    ),
    scoped(
        entry(
            "route-target-model",
            P::Routes,
            "Targets",
            "Target model",
            "Model for that target; empty uses the connection's default model.",
            "model id",
        ),
        RouteDetail,
    ),
    scoped(
        entry(
            "route-target-order",
            P::Routes,
            "Targets",
            "Order",
            "Move the target connection earlier or later in the fallback order.",
            "reorder move up down",
        ),
        RouteDetail,
    ),
    scoped(
        entry(
            "route-binding",
            P::Routes,
            "Exact binding",
            "Bind to an agent and stage",
            "Makes this route the one an agent uses at a pipeline stage in the open project.",
            "binding agent stage",
        ),
        RouteDetail,
    ),
    scoped(
        entry(
            "route-delete",
            P::Routes,
            "Danger zone",
            "Delete route",
            "Bindings that use it fall back to the default routing.",
            "remove delete",
        ),
        RouteDetail,
    ),
    // Embeddings & RAG
    entry(
        "embeddings-enabled",
        P::Embeddings,
        "Retrieval",
        "Embeddings",
        "Turns your knowledge notes into vectors so agents pull in the most relevant ones instead of the whole vault.",
        "rag vector semantic enable",
    ),
    entry(
        "context-strategy",
        P::Embeddings,
        "Retrieval",
        "Context strategy",
        "Graph follows wikilinks only; Graph + RAG adds similarity search on top.",
        "rag graph yaml context preparation",
    ),
    entry(
        "embedding-top-k",
        P::Embeddings,
        "Retrieval",
        "Notes per query",
        "How many matching notes go into an agent's context. More finds more but costs tokens and can dilute focus.",
        "top k topk",
    ),
    entry(
        "embedding-model",
        P::Embeddings,
        "Model",
        "Embedding model",
        "The local Ollama model that computes the vectors.",
        "ollama model nomic",
    ),
    entry(
        "voice-model",
        P::Voice,
        "Speech model",
        "Speech model",
        "The local model that turns your voice into note text. The system chooses it; it runs on your computer and nothing is sent anywhere.",
        "voice speech recognition transcription nemotron nvidia stt dictation download",
    ),
    entry(
        "voice-setup",
        P::Voice,
        "Speech model",
        "Download",
        "Installs the speech runtime and downloads the model once. Both are checked against fixed checksums before use.",
        "install download setup runtime nemo speech",
    ),
    entry(
        "voice-push-to-talk",
        P::Voice,
        "Push-to-talk",
        "Push-to-talk",
        "Hold the shortcut anywhere in Windows, speak and release. HVC saves the note to the project open in HVC, without opening a window.",
        "push to talk hold record microphone dictate",
    ),
    entry(
        "voice-shortcut",
        P::Voice,
        "Push-to-talk",
        "Shortcut",
        "Two or three modifier keys plus F1 to F10. If another application already uses it, choose another.",
        "hotkey keyboard ctrl alt shift function key",
    ),
    entry(
        "reindex",
        P::Embeddings,
        "Model",
        "Reindex workspace",
        "Recomputes vectors for notes that changed. Runs in the background and never blocks a pulsation.",
        "index sync rebuild",
    ),
    entry(
        "embedding-checklist",
        P::Embeddings,
        "Setup checklist",
        "Readiness",
        "Ollama reachable, model downloaded, a test call succeeds; the first failing step says what to fix.",
        "ollama download pull install readiness",
    ),
    // Second Brain
    entry(
        "adaptive-mode",
        P::SecondBrain,
        "Adaptive navigation",
        "Activation",
        "Shadow evaluates the policy without changing prompts; On enables dynamic expansion and exact code resolution.",
        "experimental preview shadow",
    ),
    entry(
        "adaptive-initial-k",
        P::SecondBrain,
        "Note expansion",
        "Initial depth",
        "Wikilink hops explored first.",
        "k hops",
    ),
    entry(
        "adaptive-max-k",
        P::SecondBrain,
        "Note expansion",
        "Maximum depth",
        "Hops the navigator may reach when too few notes qualify.",
        "k hops",
    ),
    entry(
        "adaptive-similarity",
        P::SecondBrain,
        "Note expansion",
        "Minimum similarity",
        "Notes below this similarity do not count as relevant.",
        "threshold score",
    ),
    entry(
        "adaptive-min-candidates",
        P::SecondBrain,
        "Note expansion",
        "Minimum relevant notes",
        "Keep expanding while fewer notes than this qualify.",
        "candidates",
    ),
    entry(
        "adaptive-max-notes",
        P::SecondBrain,
        "Note expansion",
        "Note budget",
        "Most notes added to an agent's context.",
        "budget limit",
    ),
    entry(
        "adaptive-beam",
        P::SecondBrain,
        "Note expansion",
        "Beam width",
        "Candidates kept per hop while exploring.",
        "beam",
    ),
    entry(
        "adaptive-files",
        P::SecondBrain,
        "Code resolution",
        "Source files",
        "Most source files resolved from note references.",
        "files code",
    ),
    entry(
        "adaptive-code",
        P::SecondBrain,
        "Code resolution",
        "Code characters",
        "Most characters of resolved code added to the context.",
        "chars size",
    ),
    entry(
        "adaptive-timeout",
        P::SecondBrain,
        "Code resolution",
        "Time budget",
        "Navigation stops after this long and keeps what it found.",
        "timeout ms",
    ),
    // Preparation & context
    entry(
        "preparation-enabled",
        P::Preparation,
        "Task refinement",
        "TaskRefine / PreSpec",
        "Scores how vague a pulsation's description is and asks clarifying questions before Specification.",
        "vagueness clarification questions prespec",
    ),
    entry(
        "preparation-threshold",
        P::Preparation,
        "Task refinement",
        "Vagueness threshold",
        "From 0 to 100. Applies to the next evaluation; turning refinement off never confirms a pending review.",
        "threshold score",
    ),
    entry(
        "context-budget",
        P::Preparation,
        "Context budget",
        "Handoff budget",
        "Characters passed between stages, from 2,000 to 80,000. Raise it when mandatory artifacts do not fit.",
        "context limit characters chars",
    ),
    entry(
        "learning-enabled",
        P::Preparation,
        "Advisory learning",
        "Learn from each run",
        "Runs after a pulsation is Done or Failed and audits what happened.",
        "learning report",
    ),
    entry(
        "learning-refinement",
        P::Preparation,
        "Advisory learning",
        "Graph refinement",
        "Observe audits without proposals; Propose suggests changes, and every write still needs your approval.",
        "observe propose",
    ),
    entry(
        "brownfield-budget",
        P::Preparation,
        "Brownfield enrichment",
        "LLM calls per pulsation",
        "0 makes no calls. Each batch publishes at most two proposals.",
        "brownfield budget calls",
    ),
    entry(
        "brownfield-radius",
        P::Preparation,
        "Brownfield enrichment",
        "Expansion radius",
        "k=1 uses direct neighbours; k=2 allows a second hop.",
        "brownfield radius hops",
    ),
    // Stage composition
    entry(
        "secret-sanitization",
        P::Composition,
        "Secret protection",
        "Secret protection and fixture authorizations",
        "Always-on context sanitization, project counters and local authorizations for exact synthetic fixture occurrences. Detected values are never displayed.",
        "secret credential redaction sanitization synthetic fixture exception security",
    ),
    entry(
        "composition-enabled",
        P::Composition,
        "Composed batches",
        "Compose Forge batches",
        "Forge batches run as host-coordinated units with a shared budget, exact edits and host checks. Free-form pulsations are not affected.",
        "v4 units batches",
    ),
    entry(
        "composition-validation",
        P::Composition,
        "Host validation",
        "Validate on the host",
        "Runs compile and test checks on a disposable copy of the project. No sandbox, downloads or forwarded credentials.",
        "compile tests checks cargo",
    ),
    entry(
        "composition-visualizer",
        P::Composition,
        "Review brief",
        "Visualizer notes",
        "An agent adds a headline and up to three notes to each checkpoint's Brief, citing only what the host delivered. Off by default; the Brief is complete without it.",
        "visualizer brief review notes agent",
    ),
    entry(
        "composition-revision-rounds",
        P::Composition,
        "Revision rounds",
        "Rounds before a decision",
        "After this many Request revision rounds in a pulsation, each further revision needs an explicit decision: continue with a reason or replan from the Specification. Technical retries are not counted.",
        "threshold revision rounds convergence replan",
    ),
    entry(
        "composition-checks",
        P::Composition,
        "Host validation",
        "Checks",
        "Commands the host runs on each admission; required ones must pass.",
        "profiles presets cargo",
    ),
    entry(
        "composition-overrides",
        P::Composition,
        "Specialty overrides",
        "Overrides",
        "A worker specialty inherits its stage's model and transport unless overridden here. The host never escalates on its own.",
        "override model transport thinking backend",
    ),
    // Agent timeouts
    entry(
        "agent-timeouts",
        P::Timeouts,
        "Execution deadlines",
        "Per-agent deadlines",
        "Deadlines extend while the CLI streams output; an inactivity window stops hung processes and a ceiling stops runaway runs.",
        "timeout deadline inactivity ceiling streaming",
    ),
    // Labs
    entry(
        "labs-max-in-flight",
        P::Labs,
        "Stage composition experiments",
        "Units in flight",
        "2 lets independent units of a stage run together. The result is the same; only wall time changes.",
        "parallel concurrency units",
    ),
    entry(
        "labs-recipe-code",
        P::Labs,
        "Stage composition experiments",
        "Code stage plans several units",
        "Turn on only after the qualification report records the recipe. Off keeps one worker per stage.",
        "recipe qualification code",
    ),
    entry(
        "labs-recipe-test",
        P::Labs,
        "Stage composition experiments",
        "Test stage plans several units",
        "Turn on only after the qualification report records the recipe. Off keeps one worker per stage.",
        "recipe qualification test",
    ),
    entry(
        "labs-cli-observation",
        P::Labs,
        "CLI observation trial",
        "Unbounded CLI observation",
        "The next 20 automatic Pulsation and Learning CLI runs have no wall-clock timeout; cancel, shutdown and output limits still apply.",
        "trial observation timeout experimental",
    ),
    // About
    entry(
        "about-version",
        P::About,
        "Application",
        "Version",
        "The build you are running.",
        "version build release debug",
    ),
    entry(
        "about-data-dir",
        P::About,
        "Application",
        "Data directory",
        "Projects, sessions, logs and settings live here.",
        "folder path appdata",
    ),
    entry(
        "about-database",
        P::About,
        "Application",
        "Database",
        "The local SQLite file.",
        "sqlite db file",
    ),
    entry(
        "about-runtime",
        P::About,
        "Runtime",
        "Runtime health",
        "CLI agents, LLM providers and embeddings, as the context rail shows them.",
        "health cli llm embeddings",
    ),
    entry(
        "about-diagnostics",
        P::About,
        "Diagnostics",
        "Copy diagnostics",
        "Version, build, operating system, data directory and runtime counts. No keys or paths inside projects.",
        "support bug report clipboard",
    ),
];

/// The entry with this id. Every renderer names ids from `ENTRIES`; an unknown one is a
/// programming error caught by the page tests.
pub(crate) fn entry_by_id(id: &str) -> &'static SettingsEntry {
    ENTRIES
        .iter()
        .find(|entry| entry.id == id)
        .unwrap_or_else(|| panic!("settings entry `{id}` is not in the index"))
}

/// One page in the search results, with the entries on it that matched.
#[derive(Debug, Clone)]
pub(crate) struct SearchHit {
    pub(crate) page: SettingsPage,
    pub(crate) entries: Vec<&'static SettingsEntry>,
}

/// Results shown per page; a longer list says "N more" instead of growing the sidebar.
pub(crate) const MAX_ENTRIES_PER_PAGE: usize = 6;

pub(crate) fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = true;
    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            out.push(ch);
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// `query` is already normalized and non-empty.
pub(crate) fn matches(query: &str, haystack: &str) -> bool {
    if query.chars().count() >= 3 {
        haystack.contains(query)
    } else {
        haystack.split(' ').any(|word| word.starts_with(query))
    }
}

fn entry_haystacks() -> &'static [String] {
    static HAYSTACKS: OnceLock<Vec<String>> = OnceLock::new();
    HAYSTACKS.get_or_init(|| {
        ENTRIES
            .iter()
            .map(|entry| {
                normalize(&format!(
                    "{} {} {} {} {}",
                    entry.title, entry.description, entry.section, entry.keywords, entry.id
                ))
            })
            .collect()
    })
}

fn page_haystacks() -> &'static [String] {
    static HAYSTACKS: OnceLock<Vec<String>> = OnceLock::new();
    HAYSTACKS.get_or_init(|| {
        SettingsPage::ALL
            .iter()
            .map(|page| {
                normalize(&format!(
                    "{} {} {}",
                    page.label(),
                    page.description(),
                    page.keywords()
                ))
            })
            .collect()
    })
}

/// `None` when the query is blank (not searching); otherwise the matching pages in sidebar
/// order, possibly empty.
pub(crate) fn search(raw: &str) -> Option<Vec<SearchHit>> {
    let query = normalize(raw);
    if query.is_empty() {
        return None;
    }
    let entries = entry_haystacks();
    let pages = page_haystacks();
    Some(
        SettingsPage::ALL
            .iter()
            .zip(pages)
            .filter_map(|(page, page_haystack)| {
                let hits: Vec<&'static SettingsEntry> = ENTRIES
                    .iter()
                    .zip(entries)
                    .filter(|(entry, haystack)| entry.page == *page && matches(&query, haystack))
                    .map(|(entry, _)| entry)
                    .collect();
                (!hits.is_empty() || matches(&query, page_haystack)).then_some(SearchHit {
                    page: *page,
                    entries: hits,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages(query: &str) -> Vec<SettingsPage> {
        search(query)
            .unwrap_or_default()
            .into_iter()
            .map(|hit| hit.page)
            .collect()
    }

    #[test]
    fn normalize_lowercases_and_turns_punctuation_into_single_spaces() {
        assert_eq!(normalize("  API-Key / Keyring!! "), "api key keyring");
        assert_eq!(normalize("Embeddings & RAG"), "embeddings rag");
        assert_eq!(normalize("…"), "");
    }

    #[test]
    fn long_queries_match_anywhere_and_short_ones_only_at_word_starts() {
        assert!(matches("oute", "routes and more"));
        assert!(matches("ro", "routes and more"));
        assert!(!matches("ou", "routes and more"));
        assert!(pages("ro").contains(&SettingsPage::Routes));
        assert!(pages("ROUTES").contains(&SettingsPage::Routes));
    }

    #[test]
    fn keywords_and_descriptions_find_the_right_pages() {
        assert!(pages("api key").contains(&SettingsPage::Connections));
        assert!(pages("hotkeys").contains(&SettingsPage::Shortcuts));
        assert!(pages("experimental").contains(&SettingsPage::Labs));
        assert!(pages("ollama").contains(&SettingsPage::Embeddings));
        let hits = search("timeout").unwrap();
        let timeouts = hits
            .iter()
            .find(|hit| hit.page == SettingsPage::Timeouts)
            .expect("timeouts page");
        assert_eq!(timeouts.entries[0].id, "agent-timeouts");
    }

    #[test]
    fn blank_queries_are_not_a_search_and_unknown_words_find_nothing() {
        assert!(search("").is_none());
        assert!(search("   ").is_none());
        assert_eq!(search("zzzz").unwrap().len(), 0);
    }

    #[test]
    fn entry_ids_are_unique_and_every_page_has_an_entry() {
        let mut ids: Vec<&str> = ENTRIES.iter().map(|entry| entry.id).collect();
        ids.sort_unstable();
        let total = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), total, "duplicate settings entry id");
        for page in SettingsPage::ALL {
            assert!(
                ENTRIES.iter().any(|entry| entry.page == page),
                "{page:?} has no entry"
            );
        }
    }
}
