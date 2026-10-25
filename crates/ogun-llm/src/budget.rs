//! Prices, worst-case costs and the per-attempt call budget. Integer micro-US-dollars throughout: no floating
//! point anywhere money is computed.

use crate::config::ProviderConfig;
use crate::error::LlmError;

/// Per-request overhead added to the input bound (message framing, role markers, schema wrapper).
pub const REQUEST_OVERHEAD_TOKENS: u64 = 256;

/// Cost of a call, rounded up to the next micro-dollar. Saturates at `u64::MAX` instead of wrapping.
#[must_use]
pub fn cost_usd_micro(
    input_tokens: u64,
    output_tokens: u64,
    input_per_mtok_usd_micro: u64,
    output_per_mtok_usd_micro: u64,
) -> u64 {
    let total = (u128::from(input_tokens) * u128::from(input_per_mtok_usd_micro))
        .saturating_add(u128::from(output_tokens) * u128::from(output_per_mtok_usd_micro));
    u64::try_from(total.div_ceil(1_000_000)).unwrap_or(u64::MAX)
}

/// A bound on the input tokens of a prompt of `bytes` bytes. Every tokenizer in use maps at least one byte of
/// UTF-8 to each token, so bytes plus framing overhead is a true upper bound (about four times the usual
/// count for English text).
#[must_use]
pub fn input_token_bound(bytes: u64) -> u64 {
    bytes.saturating_add(REQUEST_OVERHEAD_TOKENS)
}

/// A typical-case token estimate, used only when a provider reports no counts (some embedding endpoints).
#[must_use]
pub fn estimate_tokens(bytes: u64) -> u64 {
    bytes.div_ceil(4)
}

/// The cost of a call with `prompt_bytes` of input and up to `max_output_tokens` of output on `provider`,
/// assuming the worst for both.
#[must_use]
pub fn worst_case_usd_micro(
    provider: &ProviderConfig,
    prompt_bytes: u64,
    max_output_tokens: u32,
) -> u64 {
    cost_usd_micro(
        input_token_bound(prompt_bytes),
        u64::from(max_output_tokens),
        provider.input_per_mtok_usd_micro,
        provider.output_per_mtok_usd_micro,
    )
}

/// The budget check: refuses a call whose worst-case cost would exceed what remains.
pub fn check_budget(worst_case_usd_micro: u64, remaining_usd_micro: u64) -> Result<(), LlmError> {
    if worst_case_usd_micro > remaining_usd_micro {
        Err(LlmError::BudgetExceeded {
            needed_usd_micro: worst_case_usd_micro,
            remaining_usd_micro,
        })
    } else {
        Ok(())
    }
}

/// What one operator attempt may still spend and how many calls it may still make. The daemon reserves the
/// attempt's budget before starting the operator (O-04 section 5.2) and commits the sum of the `usage` lines
/// afterwards; this keeps the operator inside the reservation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallBudget {
    limit_usd_micro: u64,
    spent_usd_micro: u64,
    max_calls: u64,
    calls: u64,
}

impl CallBudget {
    /// A budget of `limit_usd_micro` and at most `max_calls` provider requests.
    #[must_use]
    pub fn new(limit_usd_micro: u64, max_calls: u64) -> Self {
        Self {
            limit_usd_micro,
            spent_usd_micro: 0,
            max_calls,
            calls: 0,
        }
    }

    /// Money left.
    #[must_use]
    pub fn remaining_usd_micro(&self) -> u64 {
        self.limit_usd_micro.saturating_sub(self.spent_usd_micro)
    }

    /// Money spent so far.
    #[must_use]
    pub fn spent_usd_micro(&self) -> u64 {
        self.spent_usd_micro
    }

    /// Provider requests made so far.
    #[must_use]
    pub fn calls(&self) -> u64 {
        self.calls
    }

    /// Requests left.
    #[must_use]
    pub fn calls_left(&self) -> u64 {
        self.max_calls.saturating_sub(self.calls)
    }

    /// The configured call limit.
    #[must_use]
    pub fn max_calls(&self) -> u64 {
        self.max_calls
    }

    /// Admits one provider request with the given worst-case cost, or refuses it. Admission counts the call.
    pub fn admit(&mut self, worst_case_usd_micro: u64) -> Result<(), LlmError> {
        if self.calls >= self.max_calls {
            return Err(LlmError::CallLimit {
                max_calls: self.max_calls,
            });
        }
        check_budget(worst_case_usd_micro, self.remaining_usd_micro())?;
        self.calls += 1;
        Ok(())
    }

    /// Records the actual cost of an admitted call. An actual cost above the worst case (a provider counting
    /// differently) is recorded in full and limits later calls.
    pub fn record(&mut self, cost_usd_micro: u64) {
        self.spent_usd_micro = self.spent_usd_micro.saturating_add(cost_usd_micro);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_rounds_up_and_never_wraps() {
        assert_eq!(cost_usd_micro(1, 0, 1, 0), 1);
        assert_eq!(
            cost_usd_micro(1_000_000, 1_000_000, 2_000_000, 10_000_000),
            12_000_000
        );
        assert_eq!(cost_usd_micro(0, 0, 5, 5), 0);
        assert_eq!(
            cost_usd_micro(u64::MAX, u64::MAX, u64::MAX, u64::MAX),
            u64::MAX
        );
    }

    #[test]
    fn budget_refuses_worst_case_over_remaining() {
        let mut b = CallBudget::new(100, 3);
        b.admit(60).unwrap();
        b.record(60);
        assert!(matches!(
            b.admit(41),
            Err(LlmError::BudgetExceeded {
                needed_usd_micro: 41,
                remaining_usd_micro: 40
            })
        ));
        b.admit(40).unwrap();
        b.admit(0).unwrap();
        assert!(matches!(
            b.admit(0),
            Err(LlmError::CallLimit { max_calls: 3 })
        ));
    }
}
