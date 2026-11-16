//! The frozen reference table a worker is allowed to cite (SC-003/SC-004).
//!
//! Handles are deterministic, namespaced by prefix and assigned by the host in
//! a stable order, so the same inputs always produce the same table and the
//! same plan digest. Persistent identifiers (task UUIDs, revisions, hashes)
//! stay on the host side of the table; the model only ever sees handles.
//!
//! | Namespace   | Prefix      | Meaning                                   |
//! |-------------|-------------|-------------------------------------------|
//! | task        | `task_N`    | immutable batch task (manifest order)     |
//! | criterion   | `crit_N_M`  | acceptance criterion M of task N          |
//! | existing    | `src_N`     | real file in the fenced baseline          |
//! | planned     | `plan_N`    | host-admitted absent destination          |
//! | candidate   | `cand_N`    | validated predecessor output              |
//! | unavailable | `unav_N`    | explicit missing/ambiguous/unsupported    |
//! | anchor      | `anchor_N`  | exact byte interval of a delivered source |
//! | check       | configured  | host validation profile (`check_ref`)     |

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use super::{
    AnchorBindingV1, CompositionError, ErrorCode, Handle, Invariant, ReferenceEntryV1, RelPath,
    Sha256Hex, SourceRole, TaskScopeV1, UnavailableReason, Uuid,
};

/// Reserved handle prefixes. A configured check handle may not use them.
pub const RESERVED_PREFIXES: [&str; 7] = [
    "task_", "crit_", "src_", "plan_", "cand_", "unav_", "anchor_",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Namespace {
    Task,
    Criterion,
    Existing,
    Planned,
    Candidate,
    Unavailable,
    Anchor,
    Check,
}

impl Namespace {
    pub fn of(handle: &Handle) -> Option<Self> {
        let text = handle.as_str();
        Some(if text.starts_with("task_") {
            Self::Task
        } else if text.starts_with("crit_") {
            Self::Criterion
        } else if text.starts_with("src_") {
            Self::Existing
        } else if text.starts_with("plan_") {
            Self::Planned
        } else if text.starts_with("cand_") {
            Self::Candidate
        } else if text.starts_with("unav_") {
            Self::Unavailable
        } else if text.starts_with("anchor_") {
            Self::Anchor
        } else {
            return None;
        })
    }
}

/// Host-side identity behind a task handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskBinding {
    pub task_id: String,
    pub revision: u64,
    pub title: String,
    pub criteria: Vec<Handle>,
}

/// Host-side identity behind a criterion handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CriterionBinding {
    pub task: Handle,
    pub criterion_id: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceTable {
    scope: Vec<TaskScopeV1>,
    tasks: BTreeMap<Handle, TaskBinding>,
    criteria: BTreeMap<Handle, CriterionBinding>,
    entries: Vec<ReferenceEntryV1>,
    anchors: Vec<AnchorBindingV1>,
    checks: Vec<Handle>,
}

impl ReferenceTable {
    pub fn builder() -> ReferenceTableBuilder {
        ReferenceTableBuilder::default()
    }

    pub fn scope(&self) -> &[TaskScopeV1] {
        &self.scope
    }

    pub fn entries(&self) -> &[ReferenceEntryV1] {
        &self.entries
    }

    pub fn anchors(&self) -> &[AnchorBindingV1] {
        &self.anchors
    }

    pub fn checks(&self) -> &[Handle] {
        &self.checks
    }

    pub fn task(&self, handle: &Handle) -> Option<&TaskBinding> {
        self.tasks.get(handle)
    }

    pub fn tasks(&self) -> impl Iterator<Item = (&Handle, &TaskBinding)> {
        self.tasks.iter()
    }

    pub fn criterion(&self, handle: &Handle) -> Option<&CriterionBinding> {
        self.criteria.get(handle)
    }

    pub fn entry(&self, handle: &Handle) -> Option<&ReferenceEntryV1> {
        self.entries.iter().find(|entry| entry.handle() == handle)
    }

