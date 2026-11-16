//! Cohesive unit planning from approved proposals (SC-004, SC-006; M3).
//!
//! The Specification proposes units and the Contract binds them; the host
//! turns those proposals into work units it can execute safely:
//!
//! 1. writers of the same file become one unit (ownership is exclusive);
//! 2. dependency cycles collapse into one unit (a cycle is not executable
//!    as separate calls and never means independence);
//! 3. a unit that reads a file another unit writes depends on that unit and
//!    receives the validated candidate, never the baseline;
//! 4. small independent units of the same task are coalesced so a cheap
//!    model is not paid one prompt per tiny fragment;
//! 5. anything above the plan limits fails admission before any call.
//!
//! The result is deterministic for identical inputs, so a Retry generation
//! reproduces the same logical unit keys and can reuse validated results.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::{
    CompositionError, ErrorCode, Handle, Invariant, MAX_AGGREGATE_ARTIFACTS, MAX_UNITS_PER_PLAN,
    RelPath, SourceRole, TaskScopeV1, Uuid,
};

/// Combined estimated output above which two independent units of the same
/// task are no longer coalesced.
pub const COALESCE_OUTPUT_TOKENS: u32 = 2_048;

/// A resolved reference of the current stage table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRef {
    pub handle: Handle,
    pub path: RelPath,
}

/// A destination a proposal wants to write, resolved to the current table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    pub handle: Handle,
    pub path: RelPath,
    pub role: SourceRole,
    /// Size of the delivered baseline; `None` for a planned (absent) file.
    pub baseline_bytes: Option<usize>,
}

/// One approved proposal after the recipe resolved its handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalNode {
    pub key: Handle,
    pub task_refs: Vec<Handle>,
    pub criterion_refs: Vec<Handle>,
    pub inputs: Vec<PathRef>,
    pub destinations: Vec<Destination>,
    /// Proposal keys this one depends on (proposal `dependsOn` ∪ binding).
    pub depends_on: Vec<Handle>,
    pub objective: String,
}

/// A file a unit reads as the validated output of a predecessor unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateRead {
    pub path: RelPath,
    pub role: SourceRole,
    pub producer_key: Handle,
    pub producer_unit_id: Uuid,
    /// Handle of the baseline entry for the same path, when one exists.
    pub baseline_handle: Option<Handle>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedUnit {
    pub key: Handle,
    pub unit_id: Uuid,
    /// Proposal keys merged into this unit, in proposal order.
    pub members: Vec<Handle>,
    pub task_refs: Vec<Handle>,
    pub criterion_refs: Vec<Handle>,
    pub depends_on: Vec<Handle>,
    /// Baseline inputs still valid for this unit (not produced by a predecessor).
    pub input_refs: Vec<Handle>,
    pub owned_destination_refs: Vec<Handle>,
    pub candidate_reads: Vec<CandidateRead>,
    pub objective: String,
    pub estimated_output_tokens: u32,
}

fn admission(code: ErrorCode, message: impl Into<String>) -> CompositionError {
    CompositionError::new(Invariant::Sc006, code, message)
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, index: usize) -> usize {
        let mut root = index;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        let mut cursor = index;
        while self.parent[cursor] != root {
            let next = self.parent[cursor];
            self.parent[cursor] = root;
            cursor = next;
        }
        root
    }

    /// Unions into the smaller index so group identity is deterministic.
    fn union(&mut self, a: usize, b: usize) -> bool {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return false;
        }
        let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
        self.parent[hi] = lo;
        true
    }
}

