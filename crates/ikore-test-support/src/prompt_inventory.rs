//! The prompt input inventory of ADR-037 (specification, section 1): every
//! section a pipeline prompt builder emits, with the trust class of its text.
//!
//! [`check_prompt`] fails when a prompt has a section the inventory does not
//! classify, when a labelled section or item lacks its provenance line, or
//! when the line names another class. The composition benchmark runs it on
//! every prompt it serves, so a new section without a class fails the
//! `prompt_input_inventory` test.

use ikore_contracts::exchange::TrustClass;

/// The prompt builders M1 labels; their system prompts tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builder {
    /// `render_unit_prompt`, pipeline v4.
    V4Unit,
    /// `build_stage_prompt`, the legacy pipeline.
    Legacy,
}

/// How a section is labelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    /// Host text: no provenance line.
    Host,
    /// One line under the heading, of this class.
    Required(TrustClass),
    /// A line of this class when the content is not the host's own (the
    /// objective of a planned unit).
    Optional(TrustClass),
    /// Each `### ` item carries its own line, of one of these classes.
    Items(&'static [TrustClass]),
}

#[derive(Debug, Clone, Copy)]
pub struct InventorySection {
    pub builder: Builder,
    pub heading: &'static str,
    pub label: Label,
}

const FILES: &[TrustClass] = &[TrustClass::Repository, TrustClass::AgentOutput];
const AGENT: &[TrustClass] = &[TrustClass::AgentOutput];
const WORKSPACE: &[TrustClass] = &[TrustClass::Repository];

const fn section(builder: Builder, heading: &'static str, label: Label) -> InventorySection {
    InventorySection {
        builder,
        heading,
        label,
    }
}

/// Tables 1.2 and 1.3 of the specification.
pub const PROMPT_INPUT_INVENTORY: &[InventorySection] = &[
    section(
        Builder::V4Unit,
        "Objective",
        Label::Optional(TrustClass::AgentOutput),
    ),
    section(Builder::V4Unit, "Scope of this unit", Label::Host),
    section(
        Builder::V4Unit,
        "Tasks and criteria",
        Label::Required(TrustClass::AgentOutput),
    ),
    section(
        Builder::V4Unit,
        "Decisions",
        Label::Required(TrustClass::AgentOutput),
    ),
    section(
        Builder::V4Unit,
        "Interfaces",
        Label::Required(TrustClass::AgentOutput),
    ),
    section(Builder::V4Unit, "References", Label::Host),
    section(Builder::V4Unit, "Delivered content", Label::Items(FILES)),
    section(Builder::V4Unit, "Risk scrutiny", Label::Host),
    section(
        Builder::V4Unit,
        "Review policy (read-only)",
        Label::Required(TrustClass::Repository),
    ),
    section(Builder::V4Unit, "Host evidence", Label::Host),
    section(Builder::V4Unit, "Evidence gaps", Label::Host),
    section(
        Builder::V4Unit,
        "Changes (base → candidate)",
        Label::Required(TrustClass::AgentOutput),
    ),
    section(
        Builder::V4Unit,
        "Candidate files (final, read-only)",
        Label::Items(AGENT),
    ),
    section(Builder::V4Unit, "Context files", Label::Items(FILES)),
    section(
        Builder::V4Unit,
        "Author claims (narrative, not evidence)",
        Label::Required(TrustClass::AgentOutput),
    ),
    section(Builder::V4Unit, "Dependency outputs", Label::Items(AGENT)),
    section(Builder::V4Unit, "Unknowns", Label::Host),
    section(Builder::V4Unit, "Correction", Label::Host),
    section(Builder::V4Unit, "Stage guidance", Label::Host),
    section(
        Builder::V4Unit,
        "Stage guidance (whole batch, shared by every unit)",
        Label::Host,
    ),
    section(Builder::V4Unit, "Response contract", Label::Host),
    section(Builder::Legacy, "Pulsation", Label::Host),
    section(
        Builder::Legacy,
        "Task batch - authoritative scope",
        Label::Required(TrustClass::AgentOutput),
    ),
    section(Builder::Legacy, "Objective", Label::Host),
    section(Builder::Legacy, "Approved handoff", Label::Host),
    section(
        Builder::Legacy,
        "Exact code context (derived - not citable)",
        Label::Required(TrustClass::Repository),
    ),
    section(
        Builder::Legacy,
        "Retrieved context files",
        Label::Items(WORKSPACE),
    ),
    section(Builder::Legacy, "Previous attempt correction", Label::Host),
    section(Builder::Legacy, "Output schema", Label::Host),
];