    pub fn anchor(&self, handle: &Handle) -> Option<&AnchorBindingV1> {
        self.anchors
            .iter()
            .find(|anchor| &anchor.anchor_ref == handle)
    }

    pub fn has_check(&self, handle: &Handle) -> bool {
        self.checks.contains(handle)
    }

    /// The writable destination (existing or planned) registered for a path,
    /// compared with Windows ordinal case-insensitive identity.
    pub fn destination_for_path(&self, path: &RelPath) -> Option<&Handle> {
        let key = path.canonical_key();
        self.entries
            .iter()
            .find(|entry| {
                matches!(
                    entry,
                    ReferenceEntryV1::ExistingSource { .. }
                        | ReferenceEntryV1::PlannedDestination { .. }
                ) && entry
                    .relative_path()
                    .is_some_and(|p| p.canonical_key() == key)
            })
            .map(ReferenceEntryV1::handle)
    }

    /// Whether `criterion` belongs to `task` in the frozen scope (SC-002).
    pub fn criterion_belongs_to(&self, task: &Handle, criterion: &Handle) -> bool {
        self.criteria
            .get(criterion)
            .is_some_and(|binding| &binding.task == task)
    }
}

#[derive(Debug, Default)]
pub struct ReferenceTableBuilder {
    scope: Vec<TaskScopeV1>,
    tasks: BTreeMap<Handle, TaskBinding>,
    criteria: BTreeMap<Handle, CriterionBinding>,
    entries: Vec<ReferenceEntryV1>,
    anchors: Vec<AnchorBindingV1>,
    checks: Vec<Handle>,
    paths: HashMap<String, Handle>,
    counters: HashMap<&'static str, usize>,
}

fn identity(message: impl Into<String>) -> CompositionError {
    CompositionError::new(Invariant::Sc003, ErrorCode::ReferenceConflict, message)
}

impl ReferenceTableBuilder {
    fn next(&mut self, prefix: &'static str) -> Handle {
        let counter = self.counters.entry(prefix).or_insert(0);
        *counter += 1;
        Handle::new(format!("{prefix}{counter}")).expect("generated handles are valid")
    }

    /// Entries registered so far, in registration order.
    pub fn entries(&self) -> &[ReferenceEntryV1] {
        &self.entries
    }

    pub fn scope(&self) -> &[TaskScopeV1] {
        &self.scope
    }

    pub fn entry(&self, handle: &Handle) -> Option<&ReferenceEntryV1> {
        self.entries.iter().find(|entry| entry.handle() == handle)
    }

    /// The existing/planned handle registered for `path` (Windows identity).
    pub fn handle_for_path(&self, path: &RelPath) -> Option<&Handle> {
        self.paths.get(&path.canonical_key())
    }

    /// Registers a batch task with its criteria in manifest order. Tasks with
    /// no criteria are rejected: the contract requires at least one.
    pub fn add_task(
        &mut self,
        task_id: &str,
        revision: u64,
        title: &str,
        criteria: &[(String, String)],
    ) -> Result<Handle, CompositionError> {
        if criteria.is_empty() {
            return Err(CompositionError::new(
                Invariant::Sc002,
                ErrorCode::ScopeInsufficient,
                format!(
                    "Task `{task_id}` has no acceptance criteria; a v4 batch requires at least one."
                ),
            ));
        }
        if self.tasks.values().any(|task| task.task_id == task_id) {
            return Err(identity(format!("Task `{task_id}` registered twice.")));
        }
        let task = self.next("task_");
        let index = self.counters["task_"];
        let mut handles = Vec::with_capacity(criteria.len());
        let mut seen = std::collections::HashSet::new();
        for (position, (criterion_id, description)) in criteria.iter().enumerate() {
            if !seen.insert(criterion_id.as_str()) {
                return Err(identity(format!(
                    "Task `{task_id}` lists criterion `{criterion_id}` twice."
                )));
            }
            let handle = Handle::new(format!("crit_{index}_{}", position + 1))
                .expect("generated handles are valid");
            self.criteria.insert(
                handle.clone(),
                CriterionBinding {
                    task: task.clone(),
                    criterion_id: criterion_id.clone(),
                    description: description.clone(),
                },
            );
            handles.push(handle);
        }
        self.scope.push(TaskScopeV1 {
            task_ref: task.clone(),
            criterion_refs: handles.clone(),
        });
        self.tasks.insert(
            task.clone(),
            TaskBinding {
                task_id: task_id.to_owned(),
                revision,
                title: title.to_owned(),
                criteria: handles,
            },
        );
        Ok(task)
    }