/// Strongly connected components (Tarjan) over `edges[i]` = successors of `i`.
fn strongly_connected_components(edges: &[BTreeSet<usize>]) -> Vec<Vec<usize>> {
    struct State<'a> {
        edges: &'a [BTreeSet<usize>],
        index: usize,
        indices: Vec<Option<usize>>,
        low: Vec<usize>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        components: Vec<Vec<usize>>,
    }
    fn visit(state: &mut State<'_>, v: usize) {
        state.indices[v] = Some(state.index);
        state.low[v] = state.index;
        state.index += 1;
        state.stack.push(v);
        state.on_stack[v] = true;
        let successors = state.edges[v].iter().copied().collect::<Vec<_>>();
        for w in successors {
            match state.indices[w] {
                None => {
                    visit(state, w);
                    state.low[v] = state.low[v].min(state.low[w]);
                }
                Some(index) if state.on_stack[w] => {
                    state.low[v] = state.low[v].min(index);
                }
                Some(_) => {}
            }
        }
        if Some(state.low[v]) == state.indices[v] {
            let mut component = Vec::new();
            while let Some(w) = state.stack.pop() {
                state.on_stack[w] = false;
                component.push(w);
                if w == v {
                    break;
                }
            }
            component.sort_unstable();
            state.components.push(component);
        }
    }
    let n = edges.len();
    let mut state = State {
        edges,
        index: 0,
        indices: vec![None; n],
        low: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        components: Vec::new(),
    };
    for v in 0..n {
        if state.indices[v].is_none() {
            visit(&mut state, v);
        }
    }
    state.components
}

/// Whether `to` is reachable from `from` over `edges`.
fn has_path(edges: &[BTreeSet<usize>], from: usize, to: usize) -> bool {
    let mut seen = vec![false; edges.len()];
    let mut stack = vec![from];
    while let Some(node) = stack.pop() {
        if node == to {
            return true;
        }
        if seen[node] {
            continue;
        }
        seen[node] = true;
        stack.extend(edges[node].iter().copied().filter(|next| !seen[*next]));
    }
    false
}

