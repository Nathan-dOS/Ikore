//! Shared stage budget arithmetic (SC-013).
//!
//! `B` is the stage's effective output-token limit and `P` the admission
//! character budget for complete model-facing requests. A stage plan reserves
//! from an initial pool sized by its planned units and an extra pool that
//! covers at most two real additional invocations of any kind (transport
//! retry, availability fallback or semantic correction), with at most one per
//! unit. Reservations are durable and monotonic: unused output is never
//! reclaimed to manufacture another call, and Retry never resets consumption.
//! Everything here is pure; persistence applies it inside one transaction.

use serde::{Deserialize, Serialize};

use super::{
    CompositionError, ErrorCode, Handle, Invariant, MAX_UNITS_PER_PLAN, RemainingBudgetV1,
    StageBudgetV1,
};

pub const MIN_PROMPT_CHARACTER_LIMIT: u32 = 64_000;
pub const EXTRA_INVOCATION_LIMIT: u32 = 2;
pub const PER_UNIT_EXTRA_INVOCATION_LIMIT: u32 = 1;

/// `P = max(64000, 4 * context_chars_limit)`, in Unicode scalar values.
pub fn prompt_character_limit(context_chars_limit: usize) -> u32 {
    let scaled = context_chars_limit.saturating_mul(4).min(u32::MAX as usize) as u32;
    scaled.max(MIN_PROMPT_CHARACTER_LIMIT)
}

/// A planned unit's expected output need, used to split `B` across units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitEstimate {
    pub logical_unit_key: Handle,
    /// Tokens the unit must be able to emit for its complete output. The host
    /// derives it from the owned regions and payload shape, never from prose.
    pub estimated_output_tokens: u32,
}

/// Derives the stage budget and each unit's `maxOutputTokens` reservation.
/// Infeasible plans are rejected before any request exists.
pub fn derive_budget(
    stage_output_limit: u32,
    context_chars_limit: usize,
    units: &[UnitEstimate],
    max_in_flight_units: u32,
) -> Result<(StageBudgetV1, Vec<u32>), CompositionError> {
    if units.len() > MAX_UNITS_PER_PLAN {
        return Err(CompositionError::new(
            Invariant::Sc006,
            ErrorCode::CapacityExceeded,
            format!(
                "A stage plan admits at most {MAX_UNITS_PER_PLAN} units; got {}.",
                units.len()
            ),
        ));
    }
    let b = stage_output_limit.max(1);
    let p = prompt_character_limit(context_chars_limit);
    let mut reservations = Vec::with_capacity(units.len());
    let total_estimate: u64 = units
        .iter()
        .map(|unit| u64::from(unit.estimated_output_tokens.max(1)))
        .sum();
    for unit in units {
        let estimate = u64::from(unit.estimated_output_tokens.max(1));
        if estimate > u64::from(b) {
            return Err(CompositionError::new(
                Invariant::Sc006,
                ErrorCode::CapacityExceeded,
                format!(
                    "Unit `{}` needs {estimate} output tokens but the stage limit is {b}.",
                    unit.logical_unit_key
                ),
            )
            .with_related([unit.logical_unit_key.clone()]));
        }
        // A single unit takes the whole stage limit; several units share it
        // proportionally to their need, never below their own estimate.
        let share = if units.len() == 1 {
            u64::from(b)
        } else {
            (u64::from(b) * estimate / total_estimate.max(1)).max(estimate)
        };
        reservations.push(share.min(u64::from(b)) as u32);
    }
    let sum: u64 = reservations.iter().map(|value| u64::from(*value)).sum();
    if sum > u64::from(b) {
        return Err(CompositionError::new(
            Invariant::Sc006,
            ErrorCode::CapacityExceeded,
            format!("Initial output reservations ({sum}) exceed the stage limit ({b})."),
        ));
    }
    let budget = StageBudgetV1 {
        initial_invocation_limit: units.len() as u32,
        extra_invocation_limit: if units.is_empty() {
            0
        } else {
            EXTRA_INVOCATION_LIMIT
        },
        per_unit_extra_invocation_limit: PER_UNIT_EXTRA_INVOCATION_LIMIT,
        initial_output_token_limit: b,
        extra_output_token_limit: if units.is_empty() { 0 } else { b },
        initial_prompt_character_limit: p,
        extra_prompt_character_limit: if units.is_empty() { 0 } else { p },
        max_in_flight_units: max_in_flight_units.clamp(1, 2),
    };
    Ok((budget, reservations))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pool {
    Initial,
    Extra,
}

impl Pool {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Extra => "extra",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "initial" => Some(Self::Initial),
            "extra" => Some(Self::Extra),
            _ => None,
        }
    }
}