    fn register_path(&mut self, path: &RelPath, handle: &Handle) -> Result<(), CompositionError> {
        let key = path.canonical_key();
        if let Some(existing) = self.paths.get(&key) {
            return Err(identity(format!(
                "Path `{path}` collides with `{existing}` under case-insensitive identity."
            )));
        }
        self.paths.insert(key, handle.clone());
        Ok(())
    }

    /// A real baseline file. Registering the same path twice with the same
    /// hash returns the original handle; a different hash is a conflict.
    pub fn add_existing(
        &mut self,
        path: RelPath,
        content_sha256: Sha256Hex,
        role: SourceRole,
    ) -> Result<Handle, CompositionError> {
        if let Some(handle) = self.paths.get(&path.canonical_key()).cloned() {
            return match self.entries.iter().find(|entry| entry.handle() == &handle) {
                Some(ReferenceEntryV1::ExistingSource {
                    content_sha256: known,
                    relative_path,
                    ..
                }) if known == &content_sha256 && relative_path == &path => Ok(handle),
                _ => Err(identity(format!(
                    "Path `{path}` is already registered with different identity."
                ))),
            };
        }
        let handle = self.next("src_");
        self.register_path(&path, &handle)?;
        self.entries.push(ReferenceEntryV1::ExistingSource {
            handle: handle.clone(),
            relative_path: path,
            content_sha256,
            source_role: role,
        });
        Ok(handle)
    }

    /// A host-admitted absent destination. Collides with existing files and
    /// other planned destinations under case-insensitive identity.
    pub fn add_planned(
        &mut self,
        path: RelPath,
        role: SourceRole,
    ) -> Result<Handle, CompositionError> {
        let handle = self.next("plan_");
        self.register_path(&path, &handle)?;
        self.entries.push(ReferenceEntryV1::PlannedDestination {
            handle: handle.clone(),
            relative_path: path,
            source_role: role,
        });
        Ok(handle)
    }

    /// A validated predecessor output visible to a dependent unit. It may share
    /// a path with an existing source (it is that file's next version) but not
    /// with another candidate.
    pub fn add_candidate(
        &mut self,
        path: RelPath,
        content_sha256: Sha256Hex,
        producer_unit_id: Uuid,
        role: SourceRole,
    ) -> Result<Handle, CompositionError> {
        let key = path.canonical_key();
        if self.entries.iter().any(|entry| {
            matches!(entry, ReferenceEntryV1::CandidateSource { .. })
                && entry
                    .relative_path()
                    .is_some_and(|p| p.canonical_key() == key)
        }) {
            return Err(identity(format!("Candidate `{path}` registered twice.")));
        }
        let handle = self.next("cand_");
        self.entries.push(ReferenceEntryV1::CandidateSource {
            handle: handle.clone(),
            relative_path: path,
            content_sha256,
            producer_unit_id,
            source_role: role,
        });
        Ok(handle)
    }

    pub fn add_unavailable(&mut self, reason: UnavailableReason, hint: &str) -> Handle {
        let handle = self.next("unav_");
        self.entries.push(ReferenceEntryV1::Unavailable {
            handle: handle.clone(),
            reason,
            hint: hint.to_owned(),
        });
        handle
    }

