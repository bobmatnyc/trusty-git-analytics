//! Jev pricing and the per-run spend cap (#111).
//!
//! Why: a live run bills per input token and the owner caps each run's
//! spend. Split out of `jev.rs` to keep it under the size cap.
//! What: the published prices and [`JevBudget`], which reserves the worst
//! case before a call and settles to the reported usage after.
//! Test: `classify::tiers::jev_tests::budget_guard_stops_calls_at_the_cap`,
//! `jev_tests::concurrent_calls_never_pass_the_cap`,
//! `jev_tests::usage_above_the_reservation_exhausts_the_budget`,
//! `jev_tests::reported_cost_charges_output_at_zero`,
//! `jev_tests::output_tokens_never_count_against_the_cap`,
//! `jev_tests::pricing_and_default_cap`.

use std::sync::Mutex;

use tracing::warn;

use crate::classify::tiers::llm_prompt::LlmUsage;

/// Jev input price, US dollars per million tokens
/// (<https://docs.typesafe.ai/models>, read 2026-09-25).
pub const JEV_INPUT_PRICE_PER_MTOK_USD: f64 = 0.042;
/// Jev output price: output tokens are free (same source).
pub const JEV_OUTPUT_PRICE_PER_MTOK_USD: f64 = 0.0;

/// Per-run spend cap, tracked in input tokens.
///
/// Why: see the module doc.
/// What: the cap is `budget_usd` at the input price; output tokens cost $0
/// and never count against it. A call reserves its worst-case input (see
/// [`JevBudget::reserve`]) before its first attempt and settles to the
/// reported input after; a reply without usage keeps its reservation as
/// spend. A reservation that would pass the cap, or a call whose reported
/// input exceeds its reservation, exhausts the budget: no later call in the
/// run starts, so the running total never passes the cap.
/// Test: see the module doc.
pub(crate) struct JevBudget {
    cap_tokens: u64,
    state: Mutex<BudgetState>,
}

#[derive(Default)]
struct BudgetState {
    /// Input tokens counted against the cap, including the reservations of
    /// calls that reported no usage.
    spent: u64,
    reserved: u64,
    /// Reported input tokens.
    input: u64,
    /// Reported output tokens (free).
    output: u64,
    exhausted: bool,
}

impl JevBudget {
    /// A cap of `cap_usd` at [`JEV_INPUT_PRICE_PER_MTOK_USD`]; a
    /// non-positive or non-finite cap allows no call.
    pub(crate) fn new(cap_usd: f64) -> Self {
        let tokens = cap_usd / JEV_INPUT_PRICE_PER_MTOK_USD * 1_000_000.0;
        let cap_tokens = if tokens.is_finite() && tokens > 0.0 {
            tokens.floor() as u64
        } else {
            0
        };
        Self {
            cap_tokens,
            state: Mutex::new(BudgetState::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BudgetState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Reserve `input_tokens`; `false` once the cap would be passed or the
    /// budget is exhausted.
    ///
    /// #111: the caller reserves its input bound × `MAX_ATTEMPTS`, the most
    /// one call can bill across its retries.
    pub(crate) fn reserve(&self, input_tokens: u64) -> bool {
        let mut s = self.lock();
        let total = s
            .spent
            .saturating_add(s.reserved)
            .saturating_add(input_tokens);
        if !s.exhausted && total <= self.cap_tokens {
            s.reserved += input_tokens;
            return true;
        }
        if !s.exhausted {
            s.exhausted = true;
            // #111: `spent_usd` is what the cap counted (reported input
            // plus the bound of unreported attempts); `cost_usd` is reported
            // usage only.
            warn!(
                spent_tokens = s.spent,
                spent_usd = cost(s.spent, 0),
                cost_usd = cost(s.input, s.output),
                cap_usd = self.cap_usd(),
                "Jev spend cap reached; remaining calls are skipped"
            );
        }
        false
    }

    /// Replace a reservation with what the call's attempts may have billed.
    ///
    /// #111: every attempt sent may be billed, a timed-out one included, so
    /// each attempt without a reported usage is charged its `per_attempt`
    /// bound: `attempts - 1` bounds plus the reported input, or `attempts`
    /// bounds when the reply reported none. A charge above the reservation
    /// means the bound was wrong; the budget is exhausted for the rest of
    /// the run.
    /// Test: `jev_review_tests::billed_retries_count_against_the_cap`.
    pub(crate) fn settle(
        &self,
        reservation: u64,
        per_attempt: u64,
        attempts: u32,
        usage: Option<LlmUsage>,
    ) {
        let mut s = self.lock();
        s.reserved = s.reserved.saturating_sub(reservation);
        let attempts = u64::from(attempts.max(1));
        let billed = match usage {
            Some(u) => {
                s.input += u.input_tokens;
                s.output += u.output_tokens;
                u.input_tokens
                    .saturating_add(per_attempt.saturating_mul(attempts - 1))
            }
            None => per_attempt.saturating_mul(attempts),
        };
        s.spent = s.spent.saturating_add(billed);
        if billed > reservation && !s.exhausted {
            s.exhausted = true;
            warn!("Jev call billed more than its reservation; remaining calls are skipped");
        }
    }

    /// Reported cost so far, in US dollars: input at the input price,
    /// output at the (zero) output price.
    pub(crate) fn cost_usd(&self) -> f64 {
        let s = self.lock();
        cost(s.input, s.output)
    }

    /// What the cap counted so far, in US dollars: reported input plus the
    /// bound of every attempt that reported none (#111).
    /// Test: `jev_round3_tests::spend_log_shows_what_the_cap_counted`.
    pub(crate) fn spent_usd(&self) -> f64 {
        cost(self.lock().spent, 0)
    }

    /// Input tokens counted against the cap so far.
    #[cfg(test)]
    pub(crate) fn spent_tokens(&self) -> u64 {
        self.lock().spent
    }

    /// The cap in US dollars.
    pub(crate) fn cap_usd(&self) -> f64 {
        self.cap_tokens as f64 * JEV_INPUT_PRICE_PER_MTOK_USD / 1_000_000.0
    }
}

fn cost(input: u64, output: u64) -> f64 {
    (input as f64 * JEV_INPUT_PRICE_PER_MTOK_USD + output as f64 * JEV_OUTPUT_PRICE_PER_MTOK_USD)
        / 1_000_000.0
}
