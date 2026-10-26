//! Trust layers of a pulsation (ADR-035 M2).
//!
//! Four layers say what the host can tell from its own durable rows: who ran
//! the work (Identity), what it could write and wrote (Scope), which limits
//! held (Guardrails) and how the approved criteria stand (Intent).
//! [`derive_layers`] is pure: it reads no clock, file or database, and sorts
//! what it reads, so the same facts always give the same snapshot.
//!
//! `clear` means the host detected nothing, never that the work is safe.
//! `issue` repeats a grave fact the host already recorded and blocks nothing
//! new: what blocks stays with the stage-failure, convergence and risk rules
//! (ADR-021, ADR-032, ADR-034). The panel offers no decision (SC-021).

use std::collections::{BTreeMap, BTreeSet};

use hvc_terminal::terminals::model_identity::{ModelVerification, verify_model};
use serde::{Deserialize, Serialize};

use crate::orchestration::composition::risk::{CoverageRowKind, CoverageState};
use crate::orchestration::composition::{CheckStatus, Risk, Verdict};
use crate::orchestration::mandate::MandateCause;

/// Version of the rules below; a changed rule or threshold is a new version.
pub const LAYERS_POLICY_VERSION: u32 = 1;
/// Items kept per layer; the rest is counted in `truncated_count`.
pub const MAX_LAYER_ITEMS: usize = 50;
/// Guardrails asks for attention when less than this share of an extra
/// (repair) pool is left while its stage still runs (hypothesis, policy v1).
pub const LOW_EXTRA_BUDGET_PERCENT: u64 = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerKind {
    Identity,
    Scope,
    Guardrails,
    Intent,
}

impl LayerKind {
    pub const ALL: [Self; 4] = [Self::Identity, Self::Scope, Self::Guardrails, Self::Intent];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Scope => "scope",
            Self::Guardrails => "guardrails",
            Self::Intent => "intent",
        }
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::Identity => "Identity",
            Self::Scope => "Scope",
            Self::Guardrails => "Guardrails",
            Self::Intent => "Intent",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerState {
    Clear,
    Attention,
    Issue,
    NotApplicable,
    Unknown,
}

impl LayerState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Attention => "attention",
            Self::Issue => "issue",
            Self::NotApplicable => "not_applicable",
            Self::Unknown => "unknown",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Attention => "attention",
            Self::Issue => "issue",
            Self::NotApplicable => "not applicable",
            Self::Unknown => "unknown",
        }
    }

    /// Order of "the worst state": not applicable, clear, unknown, attention,
    /// issue.
    const fn severity(self) -> u8 {
        match self {
            Self::NotApplicable => 0,
            Self::Clear => 1,
            Self::Unknown => 2,
            Self::Attention => 3,
            Self::Issue => 4,
        }
    }

    pub const fn worse(self, other: Self) -> Self {
        if other.severity() > self.severity() {
            other
        } else {
            self
        }
    }

    /// The states the flow bar marks with a dot.
    pub const fn is_flagged(self) -> bool {
        matches!(self, Self::Attention | Self::Issue)
    }
}

/// One fact a layer shows. `state` marks the item that raised the layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerItemV1 {
    pub label: String,
    pub value: String,
    pub state: LayerState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerV1 {
    pub kind: LayerKind,
    pub state: LayerState,
    pub headline: String,
    pub items: Vec<LayerItemV1>,
    /// Items left out after [`MAX_LAYER_ITEMS`].
    pub truncated_count: usize,
    /// Why the layer is not clear, one line per rule that fired.
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PulsationLayersSnapshot {
    pub layers_policy_version: u32,
    /// Identity, Scope, Guardrails and Intent, in that order.
    pub layers: Vec<LayerV1>,
}

impl PulsationLayersSnapshot {
    pub fn layer(&self, kind: LayerKind) -> Option<&LayerV1> {
        self.layers.iter().find(|layer| layer.kind == kind)
    }

    /// The worst layer in attention or issue; the first in layer order wins a
    /// tie.
    pub fn worst_flagged(&self) -> Option<&LayerV1> {
        self.layers
            .iter()
            .filter(|layer| layer.state.is_flagged())
            .fold(None, |worst: Option<&LayerV1>, layer| match worst {
                Some(current) if current.state.worse(layer.state) == current.state => Some(current),
                _ => Some(layer),
            })
    }
}

// ------------------------------------------------------------------ facts

/// One model run the host recorded for the pulsation.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct RunFact {
    /// Executor kind in pipeline v4, agent role before it.
    pub actor: String,
    /// `api` or `cli`.
    pub transport: String,
    /// CLI backend or API provider.
    pub provider: Option<String>,
    pub requested_model: Option<String>,
    pub observed_model: Option<String>,
    pub adapter_version: Option<u32>,
    /// The run finished, so the model it ran should be known.
    pub completed: bool,
}