/// The durable counters of a budget account: limits plus what is currently
/// held (reserved, started or settled; released reservations are not held).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccountLedger {
    pub limits: Option<StageBudgetV1>,
    pub held_initial_invocations: u32,
    pub held_extra_invocations: u32,
    pub held_initial_output_tokens: u32,
    pub held_extra_output_tokens: u32,
    pub held_initial_prompt_chars: u32,
    pub held_extra_prompt_chars: u32,
}

impl AccountLedger {
    pub fn remaining(&self) -> RemainingBudgetV1 {
        let limits = self.limits.clone().unwrap_or(StageBudgetV1 {
            initial_invocation_limit: 0,
            extra_invocation_limit: 0,
            per_unit_extra_invocation_limit: 1,
            initial_output_token_limit: 1,
            extra_output_token_limit: 0,
            initial_prompt_character_limit: 1,
            extra_prompt_character_limit: 0,
            max_in_flight_units: 1,
        });
        RemainingBudgetV1 {
            initial_invocations: limits
                .initial_invocation_limit
                .saturating_sub(self.held_initial_invocations),
            extra_invocations: limits
                .extra_invocation_limit
                .saturating_sub(self.held_extra_invocations),
            initial_output_tokens: limits
                .initial_output_token_limit
                .saturating_sub(self.held_initial_output_tokens),
            extra_output_tokens: limits
                .extra_output_token_limit
                .saturating_sub(self.held_extra_output_tokens),
            initial_prompt_characters: limits
                .initial_prompt_character_limit
                .saturating_sub(self.held_initial_prompt_chars),
            extra_prompt_characters: limits
                .extra_prompt_character_limit
                .saturating_sub(self.held_extra_prompt_chars),
        }
    }

    /// Whether a reservation of `output_tokens`/`prompt_chars` fits `pool`.
    /// `unit_extra_used` is the number of extra invocations this logical unit
    /// already holds across generations.
    pub fn can_reserve(
        &self,
        pool: Pool,
        output_tokens: u32,
        prompt_chars: u32,
        unit_extra_used: u32,
    ) -> Result<(), CompositionError> {
        let remaining = self.remaining();
        let exhausted = |what: &str| {
            Err(CompositionError::new(
                Invariant::Sc013,
                ErrorCode::BudgetExhausted,
                format!("The shared stage budget has no {what} left for this invocation."),
            ))
        };
        match pool {
            Pool::Initial => {
                if remaining.initial_invocations == 0 {
                    return exhausted("initial invocation");
                }
                if remaining.initial_output_tokens < output_tokens {
                    return exhausted("initial output allowance");
                }
                if remaining.initial_prompt_characters < prompt_chars {
                    return exhausted("initial prompt character allowance");
                }
            }
            Pool::Extra => {
                if unit_extra_used >= PER_UNIT_EXTRA_INVOCATION_LIMIT {
                    return Err(CompositionError::new(
                        Invariant::Sc013,
                        ErrorCode::BudgetExhausted,
                        "This unit already used its single extra invocation (repair, retry or fallback).",
                    ));
                }
                if remaining.extra_invocations == 0 {
                    return exhausted("extra invocation");
                }
                if remaining.extra_output_tokens < output_tokens {
                    return exhausted("extra output allowance");
                }
                if remaining.extra_prompt_characters < prompt_chars {
                    return exhausted("extra prompt character allowance");
                }
            }
        }
        Ok(())
    }

