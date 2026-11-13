//! Bounded retry policy for Notion requests (#252).
//!
//! Why: a first sync reads the whole workspace, thousands of requests over
//! tens of minutes; one transient failure must not fail the sync.
//! What: [`RetryPolicy`] classifies an attempt's failure as transient
//! (transport or timeout error, HTTP 429, any 5xx) or final, and computes the
//! wait before the next attempt: the server's `Retry-After` when it sent one,
//! else exponential backoff with jitter. Attempts and the summed wait are
//! both bounded.
//! Test: `client_tests` — `a_429_honors_retry_after`,
//! `only_transient_statuses_are_retried`, `a_transport_error_is_retried`,
//! `the_total_retry_delay_is_bounded`; `backoff_doubles_with_jitter_and_is_capped`;
//! `pipeline_tests::transient_comment_failures_are_retried_and_the_sync_completes`.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::time::Duration;

use reqwest::StatusCode;

/// Notion's "overloaded" status, treated like 429.
const STATUS_OVERLOADED: u16 = 529;

/// How a Notion request retries a transient failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts per request, counting the first. At least 1.
    pub max_attempts: u32,
    /// Backoff before the second attempt; doubles per attempt.
    pub base_delay: Duration,
    /// Ceiling on one wait, backoff or `Retry-After`.
    pub max_delay: Duration,
    /// Ceiling on the waits of one request, summed. A wait that would cross
    /// it ends the retries and returns the last error.
    pub max_total_delay: Duration,
    /// Length of one `Retry-After` second: 1 s in production; tests shrink
    /// it so a honored `Retry-After` costs milliseconds.
    pub retry_after_unit: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 6,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            max_total_delay: Duration::from_secs(120),
            retry_after_unit: Duration::from_secs(1),
        }
    }
}

impl RetryPolicy {
    /// The wait before attempt `failed_attempt + 1`.
    ///
    /// A `Retry-After` of `n` waits `n` units, clamped to `max_delay`.
    /// Without one: `base_delay * 2^(failed_attempt - 1)`, clamped to
    /// `max_delay`, then "equal jitter" — half fixed, half random — so many
    /// clients do not retry in lockstep.
    pub fn wait(&self, failed_attempt: u32, retry_after_secs: Option<u64>) -> Duration {
        if let Some(secs) = retry_after_secs {
            let units = u32::try_from(secs).unwrap_or(u32::MAX);
            return self
                .retry_after_unit
                .saturating_mul(units)
                .min(self.max_delay);
        }
        let factor = 1u32
            .checked_shl(failed_attempt.saturating_sub(1))
            .unwrap_or(u32::MAX);
        let exp = self.base_delay.saturating_mul(factor).min(self.max_delay);
        let half = exp / 2;
        half + half.mul_f64(jitter_fraction())
    }
}

/// A uniform-ish value in `[0, 1)`, from std's randomly keyed hasher.
fn jitter_fraction() -> f64 {
    let bits = RandomState::new().hash_one(()) >> 11;
    // 53 significant bits fit an f64 exactly.
    bits as f64 / (1u64 << 53) as f64
}

/// 429 (rate limited) or 529 (overloaded).
pub fn is_throttled(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.as_u16() == STATUS_OVERLOADED
}

/// A status worth another attempt: throttling or any 5xx.
pub fn is_transient_status(status: StatusCode) -> bool {
    is_throttled(status) || status.is_server_error()
}

/// A transport failure worth another attempt: connect, timeout, reset,
/// body read. Not a request that could never be built or a redirect loop.
pub fn is_transient_transport(e: &reqwest::Error) -> bool {
    !(e.is_builder() || e.is_redirect())
}

/// `Retry-After` in delta-seconds, when the response carries one.
pub fn retry_after_secs(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_with_jitter_and_is_capped() {
        let p = RetryPolicy {
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(500),
            ..RetryPolicy::default()
        };
        for (attempt, full_ms) in [(1, 100), (2, 200), (3, 400), (4, 500), (30, 500)] {
            let w = p.wait(attempt, None);
            let full = Duration::from_millis(full_ms);
            assert!(w >= full / 2 && w <= full, "attempt {attempt}: {w:?}");
        }
        assert_eq!(p.wait(1, Some(3)), Duration::from_millis(500), "clamped");
        assert_eq!(
            RetryPolicy::default().wait(1, Some(2)),
            Duration::from_secs(2)
        );
    }
}