/// The mandate revision in force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MandateFact {
    pub revision: u32,
    pub cause: MandateCause,
    pub granted: usize,
    /// A CLI fallback moved a role to the API (a `transport_fallback` revision).
    pub transport_fallback: bool,
}

/// One checkpoint decision, from its approval receipt.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ApprovalFact {
    pub decided_at: String,
    pub stage: String,
    pub purpose: String,
    pub decision: String,
}

/// The latest publication of one path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PublicationFact {
    pub path: String,
    /// `pending`, `applying`, `applied`, `conflict` or `cancelled`.
    pub state: String,
    pub sensitive: bool,
}

/// A write or dispatch the mandate refused.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RefusalFact {
    /// `scope_violation` or `capability_mismatch`.
    pub rule: String,
    pub path: Option<String>,
    pub occurrences: u32,
}

/// One extra (repair) pool of the latest execution's budget.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PoolFact {
    pub name: String,
    pub left: u64,
    pub limit: u64,
}

/// The budget account of the latest composed execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetFact {
    pub stage: String,
    /// The execution is still planned, running, validating, ready or parked.
    pub active: bool,
    pub exhausted: bool,
    /// A unit of that execution is still pending or failed.
    pub unit_waiting: bool,
    pub extra_pools: Vec<PoolFact>,
}

/// One host check of the latest composed execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCheckFact {
    pub check_ref: String,
    pub required: bool,
    pub status: CheckStatus,
}

/// Usage records of the pulsation by completeness.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageFact {
    pub complete: u32,
    pub partial: u32,
    pub unavailable: u32,
}