/// Plans the units of a Code or Test stage from resolved proposals.
pub fn plan_units(
    nodes: &[ProposalNode],
    scope: &[TaskScopeV1],
) -> Result<Vec<PlannedUnit>, CompositionError> {
    if nodes.is_empty() {
        return Ok(Vec::new());
    }
    let key_index: HashMap<&Handle, usize> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (&node.key, index))
        .collect();
    if key_index.len() != nodes.len() {
        return Err(admission(
            ErrorCode::HostContractError,
            "Proposals reuse a unit key.",
        ));
    }
    let mut groups = UnionFind::new(nodes.len());

    // Steps 1-3 to a fixpoint: merging can create new shared files or cycles.
    loop {
        let mut changed = false;
        // 1. Same-file writers.
        let mut writers: HashMap<String, usize> = HashMap::new();
        for (index, node) in nodes.iter().enumerate() {
            for destination in &node.destinations {
                match writers.get(&destination.path.canonical_key()) {
                    Some(previous) => changed |= groups.union(*previous, index),
                    None => {
                        writers.insert(destination.path.canonical_key(), index);
                    }
                }
            }
        }
        // 2-3. Dependency edges between groups: declared and read-after-write.
        let edges = group_edges(nodes, &mut groups, &key_index);
        for component in strongly_connected_components(&edges) {
            for pair in component.windows(2) {
                changed |= groups.union(pair[0], pair[1]);
            }
        }
        if !changed {
            break;
        }
    }

    // Ordered groups: (earliest task in scope, earliest proposal).
    let task_position: HashMap<&Handle, usize> = scope
        .iter()
        .enumerate()
        .map(|(position, task)| (&task.task_ref, position))
        .collect();
    let criterion_position: HashMap<&Handle, (usize, usize)> = scope
        .iter()
        .enumerate()
        .flat_map(|(task_position, task)| {
            task.criterion_refs
                .iter()
                .enumerate()
                .map(move |(position, criterion)| (criterion, (task_position, position)))
        })
        .collect();
    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for index in 0..nodes.len() {
        members.entry(groups.find(index)).or_default().push(index);
    }
    let mut ordered = members.into_values().collect::<Vec<_>>();
    let group_rank = |group: &[usize]| {
        let task = group
            .iter()
            .flat_map(|index| nodes[*index].task_refs.iter())
            .filter_map(|task| task_position.get(task))
            .min()
            .copied()
            .unwrap_or(usize::MAX);
        (task, group[0])
    };
    ordered.sort_by_key(|group| group_rank(group));

    // 4. Coalesce adjacent independent small units of the same task.
    let estimate = |group: &[usize]| -> u32 {
        let criteria = group
            .iter()
            .flat_map(|index| nodes[*index].criterion_refs.iter())
            .collect::<BTreeSet<_>>()
            .len() as u32;
        let mut seen = BTreeSet::new();
        let destinations: u32 = group
            .iter()
            .flat_map(|index| nodes[*index].destinations.iter())
            .filter(|destination| seen.insert(destination.path.canonical_key()))
            .map(|destination| match destination.baseline_bytes {
                Some(bytes) => (bytes as u32 / 4).min(1_024) + 128,
                None => 512,
            })
            .sum();
        256 + destinations + 64 * criteria
    };
    let mut index = 0usize;
    while index + 1 < ordered.len() {
        let edges = ordered_edges(nodes, &ordered, &key_index);
        let (a, b) = (index, index + 1);
        let share_task = ordered[a].iter().any(|i| {
            ordered[b].iter().any(|j| {
                nodes[*i]
                    .task_refs
                    .iter()
                    .any(|t| nodes[*j].task_refs.contains(t))
            })
        });
        let independent = !has_path(&edges, a, b) && !has_path(&edges, b, a);
        let small = estimate(&ordered[a]) + estimate(&ordered[b]) <= COALESCE_OUTPUT_TOKENS;
        if share_task && independent && small {
            let mut merged = ordered[a].clone();
            merged.extend(ordered[b].iter().copied());
            merged.sort_unstable();
            ordered[a] = merged;
            ordered.remove(b);
            continue;
        }
        index += 1;
    }

    // 5. Limits before any call exists.
    if ordered.len() > MAX_UNITS_PER_PLAN {
        return Err(admission(
            ErrorCode::CapacityExceeded,
            format!(
                "The approved proposals need {} units after grouping; a plan admits at most {MAX_UNITS_PER_PLAN}.",
                ordered.len()
            ),
        ));
    }
    let distinct_destinations = nodes
        .iter()
        .flat_map(|node| node.destinations.iter())
        .map(|destination| destination.path.canonical_key())
        .collect::<BTreeSet<_>>();
    if distinct_destinations.len() > MAX_AGGREGATE_ARTIFACTS {
        return Err(admission(
            ErrorCode::CapacityExceeded,
            format!(
                "The approved proposals write {} files; an aggregate admits at most {MAX_AGGREGATE_ARTIFACTS} artifacts.",
                distinct_destinations.len()
            ),
        ));
    }

    // Emit units in group order with candidate reads resolved by writer.
    let edges = ordered_edges(nodes, &ordered, &key_index);
    let unit_ids = ordered.iter().map(|_| Uuid::generate()).collect::<Vec<_>>();
    let group_of: HashMap<usize, usize> = ordered
        .iter()
        .enumerate()
        .flat_map(|(group, indices)| indices.iter().map(move |index| (*index, group)))
        .collect();
    let writer_of: HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .flat_map(|(index, node)| {
            let group = group_of[&index];
            node.destinations
                .iter()
                .map(move |destination| (destination.path.canonical_key(), group))
        })
        .collect();
    let mut units = Vec::with_capacity(ordered.len());
    for (group, indices) in ordered.iter().enumerate() {
        let key = nodes[indices[0]].key.clone();
        let mut tasks = indices
            .iter()
            .flat_map(|index| nodes[*index].task_refs.iter().cloned())
            .collect::<Vec<_>>();
        tasks.sort_by_key(|task| task_position.get(task).copied().unwrap_or(usize::MAX));
        tasks.dedup();
        let mut criteria = indices
            .iter()
            .flat_map(|index| nodes[*index].criterion_refs.iter().cloned())
            .collect::<Vec<_>>();
        criteria.sort_by_key(|criterion| {
            criterion_position
                .get(criterion)
                .copied()
                .unwrap_or((usize::MAX, usize::MAX))
        });
        criteria.dedup();
        let mut owned = Vec::new();
        let mut owned_keys = BTreeSet::new();
        for destination in indices
            .iter()
            .flat_map(|index| nodes[*index].destinations.iter())
        {
            if owned_keys.insert(destination.path.canonical_key()) {
                owned.push(destination.handle.clone());
            }
        }
        let mut inputs = Vec::new();
        let mut candidate_reads = Vec::new();
        let mut seen_inputs = BTreeSet::new();
        for input in indices.iter().flat_map(|index| nodes[*index].inputs.iter()) {
            let path_key = input.path.canonical_key();
            if !seen_inputs.insert(path_key.clone()) {
                continue;
            }
            match writer_of.get(&path_key) {
                Some(writer) if *writer != group => {
                    let producer = &ordered[*writer];
                    let role = nodes[producer[0]]
                        .destinations
                        .iter()
                        .chain(producer.iter().flat_map(|i| nodes[*i].destinations.iter()))
                        .find(|d| d.path.canonical_key() == path_key)
                        .map(|d| d.role)
                        .unwrap_or(SourceRole::Source);
                    candidate_reads.push(CandidateRead {
                        path: input.path.clone(),
                        role,
                        producer_key: nodes[producer[0]].key.clone(),
                        producer_unit_id: unit_ids[*writer].clone(),
                        baseline_handle: Some(input.handle.clone()),
                    });
                }
                _ => inputs.push(input.handle.clone()),
            }
        }
        for handle in &owned {
            if !inputs.contains(handle) {
                inputs.push(handle.clone());
            }
        }
        let depends_on = edges[group]
            .iter()
            .map(|successor| nodes[ordered[*successor][0]].key.clone())
            .collect::<Vec<_>>();
        let objective = indices
            .iter()
            .map(|index| nodes[*index].objective.trim())
            .collect::<Vec<_>>()
            .join(" ");
        units.push(PlannedUnit {
            key,
            unit_id: unit_ids[group].clone(),
            members: indices
                .iter()
                .map(|index| nodes[*index].key.clone())
                .collect(),
            task_refs: tasks,
            criterion_refs: criteria,
            depends_on,
            input_refs: inputs,
            owned_destination_refs: owned,
            candidate_reads,
            objective,
            estimated_output_tokens: estimate(indices),
        });
    }
    Ok(units)
}

