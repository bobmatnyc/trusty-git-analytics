//! Bounded in-run retry for transient `git fetch` failures (#262).
//!
//! A collect over ~500 repositories makes ~500 sequential fetches. A rare
//! GitHub connect stall fails one repository, and the stale-data refusal then
//! fails the whole run. This module retries only that one fetch, a bounded
//! number of times, with exponential backoff — and only for errors that look
//! like a transient connect failure. Everything else stays a single attempt.
//!
//! A retry never turns a failure into a success: the last `Failed` outcome is
//! returned unchanged except for an attempt count appended to its text, so the
//! stale-data refusal in `tga collect` / `tga analyze` sees it exactly as
//! before.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing::warn;

use crate::collect::collector::FetchOutcome;

/// Default number of retries after the first attempt (3 attempts in total).
pub const DEFAULT_FETCH_RETRIES: u32 = 2;
/// Default backoff before the first retry, in seconds.
pub const DEFAULT_FETCH_RETRY_BASE_SECS: u64 = 2;
/// Upper bound on one backoff delay before jitter, in seconds.
pub const FETCH_RETRY_CAP_SECS: u64 = 30;
/// Consecutive exhausted repositories after which the run stops retrying.
pub const DEFAULT_EXHAUSTED_REPO_LIMIT: u32 = 10;

/// How often, and how patiently, one fetch is retried.
///
/// Why (#262): the policy is data so tests can run it with a zero base and a
/// recording sleep instead of real seconds.
/// What: `max_attempts` counts the first attempt; the delay before retry `k`
/// is `base * 2^(k-1)`, capped at `cap`, plus up to 25% jitter.
/// Test: `tests::sleeps_are_non_decreasing_and_bounded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts, including the first. `0` is treated as `1`.
    pub max_attempts: u32,
    /// Delay before the first retry.
    pub base: Duration,
    /// Ceiling on the delay before jitter.
    pub cap: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::from_config(None, None)
    }
}

impl RetryPolicy {
    /// Build a policy from the optional `fetch_retries` / `fetch_retry_base_secs`
    /// repository keys, applying the defaults for any key left unset.
    pub fn from_config(retries: Option<u32>, base_secs: Option<u64>) -> Self {
        Self {
            max_attempts: retries.unwrap_or(DEFAULT_FETCH_RETRIES).saturating_add(1),
            base: Duration::from_secs(base_secs.unwrap_or(DEFAULT_FETCH_RETRY_BASE_SECS)),
            cap: Duration::from_secs(FETCH_RETRY_CAP_SECS),
        }
    }

    /// The longest delay this policy can ever sleep: the cap plus 25% jitter.
    pub fn max_delay(&self) -> Duration {
        self.cap + self.cap / 4
    }

    /// Delay before retry number `retry` (1-based), with `jitter_frac` in
    /// `[0, 1)` scaling the up-to-25% jitter.
    fn delay(&self, retry: u32, jitter_frac: f64) -> Duration {
        let shift = retry.saturating_sub(1).min(31);
        let raw = self.base.saturating_mul(1u32 << shift).min(self.cap);
        raw + raw.mul_f64(0.25 * jitter_frac.clamp(0.0, 1.0))
    }
}

/// Per-run circuit breaker over exhausted retries.
///
/// Why (#262): when the network is down for good, retrying every remaining
/// repository multiplies a long run by the backoff. After `limit` consecutive
/// repositories exhaust their retries, the rest of the run uses one attempt.
/// What: counts consecutive exhausted repositories; any repository that does
/// not exhaust resets the count. Once tripped it stays tripped for the run.
/// Test: `tests::budget_stops_retries_after_consecutive_exhausted_repos`.
#[derive(Debug, Clone)]
pub struct RetryBudget {
    consecutive_exhausted: u32,
    limit: u32,
    tripped: bool,
}

impl Default for RetryBudget {
    fn default() -> Self {
        Self::new(DEFAULT_EXHAUSTED_REPO_LIMIT)
    }
}

impl RetryBudget {
    /// A fresh budget that trips after `limit` consecutive exhausted repos.
    pub fn new(limit: u32) -> Self {
        Self {
            consecutive_exhausted: 0,
            limit,
            tripped: false,
        }
    }

    /// `true` once the run has stopped retrying.
    pub fn is_tripped(&self) -> bool {
        self.tripped
    }