/// Where an embedded block's provenance line sits relative to its marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// The closest earlier provenance line labels the block.
    Before,
    /// The very next line labels it.
    After,
}

#[derive(Debug, Clone, Copy)]
pub struct EmbeddedBlock {
    pub builder: Builder,
    pub marker: &'static str,
    pub position: Position,
    pub trust: TrustClass,
}

const fn block(
    builder: Builder,
    marker: &'static str,
    position: Position,
    trust: TrustClass,
) -> EmbeddedBlock {
    EmbeddedBlock {
        builder,
        marker,
        position,
        trust,
    }
}

const HOST_DESTINATION_ROW: &str = "task_<number> file candidates (<origin>):";

/// Blocks a host section embeds (adjustments A1, A3 and A5).
pub const EMBEDDED_BLOCKS: &[EmbeddedBlock] = &[
    block(
        Builder::V4Unit,
        "task_",
        Position::Before,
        TrustClass::AgentOutput,
    ),
    block(
        Builder::V4Unit,
        HOST_DESTINATION_ROW,
        Position::Before,
        TrustClass::Host,
    ),
    block(
        Builder::V4Unit,
        "Feature `",
        Position::Before,
        TrustClass::AgentOutput,
    ),
    block(
        Builder::V4Unit,
        "Revision feedback from the developer:",
        Position::Before,
        TrustClass::Human,
    ),
    block(
        Builder::V4Unit,
        "Clarification question:",
        Position::Before,
        TrustClass::AgentOutput,
    ),
    block(
        Builder::V4Unit,
        "Clarification answer:",
        Position::Before,
        TrustClass::Human,
    ),
    block(
        Builder::V4Unit,
        "Issues reported by Intake:",
        Position::Before,
        TrustClass::AgentOutput,
    ),
    block(
        Builder::V4Unit,
        "Approved interface intent:",
        Position::Before,
        TrustClass::AgentOutput,
    ),
    block(
        Builder::V4Unit,
        "Approved unit proposals",
        Position::Before,
        TrustClass::AgentOutput,
    ),
    block(
        Builder::V4Unit,
        "Host check diagnostics:",
        Position::After,
        TrustClass::ToolOutput,
    ),
    block(
        Builder::V4Unit,
        "Transport diagnostics:",
        Position::After,
        TrustClass::ToolOutput,
    ),
    block(
        Builder::Legacy,
        "Your complete previous raw output was:",
        Position::After,
        TrustClass::AgentOutput,
    ),
];

const LINE_PREFIX: &str = "> provenance: ";
const LEGEND_START: &str = "PROVENANCE: An item's \"> provenance:\" line";

/// Which builder rendered a prompt; `None` for prompts M1 does not label.
pub fn builder_of(system: &str) -> Option<Builder> {
    if system.starts_with("You are the `") && system.contains("` worker of the `") {
        Some(Builder::V4Unit)
    } else if system.contains("` stage agent in Ikore") {
        Some(Builder::Legacy)
    } else {
        None
    }
}

fn labels(line: &str, trust: TrustClass) -> bool {
    line.strip_prefix(LINE_PREFIX)
        .and_then(|rest| rest.strip_prefix(trust.as_str()))
        .is_some_and(|rest| rest.starts_with(ikore_contracts::exchange::SEPARATOR))
}

/// The host's frozen file plan is a separate row shape from `task_1:` scope
/// prose. Only this exact renderer shape may use host provenance; every other
/// `task_` line retains the agent-output requirement.
fn is_host_destination_row(line: &str) -> bool {
    let Some((number, row)) = line
        .strip_prefix("task_")
        .and_then(|tail| tail.split_once(" file candidates ("))
    else {
        return false;
    };
    if number.is_empty()
        || number.starts_with('0')
        || !number.bytes().all(|byte| byte.is_ascii_digit())
    {
        return false;
    }
    let Some((origin, paths)) = row.split_once("): ") else {
        return false;
    };
    matches!(origin, "Manual" | "TaskText" | "RepositoryConvention") && !paths.trim().is_empty()
}