impl UsageFact {
    pub const fn total(self) -> u32 {
        self.complete + self.partial + self.unavailable
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundsFact {
    pub rounds: u32,
    pub threshold: u32,
}

/// The criteria of the approved Specification and how units took them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CriteriaFact {
    pub total: usize,
    /// Criteria some Code or Test unit of the current revision was assigned.
    pub assigned: usize,
    /// Criteria some of those units validated.
    pub validated: usize,
    /// Criteria the Specification verifies by an executed check without
    /// naming one.
    pub without_check: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskFact {
    pub level: Risk,
    /// Host-written, value-free reasons.
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CoverageRowFact {
    pub kind: CoverageRowKind,
    pub subject: String,
    pub state: CoverageState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriterionVerdict {
    pub criterion: String,
    pub verdict: Verdict,
}

/// Everything [`derive_layers`] reads, gathered by the host from durable rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayerInputs {
    /// Pipeline v4.
    pub composed: bool,
    /// The pulsation is past its Specification, so a v4 pulsation should hold
    /// a mandate.
    pub past_specification: bool,
    pub mandate: Option<MandateFact>,
    /// The stored mandate does not match its digest.
    pub mandate_integrity_failed: bool,
    pub runs: Vec<RunFact>,
    pub approvals: Vec<ApprovalFact>,
    /// Granted paths that match a sensitive glob (ADR-034).
    pub sensitive_granted: Vec<String>,
    pub publications: Vec<PublicationFact>,
    pub refusals: Vec<RefusalFact>,
    /// Unit results the host corrected for writing outside their destinations.
    pub ownership_violations: u32,
    /// Executor kind and tool policy of each executor that ran.
    pub tool_policies: Vec<(String, String)>,
    pub budget: Option<BudgetFact>,
    pub checks: Vec<HostCheckFact>,
    pub usage: UsageFact,
    pub redactions: u64,
    pub rounds: Option<RoundsFact>,
    /// `None` until the Specification of the current revision is approved.
    pub criteria: Option<CriteriaFact>,
    pub risk: Option<RiskFact>,
    pub coverage: Vec<CoverageRowFact>,
    pub verdicts: Vec<CriterionVerdict>,
    pub acknowledgements: u32,
    pub preflight_dismissals: u32,
}

// ------------------------------------------------------------- derivation

struct Builder {
    kind: LayerKind,
    state: LayerState,
    items: Vec<LayerItemV1>,
    truncated: usize,
    reasons: Vec<String>,
}

impl Builder {
    fn new(kind: LayerKind) -> Self {
        Self {
            kind,
            state: LayerState::Clear,
            items: Vec::new(),
            truncated: 0,
            reasons: Vec::new(),
        }
    }

    fn item(&mut self, label: impl Into<String>, value: impl Into<String>, state: LayerState) {
        if self.items.len() < MAX_LAYER_ITEMS {
            self.items.push(LayerItemV1 {
                label: label.into(),
                value: value.into(),
                state,
            });
        } else {
            self.truncated += 1;
        }
    }

    fn flag(&mut self, state: LayerState, reason: String) {
        self.state = self.state.worse(state);
        if !self.reasons.contains(&reason) {
            self.reasons.push(reason);
        }
    }

    fn finish(self, headline: String) -> LayerV1 {
        LayerV1 {
            kind: self.kind,
            state: self.state,
            headline,
            items: self.items,
            truncated_count: self.truncated,
            reasons: self.reasons,
        }
    }
}

fn fixed(kind: LayerKind, state: LayerState, headline: &str) -> LayerV1 {
    LayerV1 {
        kind,
        state,
        headline: headline.to_owned(),
        items: Vec::new(),
        truncated_count: 0,
        reasons: Vec::new(),
    }
}

fn sorted<T: Clone + Ord>(values: &[T]) -> Vec<T> {
    let mut values = values.to_vec();
    values.sort();
    values
}

const fn risk_word(level: Risk) -> &'static str {
    match level {
        Risk::Low => "low",
        Risk::Medium => "medium",
        Risk::High => "high",
        Risk::Unknown => "unknown",
    }
}

const fn check_word(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Pending => "pending",
        CheckStatus::Running => "running",
        CheckStatus::Passed => "passed",
        CheckStatus::Failed => "failed",
        CheckStatus::TimedOut => "timed out",
        CheckStatus::Cancelled => "cancelled",
        CheckStatus::Unavailable => "unavailable",
        CheckStatus::NotRun => "not run",
        CheckStatus::Error => "runner error",
    }
}

fn cause_word(cause: MandateCause) -> String {
    cause.as_db_value().replace('_', " ")
}

/// Derives the four layers from the host's facts.
pub fn derive_layers(inputs: &LayerInputs) -> PulsationLayersSnapshot {
    PulsationLayersSnapshot {
        layers_policy_version: LAYERS_POLICY_VERSION,
        layers: vec![
            identity(inputs),
            scope(inputs),
            guardrails(inputs),
            intent(inputs),
        ],
    }
}

fn identity(inputs: &LayerInputs) -> LayerV1 {
    let kind = LayerKind::Identity;
    let mandate_expected = inputs.composed && inputs.past_specification;
    if inputs.runs.is_empty()
        && inputs.mandate.is_none()
        && inputs.approvals.is_empty()
        && !mandate_expected
    {
        return fixed(kind, LayerState::Unknown, "No agent has run yet.");
    }
    let mut layer = Builder::new(kind);
    let mut groups = BTreeMap::<RunFact, u32>::new();
    for run in &inputs.runs {
        *groups.entry(run.clone()).or_default() += 1;
    }
    let mut models = BTreeSet::new();
    for (run, count) in &groups {
        let observed = run.observed_model.as_deref();
        let requested = run.requested_model.as_deref();
        if let Some(model) = observed.or(requested) {
            models.insert(model.to_owned());
        }
        let verification = verify_model(run.provider.as_deref(), requested, observed);
        let state = if verification == ModelVerification::Mismatch {
            layer.flag(
                LayerState::Attention,
                format!(
                    "`{}` ran `{}`, not the requested `{}`.",
                    run.actor,
                    observed.unwrap_or_default(),
                    requested.unwrap_or_default()
                ),
            );
            LayerState::Attention
        } else if run.completed && observed.is_none() {
            layer.flag(
                LayerState::Attention,
                format!("`{}` did not report the model it ran.", run.actor),
            );
            LayerState::Attention
        } else {
            LayerState::Clear
        };
        let target = match &run.provider {
            Some(provider) => format!("{} · {provider}", run.transport),
            None => run.transport.clone(),
        };
        let adapter = run
            .adapter_version
            .map(|version| format!(" · adapter v{version}"))
            .unwrap_or_default();
        layer.item(
            run.actor.clone(),
            format!(
                "{target} · requested {} · observed {}{adapter} · {count} run(s)",
                requested.unwrap_or("default"),
                observed.unwrap_or("unknown"),
            ),
            state,
        );
    }
    let mandate = if !inputs.composed {
        layer.item(
            "Mandate",
            "not applied (legacy pipeline)",
            LayerState::Clear,
        );
        "legacy pipeline".to_owned()
    } else {
        match &inputs.mandate {
            Some(mandate) => {
                layer.item(
                    "Mandate",
                    format!(
                        "revision {} ({})",
                        mandate.revision,
                        cause_word(mandate.cause)
                    ),
                    LayerState::Clear,
                );
                if mandate.transport_fallback {
                    layer.flag(
                        LayerState::Attention,
                        "An approved CLI fallback moved a role to the API.".into(),
                    );
                }
                format!("mandate revision {}", mandate.revision)
            }
            None if mandate_expected => {
                layer.item("Mandate", "not issued", LayerState::Attention);
                layer.flag(
                    LayerState::Attention,
                    "No mandate was issued: the pulsation started before this version.".into(),
                );
                "no mandate".to_owned()
            }
            None => {
                layer.item(
                    "Mandate",
                    "issued when the Specification is approved",
                    LayerState::Clear,
                );
                "no mandate yet".to_owned()
            }
        }
    };
    for approval in sorted(&inputs.approvals) {
        layer.item(
            format!("{} {}", approval.stage, approval.purpose.replace('_', " ")),
            format!(
                "{} · {}",
                approval.decision.replace('_', " "),
                approval.decided_at
            ),
            LayerState::Clear,
        );
    }
    let runs = inputs.runs.len();
    let headline = if runs == 0 {
        format!("No agent has run yet · {mandate}")
    } else {
        format!(
            "{runs} run(s) on {} model(s) · {mandate}",
            models.len().max(1)
        )
    };
    layer.finish(headline)
}

fn scope(inputs: &LayerInputs) -> LayerV1 {
    let kind = LayerKind::Scope;
    if !inputs.composed {
        return fixed(
            kind,
            LayerState::NotApplicable,
            "Not enforced (legacy pipeline).",
        );
    }
    let mut layer = Builder::new(kind);
    if inputs.mandate_integrity_failed {
        layer.item("Mandate", "digest mismatch", LayerState::Issue);
        layer.flag(
            LayerState::Issue,
            "The stored mandate does not match its digest.".into(),
        );
        return layer.finish("Mandate integrity check failed".into());
    }
    let Some(mandate) = &inputs.mandate else {
        return if inputs.past_specification {
            fixed(
                kind,
                LayerState::NotApplicable,
                "Mandate not issued (started before this version).",
            )
        } else {
            fixed(
                kind,
                LayerState::Unknown,
                "No write authority yet: approving the Specification issues the mandate.",
            )
        };
    };
    layer.item(
        "Envelope",
        format!("{} path(s) granted", mandate.granted),
        LayerState::Clear,
    );
    let sensitive = sorted(&inputs.sensitive_granted);
    for path in &sensitive {
        layer.item(path.clone(), "granted · sensitive", LayerState::Attention);
    }
    if !sensitive.is_empty() {
        layer.flag(
            LayerState::Attention,
            format!(
                "{} sensitive path(s) in the write envelope.",
                sensitive.len()
            ),
        );
    }
    let publications = sorted(&inputs.publications);
    let mut written = 0;
    let mut conflicts = 0;
    let mut sensitive_written = 0;
    for publication in &publications {
        let state = if publication.state == "conflict" {
            conflicts += 1;
            LayerState::Issue
        } else if publication.sensitive {
            LayerState::Attention
        } else {
            LayerState::Clear
        };
        if publication.state == "applied" {
            written += 1;
            if publication.sensitive {
                sensitive_written += 1;
            }
        }
        let suffix = if publication.sensitive {
            " · sensitive"
        } else {
            ""
        };
        layer.item(
            publication.path.clone(),
            format!("{}{suffix}", publication.state),
            state,
        );
    }
    if sensitive_written > 0 {
        layer.flag(
            LayerState::Attention,
            format!("{sensitive_written} sensitive path(s) written."),
        );
    }
    if conflicts > 0 {
        layer.flag(
            LayerState::Issue,
            format!(
                "{conflicts} publication(s) in conflict: the workspace kept the developer's version."
            ),
        );
    }
    let refusals = sorted(&inputs.refusals);
    for refusal in &refusals {
        let (label, word) = match refusal.rule.as_str() {
            "capability_mismatch" => ("Dispatch".to_owned(), "dispatch refused"),
            _ => (
                refusal.path.clone().unwrap_or_else(|| "Write".into()),
                "write refused",
            ),
        };
        layer.item(
            label,
            format!("{word} · {} time(s)", refusal.occurrences),
            LayerState::Issue,
        );
    }
    if !refusals.is_empty() {
        layer.flag(
            LayerState::Issue,
            format!(
                "The mandate refused {} write(s) or dispatch(es).",
                refusals.len()
            ),
        );
    }
    if inputs.ownership_violations > 0 {
        layer.item(
            "Ownership",
            format!("{} correction(s)", inputs.ownership_violations),
            LayerState::Attention,
        );
        layer.flag(
            LayerState::Attention,
            format!(
                "{} unit result(s) wrote outside their own destinations.",
                inputs.ownership_violations
            ),
        );
    }
    layer.finish(format!(
        "{} granted · {written} written · {} refused",
        mandate.granted,
        refusals.len()
    ))
}

fn guardrails(inputs: &LayerInputs) -> LayerV1 {
    let kind = LayerKind::Guardrails;
    if inputs.runs.is_empty()
        && inputs.budget.is_none()
        && inputs.checks.is_empty()
        && inputs.usage.total() == 0
        && inputs.redactions == 0
    {
        return fixed(kind, LayerState::Unknown, "Nothing has run yet.");
    }
    let mut layer = Builder::new(kind);
    for (executor, policy) in sorted(&inputs.tool_policies) {
        layer.item(executor, policy.replace('_', " "), LayerState::Clear);
    }
    let mut lowest_extra = None::<u64>;
    if let Some(budget) = &inputs.budget {
        for pool in sorted(&budget.extra_pools) {
            if pool.limit == 0 {
                continue;
            }
            let percent = pool.left.saturating_mul(100) / pool.limit;
            lowest_extra = Some(lowest_extra.map_or(percent, |lowest| lowest.min(percent)));
            let low = budget.active
                && pool.left.saturating_mul(100)
                    < pool.limit.saturating_mul(LOW_EXTRA_BUDGET_PERCENT);
            if low {
                layer.flag(
                    LayerState::Attention,
                    format!(
                        "Less than {LOW_EXTRA_BUDGET_PERCENT}% of the {} extra {} is left.",
                        budget.stage, pool.name
                    ),
                );
            }
            layer.item(
                format!("{} extra {}", budget.stage, pool.name),
                format!("{} of {} left", pool.left, pool.limit),
                if low {
                    LayerState::Attention
                } else {
                    LayerState::Clear
                },
            );
        }
        if budget.exhausted && budget.unit_waiting {
            layer.item(
                format!("{} budget", budget.stage),
                "exhausted",
                LayerState::Issue,
            );
            layer.flag(
                LayerState::Issue,
                format!(
                    "The {} budget is exhausted with a unit still waiting.",
                    budget.stage
                ),
            );
        }
    }
    let mut checks = inputs.checks.clone();
    checks.sort_by(|left, right| left.check_ref.cmp(&right.check_ref));
    let mut required = 0;
    let mut passed = 0;
    for check in &checks {
        let state = if !check.required {
            LayerState::Clear
        } else {
            required += 1;
            match check.status {
                CheckStatus::Passed => {
                    passed += 1;
                    LayerState::Clear
                }
                CheckStatus::Failed | CheckStatus::TimedOut => {
                    layer.flag(
                        LayerState::Issue,
                        format!(
                            "Required check `{}` {}.",
                            check.check_ref,
                            check_word(check.status)
                        ),
                    );
                    LayerState::Issue
                }
                CheckStatus::Unavailable
                | CheckStatus::NotRun
                | CheckStatus::Cancelled
                | CheckStatus::Error => {
                    layer.flag(
                        LayerState::Attention,
                        format!(
                            "Required check `{}` did not run to a result ({}).",
                            check.check_ref,
                            check_word(check.status)
                        ),
                    );
                    LayerState::Attention
                }
                CheckStatus::Pending | CheckStatus::Running => LayerState::Clear,
            }
        };
        layer.item(
            check.check_ref.clone(),
            format!(
                "{} · {}",
                if check.required {
                    "required"
                } else {
                    "optional"
                },
                check_word(check.status)
            ),
            state,
        );
    }
    let usage = inputs.usage;
    if usage.total() > 0 {
        layer.item(
            "Usage",
            format!(
                "{} complete · {} partial · {} unavailable",
                usage.complete, usage.partial, usage.unavailable
            ),
            if usage.partial > 0 {
                LayerState::Attention
            } else {
                LayerState::Clear
            },
        );
        if usage.partial > 0 {
            layer.flag(
                LayerState::Attention,
                format!("{} usage record(s) are partial.", usage.partial),
            );
        }
    }
    layer.item(
        "Sanitization",
        format!("{} redaction(s)", inputs.redactions),
        LayerState::Clear,
    );
    if let Some(rounds) = inputs.rounds {
        let near = rounds.rounds.saturating_add(1) >= rounds.threshold;
        if near {
            layer.flag(
                LayerState::Attention,
                if rounds.rounds >= rounds.threshold {
                    format!(
                        "Revision rounds are beyond the threshold ({} of {}).",
                        rounds.rounds, rounds.threshold
                    )
                } else {
                    format!(
                        "Revision rounds at {} of {}: the next revision needs a decision.",
                        rounds.rounds, rounds.threshold
                    )
                },
            );
        }
        layer.item(
            "Revision rounds",
            format!("{} of {}", rounds.rounds, rounds.threshold),
            if near {
                LayerState::Attention
            } else {
                LayerState::Clear
            },
        );
    }
    let mut headline = Vec::new();
    if required > 0 {
        headline.push(format!("{passed}/{required} required check(s) passed"));
    } else if !checks.is_empty() || inputs.composed {
        headline.push("no required check".to_owned());
    }
    if let Some(percent) = lowest_extra {
        headline.push(format!("{percent}% extra budget left"));
    }
    if headline.is_empty() {
        headline.push(format!(
            "{} usage record(s) · {} redaction(s)",
            usage.total(),
            inputs.redactions
        ));
    }
    layer.finish(headline.join(" · "))
}

fn intent(inputs: &LayerInputs) -> LayerV1 {
    let kind = LayerKind::Intent;
    if !inputs.composed {
        return fixed(
            kind,
            LayerState::NotApplicable,
            "Criteria states unavailable (legacy pipeline).",
        );
    }
    let Some(criteria) = &inputs.criteria else {
        return fixed(kind, LayerState::Unknown, "No approved criteria yet.");
    };
    let mut layer = Builder::new(kind);
    let total = criteria.total;
    layer.item("Criteria", total.to_string(), LayerState::Clear);
    layer.item(
        "Assigned to a unit",
        format!("{} of {total}", criteria.assigned),
        LayerState::Clear,
    );
    layer.item(
        "With a validated unit",
        format!("{} of {total}", criteria.validated),
        LayerState::Clear,
    );
    let coverage = sorted(&inputs.coverage);
    let fresh = coverage
        .iter()
        .filter(|row| {
            row.kind == CoverageRowKind::Criterion && row.state == CoverageState::Executed
        })
        .count();
    if !coverage.is_empty() {
        layer.item(
            "Backed by a fresh host receipt",
            format!("{fresh} of {total}"),
            LayerState::Clear,
        );
    }
    let count = |verdict: Verdict| {
        inputs
            .verdicts
            .iter()
            .filter(|fact| fact.verdict == verdict)
            .count()
    };
    let supported = count(Verdict::Supported);
    let contradicted = count(Verdict::Contradicted);
    if !inputs.verdicts.is_empty() {
        layer.item(
            "Integrator verdicts",
            format!(
                "{supported} supported · {} unverified · {contradicted} contradicted",
                count(Verdict::Unverified)
            ),
            if contradicted > 0 {
                LayerState::Issue
            } else {
                LayerState::Clear
            },
        );
    }
    let mut against = inputs
        .verdicts
        .iter()
        .filter(|fact| fact.verdict == Verdict::Contradicted)
        .map(|fact| fact.criterion.clone())
        .collect::<Vec<_>>();
    against.sort();
    against.dedup();
    for criterion in &against {
        layer.item(criterion.clone(), "contradicted", LayerState::Issue);
    }
    if !against.is_empty() {
        layer.flag(
            LayerState::Issue,
            format!(
                "The Integrator found {} criterion(s) contradicted.",
                against.len()
            ),
        );
    }
    if let Some(risk) = &inputs.risk {
        let flagged = matches!(risk.level, Risk::High | Risk::Unknown);
        layer.item(
            "Risk",
            risk_word(risk.level),
            if flagged {
                LayerState::Attention
            } else {
                LayerState::Clear
            },
        );
        if flagged {
            layer.flag(
                LayerState::Attention,
                format!("Risk is {}.", risk_word(risk.level)),
            );
        }
        for reason in sorted(&risk.reasons) {
            layer.item("Risk reason", reason, LayerState::Clear);
        }
    }
    let weak = coverage
        .iter()
        .filter(|row| {
            matches!(
                row.state,
                CoverageState::Unverified | CoverageState::ModelOnly
            )
        })
        .collect::<Vec<_>>();
    for row in &weak {
        let kind_word = match row.kind {
            CoverageRowKind::Criterion => "criterion",
            CoverageRowKind::Check => "check",
        };
        layer.item(
            format!("{kind_word} {}", row.subject),
            row.state.as_str().replace('_', " "),
            LayerState::Attention,
        );
    }
    if !weak.is_empty() {
        layer.flag(
            LayerState::Attention,
            format!(
                "{} item(s) are unverified or backed by the model only.",
                weak.len()
            ),
        );
    }
    let without = sorted(&criteria.without_check);
    for criterion in &without {
        layer.item(
            criterion.clone(),
            "executed check expected, none named",
            LayerState::Attention,
        );
    }
    if !without.is_empty() {
        layer.flag(
            LayerState::Attention,
            format!(
                "{} criterion(s) expect an executed check the Specification does not name.",
                without.len()
            ),
        );
    }
    if inputs.acknowledgements > 0 {
        layer.item(
            "Acknowledgements",
            inputs.acknowledgements.to_string(),
            LayerState::Clear,
        );
    }
    if inputs.preflight_dismissals > 0 {
        layer.item(
            "Pre-flight dismissals",
            inputs.preflight_dismissals.to_string(),
            LayerState::Clear,
        );
    }
    let mut headline = vec![
        format!("{total} criteria"),
        format!("{} with a validated unit", criteria.validated),
    ];
    if let Some(risk) = &inputs.risk {
        headline.push(format!("risk {}", risk_word(risk.level)));
    }
    if !inputs.verdicts.is_empty() {
        headline.push(format!("{supported} supported"));
    }
    layer.finish(headline.join(" · "))
}

#[cfg(test)]
#[path = "layers_tests.rs"]
mod tests;