    fn record(&mut self, exhausted: bool) {
        if self.tripped {
            return;
        }
        if !exhausted {
            self.consecutive_exhausted = 0;
            return;
        }
        self.consecutive_exhausted += 1;
        if self.consecutive_exhausted >= self.limit {
            self.tripped = true;
            warn!(
                consecutive = self.consecutive_exhausted,
                "fetch retries exhausted on consecutive repositories; \
                 using single attempts for the rest of this run"
            );
        }
    }
}

/// `true` when a recorded fetch error is a transient connect failure.
///
/// Why (#262): only a connect stall is worth waiting for. Auth, credential,
/// certificate, not-found and unsupported-protocol errors fail the same way on
/// every attempt, so retrying them only delays the refusal.
/// What: reads git2's `Display` form (`<message>; class=<Class> (<n>); …`).
/// Retryable when the class is `Net`, `Http`, `Ssh` or `Os`, the message names
/// a connect timeout, a time-out, a refused or reset connection, or a failed
/// connect, and the message names none of the permanent markers. `Os` is in
/// the list because libgit2's socket stream reports a TCP connect failure as
/// `failed to connect to <host>: <strerror>; class=Os (2)` — every failure in
/// the #262 incident log has that shape.
/// Test: `tests::classifier_table`, `tests::real_connection_refused_is_retried_to_exhaustion`.
pub fn is_retryable_fetch_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    let class_ok = ["class=net ", "class=http ", "class=ssh ", "class=os "]
        .iter()
        .any(|c| lower.contains(c));
    if !class_ok {
        return false;
    }
    const PERMANENT: [&str; 8] = [
        "auth",
        "credential",
        "certificate",
        "not found",
        "404",
        "unsupported url protocol",
        "corrupt",
        "permission denied",
    ];
    if PERMANENT.iter().any(|p| lower.contains(p)) {
        return false;
    }
    const TRANSIENT: [&str; 5] = [
        "connect timeout",
        "timed out",
        "connection refused",
        "connection reset",
        "failed to connect",
    ];
    TRANSIENT.iter().any(|t| lower.contains(t))
}

/// Jitter fraction in `[0, 1)` from the wall clock's sub-second nanos.
fn clock_jitter() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    f64::from(nanos) / 1_000_000_000.0
}

/// Run `attempt` until it stops failing transiently, within `policy` and `budget`.
///
/// Why (#262): the seam keeps the loop testable — the real caller passes a
/// closure over `remote.fetch` and `std::thread::sleep`; tests pass scripted
/// outcomes and a recording sleep.
/// What: calls `attempt` up to `policy.max_attempts` times (once when the
/// budget is tripped), sleeping between attempts while the outcome is a
/// retryable `Failed`. Returns the final outcome and the number of attempts.
/// A final `Failed` after more than one attempt has ` (after N attempts)`
/// appended to its error; `Success` and `Skipped` pass through unchanged.
/// Sleeps never decrease and never exceed [`RetryPolicy::max_delay`].
/// Test: `tests::retryable_failure_then_success`,
/// `tests::non_retryable_error_gets_one_attempt`,
/// `tests::persistent_timeout_keeps_last_error`.
pub fn fetch_with_retry(
    mut attempt: impl FnMut() -> FetchOutcome,
    policy: RetryPolicy,
    budget: &mut RetryBudget,
    mut sleep: impl FnMut(Duration),
) -> (FetchOutcome, u32) {
    let retries_on = !budget.is_tripped() && policy.max_attempts > 1;
    let max = if retries_on { policy.max_attempts } else { 1 };
    let mut attempts = 0u32;
    let mut last_delay = Duration::ZERO;
    loop {
        attempts += 1;
        let outcome = attempt();
        let retryable = matches!(&outcome, FetchOutcome::Failed { error, .. }
            if is_retryable_fetch_error(error));
        if retryable && attempts < max {
            let delay = policy
                .delay(attempts, clock_jitter())
                .max(last_delay)
                .min(policy.max_delay());
            last_delay = delay;
            if let FetchOutcome::Failed { remote, error } = &outcome {
                warn!(
                    remote,
                    error,
                    attempt = attempts,
                    ?delay,
                    "transient fetch failure; retrying"
                );
            }
            sleep(delay);
            continue;
        }
        if retries_on {
            budget.record(retryable);
        }
        let outcome = match outcome {
            FetchOutcome::Failed { remote, error } if attempts > 1 => FetchOutcome::Failed {
                remote,
                error: format!("{error} (after {attempts} attempts)"),
            },
            other => other,
        };
        return (outcome, attempts);
    }
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod tests;