    /// Applies a reservation after `can_reserve` succeeded.
    pub fn hold(&mut self, pool: Pool, output_tokens: u32, prompt_chars: u32) {
        match pool {
            Pool::Initial => {
                self.held_initial_invocations += 1;
                self.held_initial_output_tokens = self
                    .held_initial_output_tokens
                    .saturating_add(output_tokens);
                self.held_initial_prompt_chars =
                    self.held_initial_prompt_chars.saturating_add(prompt_chars);
            }
            Pool::Extra => {
                self.held_extra_invocations += 1;
                self.held_extra_output_tokens =
                    self.held_extra_output_tokens.saturating_add(output_tokens);
                self.held_extra_prompt_chars =
                    self.held_extra_prompt_chars.saturating_add(prompt_chars);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn estimate(key: &str, tokens: u32) -> UnitEstimate {
        UnitEstimate {
            logical_unit_key: Handle::new(key).unwrap(),
            estimated_output_tokens: tokens,
        }
    }

    #[test]
    fn a_single_unit_receives_the_stage_limit_and_two_extra_calls() {
        let (budget, reservations) =
            derive_budget(16_000, 16_000, &[estimate("only", 3_000)], 1).unwrap();
        assert_eq!(reservations, vec![16_000]);
        assert_eq!(budget.initial_invocation_limit, 1);
        assert_eq!(budget.extra_invocation_limit, 2);
        assert_eq!(budget.per_unit_extra_invocation_limit, 1);
        assert_eq!(budget.initial_output_token_limit, 16_000);
        assert_eq!(budget.extra_output_token_limit, 16_000);
        assert_eq!(budget.initial_prompt_character_limit, 64_000);
        assert_eq!(prompt_character_limit(20_000), 80_000);
        assert_eq!(budget.max_in_flight_units, 1);
        let (empty, none) = derive_budget(8_000, 16_000, &[], 2).unwrap();
        assert!(none.is_empty());
        assert_eq!(empty.initial_invocation_limit, 0);
        assert_eq!(empty.extra_invocation_limit, 0);
        assert_eq!(empty.max_in_flight_units, 2);
    }

    #[test]
    fn several_units_share_the_limit_and_infeasible_plans_are_rejected() {
        let (_, reservations) = derive_budget(
            10_000,
            16_000,
            &[estimate("a", 1_000), estimate("b", 3_000)],
            1,
        )
        .unwrap();
        assert!(reservations.iter().sum::<u32>() <= 10_000);
        assert!(reservations[0] >= 1_000 && reservations[1] >= 3_000);
        assert!(derive_budget(2_000, 16_000, &[estimate("a", 2_500)], 1).is_err());
        let seventeen = (0..17)
            .map(|i| estimate(&format!("u{i}"), 10))
            .collect::<Vec<_>>();
        assert!(derive_budget(10_000, 16_000, &seventeen, 1).is_err());
    }

    #[test]
    fn the_ledger_enforces_pools_and_the_per_unit_extra_limit() {
        let (budget, _) = derive_budget(1_000, 16_000, &[estimate("only", 500)], 1).unwrap();
        let mut ledger = AccountLedger {
            limits: Some(budget),
            ..Default::default()
        };
        ledger.can_reserve(Pool::Initial, 1_000, 60_000, 0).unwrap();
        ledger.hold(Pool::Initial, 1_000, 60_000);
        assert_eq!(ledger.remaining().initial_invocations, 0);
        assert!(ledger.can_reserve(Pool::Initial, 1, 1, 0).is_err());
        ledger.can_reserve(Pool::Extra, 1_000, 64_000, 0).unwrap();
        ledger.hold(Pool::Extra, 1_000, 64_000);
        // Second extra call is refused for the same unit even though the stage pool has one left.
        assert_eq!(ledger.remaining().extra_invocations, 1);
        let error = ledger.can_reserve(Pool::Extra, 1, 1, 1).unwrap_err();
        assert_eq!(error.code, ErrorCode::BudgetExhausted);
        // Another unit could use the stage's last extra call, but not more output than remains.
        assert!(ledger.can_reserve(Pool::Extra, 1, 1, 0).is_err());
        assert_eq!(ledger.remaining().extra_output_tokens, 0);
    }
}