fn matches_embedded_block(block: &EmbeddedBlock, line: &str) -> bool {
    if block.marker == HOST_DESTINATION_ROW {
        is_host_destination_row(line)
    } else if block.marker == "task_" {
        line.starts_with(block.marker) && !is_host_destination_row(line)
    } else {
        line.starts_with(block.marker)
    }
}

/// Checks one rendered prompt against the inventory. Prompts of other
/// builders (the Visualizer, for one) are outside M1 and pass.
pub fn check_prompt(system: &str, user: &str) -> Result<(), Vec<String>> {
    let Some(builder) = builder_of(system) else {
        return Ok(());
    };
    let mut errors = Vec::new();
    if !system.contains(LEGEND_START) {
        errors.push(format!(
            "{builder:?}: the system prompt lacks the provenance legend"
        ));
    }
    if system.contains("TRUST:") {
        errors.push(format!(
            "{builder:?}: the system prompt still has the old TRUST sentence"
        ));
    }
    let lines = user.lines().collect::<Vec<_>>();
    let mut current: Option<&InventorySection> = None;
    // Fenced content is data: a delivered note's own headings are not
    // prompt sections.
    let mut fence: Option<&str> = None;
    for (index, line) in lines.iter().enumerate() {
        if let Some(open) = fence {
            if *line == open {
                fence = None;
            }
            continue;
        }
        let ticks = line.len() - line.trim_start_matches('`').len();
        if ticks >= 3 {
            fence = Some(&line[..ticks]);
            continue;
        }
        if let Some(heading) = line.strip_prefix("## ") {
            current = PROMPT_INPUT_INVENTORY
                .iter()
                .find(|entry| entry.builder == builder && entry.heading == heading);
            let Some(entry) = current else {
                errors.push(format!(
                    "{builder:?} emits `## {heading}`, which the prompt input inventory does not classify"
                ));
                continue;
            };
            let next = lines.get(index + 1).copied().unwrap_or_default();
            match entry.label {
                Label::Required(trust) if !labels(next, trust) => errors.push(format!(
                    "{builder:?}: `## {heading}` needs a `{}` provenance line, found `{next}`",
                    trust.as_str()
                )),
                Label::Optional(trust) if next.starts_with(LINE_PREFIX) && !labels(next, trust) => {
                    errors.push(format!(
                        "{builder:?}: `## {heading}` carries `{next}`, expected `{}`",
                        trust.as_str()
                    ))
                }
                Label::Host if next.starts_with(LINE_PREFIX) => errors.push(format!(
                    "{builder:?}: host section `## {heading}` carries a provenance line"
                )),
                _ => {}
            }
            continue;
        }
        if let Some(InventorySection {
            label: Label::Items(allowed),
            heading,
            ..
        }) = current
            && line.starts_with("### ")
        {
            // The legacy title is followed by `score:`; the line comes after it.
            let mut probe = index + 1;
            if lines
                .get(probe)
                .is_some_and(|next| next.starts_with("score: "))
            {
                probe += 1;
            }
            let next = lines.get(probe).copied().unwrap_or_default();
            if !allowed.iter().any(|trust| labels(next, *trust)) {
                errors.push(format!(
                    "{builder:?}: item `{line}` of `## {heading}` lacks an allowed provenance line, found `{next}`"
                ));
            }
        }
        for block in EMBEDDED_BLOCKS
            .iter()
            .filter(|block| block.builder == builder && matches_embedded_block(block, line))
        {
            let found = match block.position {
                Position::After => lines.get(index + 1).copied(),
                Position::Before => lines[..index]
                    .iter()
                    .rev()
                    .find(|earlier| earlier.starts_with(LINE_PREFIX))
                    .copied(),
            };
            if !found.is_some_and(|candidate| labels(candidate, block.trust)) {
                errors.push(format!(
                    "{builder:?}: block `{}` needs a `{}` provenance line, found `{}`",
                    block.marker,
                    block.trust.as_str(),
                    found.unwrap_or_default()
                ));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