    /// A configured host check. Its handle comes from project configuration
    /// and may not shadow a reserved namespace.
    pub fn add_check(&mut self, check_ref: Handle) -> Result<(), CompositionError> {
        if Namespace::of(&check_ref).is_some() {
            return Err(identity(format!(
                "Check handle `{check_ref}` uses a reserved namespace prefix."
            )));
        }
        if self.checks.contains(&check_ref) {
            return Err(identity(format!(
                "Check handle `{check_ref}` registered twice."
            )));
        }
        self.checks.push(check_ref);
        Ok(())
    }

    /// An exact byte interval of a delivered existing/candidate source. The
    /// caller supplies the exact bytes the handle was issued for; both ends
    /// must be UTF-8 character boundaries.
    pub fn add_anchor(
        &mut self,
        source: &Handle,
        source_bytes: &[u8],
        start_byte: usize,
        end_byte: usize,
    ) -> Result<Handle, CompositionError> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.handle() == source)
            .ok_or_else(|| identity(format!("Anchor source `{source}` is not registered.")))?;
        let expected = entry.content_sha256().ok_or_else(|| {
            identity(format!("Anchor source `{source}` has no content identity."))
        })?;
        let actual = Sha256Hex::of(source_bytes);
        if &actual != expected {
            return Err(identity(format!(
                "Anchor source `{source}` bytes do not match the registered hash."
            )));
        }
        let text = std::str::from_utf8(source_bytes)
            .map_err(|_| identity(format!("Anchor source `{source}` is not UTF-8.")))?;
        if start_byte > end_byte
            || end_byte > source_bytes.len()
            || !text.is_char_boundary(start_byte)
            || !text.is_char_boundary(end_byte)
        {
            return Err(identity(format!(
                "Anchor interval {start_byte}..{end_byte} is not a valid UTF-8 boundary range of `{source}`."
            )));
        }
        if end_byte > u32::MAX as usize {
            return Err(identity("Anchor interval exceeds the contract range."));
        }
        let handle = self.next("anchor_");
        self.anchors.push(AnchorBindingV1 {
            anchor_ref: handle.clone(),
            source_ref: source.clone(),
            source_sha256: actual,
            start_byte: start_byte as u32,
            end_byte: end_byte as u32,
            region_sha256: Sha256Hex::of(&source_bytes[start_byte..end_byte]),
        });
        Ok(handle)
    }

    /// An anchor covering whole lines `start_line..=end_line` (1-based), i.e.
    /// from the start of `start_line` through the newline that ends `end_line`
    /// (or EOF). Line splitting is on `\n` only, so CRLF bytes stay intact.
    pub fn add_line_anchor(
        &mut self,
        source: &Handle,
        source_bytes: &[u8],
        start_line: u32,
        end_line: u32,
    ) -> Result<Handle, CompositionError> {
        let (start, end) = line_span(source_bytes, start_line, end_line).ok_or_else(|| {
            identity(format!(
                "Lines {start_line}..={end_line} are outside `{source}`."
            ))
        })?;
        self.add_anchor(source, source_bytes, start, end)
    }

    pub fn build(self) -> ReferenceTable {
        ReferenceTable {
            scope: self.scope,
            tasks: self.tasks,
            criteria: self.criteria,
            entries: self.entries,
            anchors: self.anchors,
            checks: self.checks,
        }
    }
}

/// Byte span of inclusive 1-based lines; `None` when out of range or inverted.
pub fn line_span(bytes: &[u8], start_line: u32, end_line: u32) -> Option<(usize, usize)> {
    if start_line == 0 || end_line < start_line {
        return None;
    }
    let mut line = 1u32;
    let mut start = None;
    let mut offset = 0usize;
    loop {
        if line == start_line {
            start = Some(offset);
        }
        let line_end = bytes[offset..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|position| offset + position + 1)
            .unwrap_or(bytes.len());
        if line == end_line {
            return start.map(|start| (start, line_end));
        }
        if line_end >= bytes.len() {
            return None;
        }
        offset = line_end;
        line += 1;
    }
}