/// Edges between current union-find groups (root indices), from declared
/// dependencies and read-after-write relations.
fn group_edges(
    nodes: &[ProposalNode],
    groups: &mut UnionFind,
    key_index: &HashMap<&Handle, usize>,
) -> Vec<BTreeSet<usize>> {
    let mut edges = vec![BTreeSet::new(); nodes.len()];
    let writers: HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .flat_map(|(index, node)| {
            node.destinations
                .iter()
                .map(move |destination| (destination.path.canonical_key(), index))
        })
        .collect();
    for (index, node) in nodes.iter().enumerate() {
        let from = groups.find(index);
        for dependency in &node.depends_on {
            if let Some(target) = key_index.get(dependency) {
                let to = groups.find(*target);
                if to != from {
                    edges[from].insert(to);
                }
            }
        }
        for input in &node.inputs {
            if let Some(writer) = writers.get(&input.path.canonical_key()) {
                let to = groups.find(*writer);
                if to != from {
                    edges[from].insert(to);
                }
            }
        }
    }
    edges
}

/// Edges between ordered groups (positions in `ordered`).
fn ordered_edges(
    nodes: &[ProposalNode],
    ordered: &[Vec<usize>],
    key_index: &HashMap<&Handle, usize>,
) -> Vec<BTreeSet<usize>> {
    let group_of: HashMap<usize, usize> = ordered
        .iter()
        .enumerate()
        .flat_map(|(group, indices)| indices.iter().map(move |index| (*index, group)))
        .collect();
    let writers: HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .flat_map(|(index, node)| {
            let group = group_of[&index];
            node.destinations
                .iter()
                .map(move |destination| (destination.path.canonical_key(), group))
        })
        .collect();
    let mut edges = vec![BTreeSet::new(); ordered.len()];
    for (group, indices) in ordered.iter().enumerate() {
        for index in indices {
            for dependency in &nodes[*index].depends_on {
                if let Some(target) = key_index.get(dependency) {
                    let to = group_of[target];
                    if to != group {
                        edges[group].insert(to);
                    }
                }
            }
            for input in &nodes[*index].inputs {
                if let Some(to) = writers.get(&input.path.canonical_key())
                    && *to != group
                {
                    edges[group].insert(*to);
                }
            }
        }
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(text: &str) -> Handle {
        Handle::new(text).unwrap()
    }

    fn scope() -> Vec<TaskScopeV1> {
        vec![
            TaskScopeV1 {
                task_ref: h("task_1"),
                criterion_refs: vec![h("crit_1_1"), h("crit_1_2")],
            },
            TaskScopeV1 {
                task_ref: h("task_2"),
                criterion_refs: vec![h("crit_2_1")],
            },
        ]
    }

    fn source(handle: &str, path: &str) -> PathRef {
        PathRef {
            handle: h(handle),
            path: RelPath::new(path).unwrap(),
        }
    }

    fn existing(handle: &str, path: &str) -> Destination {
        Destination {
            handle: h(handle),
            path: RelPath::new(path).unwrap(),
            role: SourceRole::Source,
            baseline_bytes: Some(2_000),
        }
    }

    fn planned(handle: &str, path: &str) -> Destination {
        Destination {
            handle: h(handle),
            path: RelPath::new(path).unwrap(),
            role: SourceRole::Source,
            baseline_bytes: None,
        }
    }

    fn node(
        key: &str,
        task: &str,
        criteria: &[&str],
        inputs: Vec<PathRef>,
        destinations: Vec<Destination>,
        depends: &[&str],
    ) -> ProposalNode {
        ProposalNode {
            key: h(key),
            task_refs: vec![h(task)],
            criterion_refs: criteria.iter().map(|c| h(c)).collect(),
            inputs,
            destinations,
            depends_on: depends.iter().map(|d| h(d)).collect(),
            objective: format!("Do {key}."),
        }
    }

    #[test]
    fn same_file_writers_and_cycles_become_one_unit() {
        let nodes = vec![
            node(
                "a",
                "task_1",
                &["crit_1_1"],
                vec![],
                vec![existing("src_1", "src/a.rs")],
                &[],
            ),
            node(
                "b",
                "task_1",
                &["crit_1_2"],
                vec![],
                vec![existing("src_1", "src/a.rs")],
                &[],
            ),
            node(
                "c",
                "task_2",
                &["crit_2_1"],
                vec![],
                vec![existing("src_2", "src/c.rs")],
                &["d"],
            ),
            node(
                "d",
                "task_2",
                &["crit_2_1"],
                vec![],
                vec![existing("src_3", "src/d.rs")],
                &["c"],
            ),
        ];
        let units = plan_units(&nodes, &scope()).unwrap();
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].members, vec![h("a"), h("b")]);
        assert_eq!(units[0].criterion_refs, vec![h("crit_1_1"), h("crit_1_2")]);
        assert_eq!(units[0].owned_destination_refs, vec![h("src_1")]);
        assert_eq!(units[1].members, vec![h("c"), h("d")]);
        assert!(
            units[1].depends_on.is_empty(),
            "a collapsed cycle has no self edge"
        );
        assert_eq!(
            units[1].owned_destination_refs,
            vec![h("src_2"), h("src_3")]
        );
    }

    #[test]
    fn a_reader_of_another_units_output_depends_on_it_and_gets_the_candidate() {
        let nodes = vec![
            node(
                "consumer",
                "task_1",
                &["crit_1_1"],
                vec![
                    source("src_2", "src/lib.rs"),
                    source("src_3", "src/util.rs"),
                ],
                vec![existing("src_1", "src/app.rs")],
                &[],
            ),
            node(
                "producer",
                "task_1",
                &["crit_1_2"],
                vec![],
                vec![existing("src_2", "src/lib.rs")],
                &[],
            ),
        ];
        // Large enough estimates that coalescing does not fold them together.
        let mut nodes = nodes;
        nodes[0].destinations[0].baseline_bytes = Some(8_000);
        nodes[1].destinations[0].baseline_bytes = Some(8_000);
        let units = plan_units(&nodes, &scope()).unwrap();
        assert_eq!(units.len(), 2);
        let consumer = units.iter().find(|u| u.key == h("consumer")).unwrap();
        let producer = units.iter().find(|u| u.key == h("producer")).unwrap();
        assert_eq!(consumer.depends_on, vec![h("producer")]);
        assert_eq!(consumer.candidate_reads.len(), 1);
        assert_eq!(
            consumer.candidate_reads[0].producer_unit_id,
            producer.unit_id
        );
        assert_eq!(
            consumer.candidate_reads[0].baseline_handle,
            Some(h("src_2"))
        );
        // The baseline of the produced file is not delivered; the other input is.
        assert_eq!(consumer.input_refs, vec![h("src_3"), h("src_1")]);
        assert!(producer.candidate_reads.is_empty());
    }

    #[test]
    fn mutual_reads_and_writes_collapse_into_one_unit() {
        let nodes = vec![
            node(
                "x",
                "task_1",
                &["crit_1_1"],
                vec![source("src_2", "src/b.rs")],
                vec![existing("src_1", "src/a.rs")],
                &[],
            ),
            node(
                "y",
                "task_1",
                &["crit_1_2"],
                vec![source("src_1", "src/a.rs")],
                vec![existing("src_2", "src/b.rs")],
                &[],
            ),
        ];
        let units = plan_units(&nodes, &scope()).unwrap();
        assert_eq!(units.len(), 1);
        assert!(units[0].candidate_reads.is_empty());
        assert_eq!(units[0].input_refs, vec![h("src_2"), h("src_1")]);
    }

    #[test]
    fn small_independent_units_of_one_task_coalesce_but_other_tasks_do_not() {
        let nodes = vec![
            node(
                "p",
                "task_1",
                &["crit_1_1"],
                vec![],
                vec![planned("plan_1", "src/p.rs")],
                &[],
            ),
            node(
                "q",
                "task_1",
                &["crit_1_2"],
                vec![],
                vec![planned("plan_2", "src/q.rs")],
                &[],
            ),
            node(
                "r",
                "task_2",
                &["crit_2_1"],
                vec![],
                vec![planned("plan_3", "src/r.rs")],
                &[],
            ),
        ];
        let units = plan_units(&nodes, &scope()).unwrap();
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].members, vec![h("p"), h("q")]);
        assert_eq!(
            units[0].owned_destination_refs,
            vec![h("plan_1"), h("plan_2")]
        );
        assert_eq!(units[1].members, vec![h("r")]);
        // Deterministic: the same inputs produce the same keys and order.
        let again = plan_units(&nodes, &scope()).unwrap();
        assert_eq!(
            again.iter().map(|u| &u.key).collect::<Vec<_>>(),
            units.iter().map(|u| &u.key).collect::<Vec<_>>()
        );
    }

    #[test]
    fn plans_above_the_limits_fail_before_any_call() {
        let nodes = (0..17)
            .map(|i| {
                let mut n = node(
                    &format!("u{i}"),
                    if i < 9 { "task_1" } else { "task_2" },
                    &["crit_1_1"],
                    vec![],
                    vec![existing(&format!("src_{}", i + 1), &format!("src/f{i}.rs"))],
                    &[],
                );
                n.destinations[0].baseline_bytes = Some(20_000);
                n
            })
            .collect::<Vec<_>>();
        let error = plan_units(&nodes, &scope()).unwrap_err();
        assert_eq!(error.code, ErrorCode::CapacityExceeded);
    }
}