/// Deterministic note destination under the vault for a stage document.
pub fn vault_note_path(folder: &str, title: &str) -> Result<RelPath, CompositionError> {
    RelPath::new(format!("knowledge/obsidian/{folder}/{}.md", slug(title)))
}

/// ASCII kebab-case slug; non-ASCII letters are dropped rather than
/// transliterated so the destination is deterministic across locales.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let trimmed = out.trim_end_matches('-').to_owned();
    if trimmed.is_empty() {
        "untitled".into()
    } else {
        trimmed
            .chars()
            .take(80)
            .collect::<String>()
            .trim_end_matches('-')
            .to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(value: &str) -> RelPath {
        RelPath::new(value).unwrap()
    }

    fn criteria(ids: &[&str]) -> Vec<(String, String)> {
        ids.iter()
            .map(|id| (id.to_string(), format!("Criterion {id}")))
            .collect()
    }

    #[test]
    fn handles_are_deterministic_and_namespaced() {
        let mut builder = ReferenceTable::builder();
        let task_a = builder
            .add_task("t-a", 1, "A", &criteria(&["AC-1", "AC-2"]))
            .unwrap();
        let task_b = builder
            .add_task("t-b", 3, "B", &criteria(&["AC-1"]))
            .unwrap();
        let src = builder
            .add_existing(path("src/lib.rs"), Sha256Hex::of(b"x"), SourceRole::Source)
            .unwrap();
        let planned = builder
            .add_planned(path("tests/lib_test.rs"), SourceRole::Test)
            .unwrap();
        let unavailable = builder.add_unavailable(UnavailableReason::Missing, "No test file.");
        builder
            .add_check(Handle::new("cargo_check").unwrap())
            .unwrap();
        let table = builder.build();
        assert_eq!(task_a.as_str(), "task_1");
        assert_eq!(task_b.as_str(), "task_2");
        assert_eq!(src.as_str(), "src_1");
        assert_eq!(planned.as_str(), "plan_1");
        assert_eq!(unavailable.as_str(), "unav_1");
        assert_eq!(table.scope()[0].criterion_refs[1].as_str(), "crit_1_2");
        assert_eq!(table.scope()[1].criterion_refs[0].as_str(), "crit_2_1");
        // Same criterion ID under two tasks are different handles bound to their task.
        let crit_b = Handle::new("crit_2_1").unwrap();
        assert!(table.criterion_belongs_to(&task_b, &crit_b));
        assert!(!table.criterion_belongs_to(&task_a, &crit_b));
        assert_eq!(table.task(&task_b).unwrap().task_id, "t-b");
        assert_eq!(Namespace::of(&src), Some(Namespace::Existing));
        assert_eq!(Namespace::of(&Handle::new("cargo_check").unwrap()), None);
        assert!(table.has_check(&Handle::new("cargo_check").unwrap()));
        assert_eq!(table.destination_for_path(&path("SRC/LIB.RS")), Some(&src));
    }

    #[test]
    fn identity_collisions_and_reserved_prefixes_are_rejected() {
        let mut builder = ReferenceTable::builder();
        assert!(builder.add_task("t", 1, "T", &[]).is_err());
        builder.add_task("t", 1, "T", &criteria(&["AC-1"])).unwrap();
        assert!(builder.add_task("t", 1, "T", &criteria(&["AC-1"])).is_err());
        assert!(
            builder
                .add_task("u", 1, "U", &criteria(&["AC-1", "AC-1"]))
                .is_err()
        );
        let src = builder
            .add_existing(path("src/Lib.rs"), Sha256Hex::of(b"a"), SourceRole::Source)
            .unwrap();
        // Same path + same hash: same handle. Different hash or case alias: conflict.
        assert_eq!(
            builder
                .add_existing(path("src/Lib.rs"), Sha256Hex::of(b"a"), SourceRole::Source)
                .unwrap(),
            src
        );
        assert!(
            builder
                .add_existing(path("src/Lib.rs"), Sha256Hex::of(b"b"), SourceRole::Source)
                .is_err()
        );
        assert!(
            builder
                .add_planned(path("SRC/lib.rs"), SourceRole::Source)
                .is_err()
        );
        assert!(builder.add_check(Handle::new("task_9").unwrap()).is_err());
        assert!(builder.add_check(Handle::new("anchor_x").unwrap()).is_err());
        builder.add_check(Handle::new("behavior").unwrap()).unwrap();
        assert!(builder.add_check(Handle::new("behavior").unwrap()).is_err());
        // A candidate may share the existing path (next version) but not another candidate.
        let unit = Uuid::generate();
        builder
            .add_candidate(
                path("src/lib.rs"),
                Sha256Hex::of(b"c"),
                unit.clone(),
                SourceRole::Source,
            )
            .unwrap();
        assert!(
            builder
                .add_candidate(
                    path("src/lib.rs"),
                    Sha256Hex::of(b"d"),
                    unit,
                    SourceRole::Source
                )
                .is_err()
        );
    }

    #[test]
    fn anchors_bind_exact_bytes_on_utf8_boundaries() {
        let bytes = "fn a() {}\r\nfn é() {}\nfn c() {}".as_bytes();
        let mut builder = ReferenceTable::builder();
        let src = builder
            .add_existing(path("src/lib.rs"), Sha256Hex::of(bytes), SourceRole::Source)
            .unwrap();
        // Line 2 including its LF; CRLF of line 1 untouched.
        let anchor = builder.add_line_anchor(&src, bytes, 2, 2).unwrap();
        let table = builder.build();
        let binding = table.anchor(&anchor).unwrap();
        assert_eq!(binding.start_byte, 11);
        assert_eq!(binding.end_byte, 11 + "fn é() {}\n".len() as u32);
        assert_eq!(
            binding.region_sha256,
            Sha256Hex::of("fn é() {}\n".as_bytes())
        );
        assert_eq!(binding.source_sha256, Sha256Hex::of(bytes));
        assert_eq!(
            line_span(bytes, 3, 3),
            Some((11 + "fn é() {}\n".len(), bytes.len()))
        );
        assert_eq!(line_span(bytes, 4, 4), None);
        assert_eq!(line_span(bytes, 2, 1), None);
        assert_eq!(line_span(bytes, 0, 1), None);

        let mut builder = ReferenceTable::builder();
        let src = builder
            .add_existing(path("src/lib.rs"), Sha256Hex::of(bytes), SourceRole::Source)
            .unwrap();
        // Inside the two-byte `é` is not a char boundary.
        let inside = 11 + "fn ".len() + 1;
        assert!(
            builder
                .add_anchor(&src, bytes, inside, bytes.len())
                .is_err()
        );
        assert!(builder.add_anchor(&src, bytes, 5, 2).is_err());
        assert!(builder.add_anchor(&src, bytes, 0, bytes.len() + 1).is_err());
        // Stale bytes cannot issue an anchor.
        assert!(builder.add_anchor(&src, b"other", 0, 1).is_err());
        // Planned destinations have no bytes to anchor.
        let planned = builder
            .add_planned(path("src/new.rs"), SourceRole::Source)
            .unwrap();
        assert!(builder.add_anchor(&planned, b"", 0, 0).is_err());
        assert_eq!(
            builder
                .add_anchor(&src, bytes, 0, bytes.len())
                .unwrap()
                .as_str(),
            "anchor_1"
        );
    }

    #[test]
    fn note_paths_are_deterministic_ascii_slugs() {
        assert_eq!(slug("Login: Sessão de Usuário!"), "login-sess-o-de-usu-rio");
        assert_eq!(slug("   "), "untitled");
        assert_eq!(slug("A--B"), "a-b");
        assert_eq!(
            vault_note_path("features", "User Login").unwrap().as_str(),
            "knowledge/obsidian/features/user-login.md"
        );
    }
}
