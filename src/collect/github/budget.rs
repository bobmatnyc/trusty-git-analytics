//! The one bound on how much GitHub fetching a single `tga` run may do (#6084).
//!
//! Why: every GitHub call in this crate retried 429 on a fixed 1s/2s/4s ladder
//! and then moved on to the next call, which retried the same way. Nothing
//! carried state ACROSS calls, so a secondary rate limit made every remaining
//! item pay four more rejected requests: a self-audit of this repo sat in a
//! continuous 429 loop for 45+ minutes across 2625 pull requests, and the
//! per-`#N` reference path reproduced it across 3681 references. A per-request
//! attempt cap cannot stop that — only a budget the whole run shares can.
//! What: [`FetchBudget`] is that shared state. It holds a total sleep
//! allowance, latches a breaker the first time the allowance is exhausted so
//! every later call fails fast instead of issuing another rejected request,
//! and carries the ledger of bounds the run actually hit so a truncated result
//! is reported as truncated rather than presented as complete.
//! [`rate_limit_delay`] is the single place a GitHub response is classified as
//! rate-limited and its `Retry-After` turned into a bounded wait.
//! Test: `budget_tests` in this directory.

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, RETRY_AFTER};

use crate::collect::errors::CollectError;

/// Hard ceiling on pages any one paginated GitHub listing will walk.
///
/// At [`super::client::PAGE_SIZE`] (100) per page this is 10,000 items — far
/// past any repository this tool profiles, and finite even when GitHub keeps
/// answering with full pages.
pub(crate) const MAX_PAGES: u32 = 100;

/// Hard ceiling on the pages the bulk pull-request listing will walk.
///
/// Why (#200): the PR listing is the only walk that scales with a repository's
/// ENTIRE pull-request history, and real repositories exceed [`MAX_PAGES`]
/// (10,000 PRs) — duetto held 10,000 and rust-lang/rust holds ~99,000. The
/// shared cap silently truncated them. The `/pulls` endpoint ends a walk with a
/// short page, so a large-but-finite cap keeps the #6084 guarantee (a walk
/// stays finite even when GitHub keeps answering with full pages) while letting
/// a large repository collect fully.
///
/// At 100 per page this is 200,000 pull requests.
/// Test: `bounded_fetch_tests::a_pulls_listing_pages_past_the_shared_cap_and_collects_fully`.
pub(crate) const PULLS_MAX_PAGES: u32 = 2000;

/// Hard ceiling on per-reference issue lookups in one classification batch.
///
/// The `fetch_on_reference` path issues one Issues-API call per unique `#N`
/// found in commit messages, and that count scales with repository history
/// rather than with anything the operator chose.
pub(crate) const MAX_REFERENCE_LOOKUPS: usize = 500;

/// Total wall-clock one run may spend asleep waiting out GitHub.
pub(crate) const RATE_LIMIT_SLEEP_BUDGET: Duration = Duration::from_secs(120);

/// Operator override for [`RATE_LIMIT_SLEEP_BUDGET`], in whole seconds.
///
/// Why (#6565): the allowance now bounds a whole run rather than one client, so
/// the same 120 s covers strictly more work than it used to. A long multi-org
/// sweep that legitimately needs a larger total has no other way to ask for one.
/// What: `TGA_RATE_LIMIT_SLEEP_BUDGET_SECS`, a positive integer.
/// Test: `run_sleep_budget_honours_a_valid_override`,
/// `run_sleep_budget_ignores_junk_overrides`.
pub(crate) const RATE_LIMIT_SLEEP_BUDGET_ENV: &str = "TGA_RATE_LIMIT_SLEEP_BUDGET_SECS";

/// The total sleep allowance this run should use.
///
/// Why: reads the override in the one place that owns the policy, so no caller
/// re-implements the parse.
/// What: [`run_sleep_budget_from`] applied to [`RATE_LIMIT_SLEEP_BUDGET_ENV`].
/// Test: covered through `run_sleep_budget_from`'s tests.
pub(crate) fn run_sleep_budget() -> Duration {
    run_sleep_budget_from(std::env::var(RATE_LIMIT_SLEEP_BUDGET_ENV).ok().as_deref())
}

/// Pure half of [`run_sleep_budget`], over an already-read value.
///
/// Why: pure so the parse is testable without mutating a process env shared with
/// every other test in the binary.
/// What: a trimmed, positive integer wins; unset, empty, `0`, and unparseable
/// all fall back to [`RATE_LIMIT_SLEEP_BUDGET`]. A zero allowance would make the
/// breaker latch on the first rate-limited response, so junk must never produce
/// one.
/// Test: `run_sleep_budget_honours_a_valid_override`,
/// `run_sleep_budget_ignores_junk_overrides`.
pub(crate) fn run_sleep_budget_from(value: Option<&str>) -> Duration {
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .map_or(RATE_LIMIT_SLEEP_BUDGET, Duration::from_secs)
}

/// Longest single wait honoured, however large `Retry-After` is.
///
/// GitHub answers a drained primary limit with a reset up to an hour out.
/// Sleeping that long inside a collection run is indistinguishable from a
/// hang, so the budget stops the run instead.
pub(crate) const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Wait used when a rate-limited response names no delay of its own.
pub(crate) const DEFAULT_RATE_LIMIT_DELAY: Duration = Duration::from_secs(1);

/// Classify a GitHub response and, when it is rate-limited, say how long to wait.
///
/// Why: GitHub signals its two rate limits differently and neither is just
/// "429". A secondary limit arrives as 403 or 429 carrying `Retry-After`; a
/// drained primary limit arrives as 403 with `x-ratelimit-remaining: 0` and an
/// `x-ratelimit-reset` epoch. A plain 403 — a token without the scope — is
/// neither, and retrying it is pure waste, so the classification has to be
/// narrower than "the status was 403".
/// What: returns `Some(delay)` only for those two shapes, preferring
/// `Retry-After`, then `x-ratelimit-reset`, then [`DEFAULT_RATE_LIMIT_DELAY`],
/// clamped to [`MAX_RETRY_AFTER`]. Returns `None` for everything else,
/// including a 403 with no rate-limit evidence.
/// Test: `budget_tests::retry_after_is_preferred_over_every_other_hint`,
/// `budget_tests::a_403_without_rate_limit_evidence_is_not_a_rate_limit`,
/// `budget_tests::a_reset_epoch_is_used_when_retry_after_is_absent`,
/// `budget_tests::a_very_long_retry_after_is_clamped`.
pub(crate) fn rate_limit_delay(status: u16, headers: &HeaderMap) -> Option<Duration> {
    let retry_after = header_u64(headers, RETRY_AFTER.as_str()).map(Duration::from_secs);
    let quota_drained = header_u64(headers, "x-ratelimit-remaining") == Some(0);

    let rate_limited = status == 429 || (status == 403 && (retry_after.is_some() || quota_drained));
    if !rate_limited {
        return None;
    }

    let delay = retry_after
        .or_else(|| reset_delay(headers))
        .unwrap_or(DEFAULT_RATE_LIMIT_DELAY);
    Some(delay.min(MAX_RETRY_AFTER))
}

/// Read a header as an unsigned integer, or `None` when absent or unparseable.
fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
}

/// Turn an `x-ratelimit-reset` epoch into a delay from now.
///
/// A reset already in the past yields `Duration::ZERO` rather than `None`, so a
/// clock skew shortens the wait instead of falling through to the default.
fn reset_delay(headers: &HeaderMap) -> Option<Duration> {
    let reset = header_u64(headers, "x-ratelimit-reset")?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(Duration::from_secs(reset.saturating_sub(now)))
}

/// Mutable half of a [`FetchBudget`], guarded by one lock.
#[derive(Default)]
struct BudgetState {
    /// Rate-limit and backoff sleep already committed by this run.
    slept: Duration,
    /// Set once the breaker latches: the status and delay that exhausted it.
    tripped: Option<(u16, Option<Duration>)>,
    /// Bounds this run hit, in operator-facing wording.
    notices: Vec<String>,
}

/// The run-wide bound every GitHub call in this crate charges against.
///
/// Why: see the module header — a spiral is what happens when each request
/// bounds only itself. Sharing one budget is what converts "45 minutes of
/// rejected requests" into a terminal outcome the caller can report.
/// What: a sleep allowance plus a latching breaker plus a truncation ledger.
/// [`Self::reserve`] is the only way to spend the allowance; once it refuses,
/// [`Self::tripped_error`] answers for every later call without a request
/// leaving the process.
/// Test: `budget_tests::*`.
pub struct FetchBudget {
    /// Total sleep this budget will authorise across its whole lifetime.
    sleep_budget: Duration,
    state: Mutex<BudgetState>,
    /// #246: the run budget a scoped budget also charges, as the outer bound.
    /// `None` for a run's own budget.
    parent: Option<RunBudget>,
}

impl FetchBudget {
    /// A budget with the standard [`RATE_LIMIT_SLEEP_BUDGET`] allowance.
    pub fn new() -> Self {
        Self::with_sleep_budget(RATE_LIMIT_SLEEP_BUDGET)
    }

    /// A budget with a caller-chosen allowance, so tests can exhaust it without
    /// sleeping for two minutes.
    pub fn with_sleep_budget(sleep_budget: Duration) -> Self {
        Self {
            sleep_budget,
            state: Mutex::new(BudgetState::default()),
            parent: None,
        }
    }

    /// A budget scoped to one org's PR listing, inside the run's budget.
    ///
    /// Why: #246 — the PR listing walked every org on the run's one breaker,
    /// so a rate limit in the first org latched it and every repository in a
    /// later org stopped at page 1. A scoped budget gives each org its own
    /// allowance and breaker while the run budget stays the outer ceiling.
    /// What: a budget with its own `sleep_budget` and breaker. Every
    /// reservation must fit both this allowance and `parent`'s; a trip on the
    /// attempt cap latches this budget only. A parent that has latched
    /// refuses for every scoped budget. Truncation notices go to the parent,
    /// so the run reports them.
    /// Test: `budget_tests::a_scoped_budget_trips_alone_and_leaves_the_run_budget_open`,
    /// `budget_tests::a_scoped_budget_is_bounded_by_its_parent`.
    pub(crate) fn scoped(parent: &RunBudget, sleep_budget: Duration) -> Self {
        Self {
            sleep_budget,
            state: Mutex::new(BudgetState::default()),
            parent: Some(parent.clone()),
        }
    }

    /// Sleep allowance still unspent; zero once the breaker has latched.
    // #246: sizes each org's share of what the run has left.
    pub(crate) fn remaining(&self) -> Duration {
        let state = self.lock();
        if state.tripped.is_some() {
            return Duration::ZERO;
        }
        self.sleep_budget.saturating_sub(state.slept)
    }

    /// Take the lock, treating a poisoned mutex as usable.
    ///
    /// A panic while holding this lock leaves counters, not invariants, so
    /// refusing to fetch afterwards would be a worse outcome than continuing.
    fn lock(&self) -> std::sync::MutexGuard<'_, BudgetState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Commit `delay` of waiting against the allowance.
    ///
    /// Why: this is the single choke point that turns an unbounded retry storm
    /// into a terminal error. A caller that sleeps without reserving first has
    /// reintroduced the bug.
    /// What: returns the delay to sleep when it fits. When it does not — or
    /// when the breaker already latched — latches the breaker and returns
    /// [`CollectError::Throttled`], which the caller propagates.
    /// Test: `budget_tests::the_budget_latches_after_the_allowance_is_spent`,
    /// `budget_tests::a_latched_budget_refuses_every_later_reservation`.
    ///
    /// # Errors
    ///
    /// [`CollectError::Throttled`] once the allowance cannot cover `delay`.
    pub(crate) fn reserve(&self, delay: Duration, status: u16) -> Result<Duration, CollectError> {
        {
            let mut state = self.lock();
            if let Some((status, retry_after)) = state.tripped {
                return Err(CollectError::Throttled {
                    status,
                    retry_after,
                });
            }
            if state.slept + delay > self.sleep_budget {
                state.tripped = Some((status, Some(delay)));
                return Err(CollectError::Throttled {
                    status,
                    retry_after: Some(delay),
                });
            }
        }
        // #246: the run budget stays the outer bound for a scoped budget. Its
        // lock is taken after this budget's is released, never both at once.
        if let Some(parent) = &self.parent {
            parent.shared().reserve(delay, status)?;
        }
        self.lock().slept += delay;
        Ok(delay)
    }

    /// Latch the breaker because a request exhausted its own attempt cap while
    /// still rate-limited, and return the error to propagate.
    ///
    /// Test: `budget_tests::tripping_on_the_attempt_cap_latches_the_breaker`.
    // #246: a scoped budget latches only itself, never its parent.
    pub(crate) fn trip(&self, status: u16, retry_after: Option<Duration>) -> CollectError {
        let mut state = self.lock();
        if state.tripped.is_none() {
            state.tripped = Some((status, retry_after));
        }
        CollectError::Throttled {
            status,
            retry_after,
        }
    }

    /// The error every call should fail with once the breaker has latched, or
    /// `None` while the run may still fetch.
    pub(crate) fn tripped_error(&self) -> Option<CollectError> {
        let own = self
            .lock()
            .tripped
            .map(|(status, retry_after)| CollectError::Throttled {
                status,
                retry_after,
            });
        // #246: a latched run budget stops every scoped budget too.
        own.or_else(|| {
            self.parent
                .as_ref()
                .and_then(|p| p.shared().tripped_error())
        })
    }

    /// Record that a bound trimmed the result set.
    ///
    /// Why: a cap that silently shortens a listing produces data that reads
    /// exactly like a complete collection. Every caller that breaks out of a
    /// walk early records why here so the run can say so.
    /// Test: `budget_tests::truncation_notices_are_recorded_in_order`.
    pub(crate) fn note_truncation(&self, message: impl Into<String>) {
        // #246: a scoped budget's notices belong to the run's ledger.
        match &self.parent {
            Some(parent) => parent.shared().note_truncation(message),
            None => self.lock().notices.push(message.into()),
        }
    }

    /// Every truncation recorded so far, in the order it happened.
    pub(crate) fn notices(&self) -> Vec<String> {
        self.lock().notices.clone()
    }
}

impl Default for FetchBudget {
    fn default() -> Self {
        Self::new()
    }
}

/// The ONE [`FetchBudget`] a `tga` run shares across every client it builds.
///
/// Why (#6565): #6084 gave each client a budget, which bounds a client and not
/// a run. One `collect` builds several — org discovery, the PR sweep, the
/// reviewer pass — so the 120 s ceiling was charged that many times over, and
/// the wall-clock a run could spend asleep scaled with the number of clients
/// rather than being the fixed bound the constant reads as. Handing every client
/// a handle to the same budget is what makes the ceiling per-RUN, so a rate
/// limit that latches during org discovery is already latched when the PR sweep
/// starts instead of funding a fresh storm.
///
/// What: a cheap-to-clone handle over one shared [`FetchBudget`]. Cloning shares
/// the allowance; constructing a new one does not, which is why construction is
/// confined to a run's entry point and every other layer takes a handle.
/// Test: `two_clients_draw_down_one_run_budget`,
/// `separate_run_budgets_do_not_share_an_allowance`.
#[derive(Clone)]
pub struct RunBudget(std::sync::Arc<FetchBudget>);

impl RunBudget {
    /// A run budget with this run's allowance — see [`run_sleep_budget`].
    pub fn new() -> Self {
        Self::with_sleep_budget(run_sleep_budget())
    }

    /// A run budget with a caller-chosen allowance, so tests can exhaust it
    /// without sleeping for two minutes.
    pub fn with_sleep_budget(sleep_budget: Duration) -> Self {
        Self(std::sync::Arc::new(FetchBudget::with_sleep_budget(
            sleep_budget,
        )))
    }

    /// The shared budget every call on this run charges against.
    pub(crate) fn shared(&self) -> &FetchBudget {
        &self.0
    }
}

impl Default for RunBudget {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for RunBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunBudget")
            .field("sleep_budget", &self.0.sleep_budget)
            .finish_non_exhaustive()
    }
}

/// One org's PR-listing budget and its truncation count (#246).
struct OrgListing {
    budget: FetchBudget,
    repos: usize,
    truncated: usize,
}

/// Per-org budgets for one pass of the PR listing.
///
/// Why: #246 — one shared breaker meant the first org to hit a rate limit
/// truncated every repository in every later org, so orgs listed later were
/// systematically under-collected.
/// What: hands each org (the repository owner) a [`FetchBudget::scoped`]
/// budget the first time one of its repositories is listed. Its allowance is
/// an equal share of what the run budget has left, split across the orgs not
/// yet started, so an earlier org cannot spend a later org's share. Counts
/// each org's repositories and the ones a rate limit truncated, and words one
/// summary line per org that had any.
/// Test: `budget_tests::org_budgets_split_what_the_run_has_left`,
/// `client_tests::bounded_fetch_tests::a_second_org_is_fully_listed_after_the_first_trips_its_budget`.
pub(crate) struct OrgBudgets {
    run: RunBudget,
    orgs_left: usize,
    /// Org slugs in the order their first repository was listed.
    order: Vec<String>,
    orgs: std::collections::HashMap<String, OrgListing>,
}

impl OrgBudgets {
    /// Budgets for a listing over `owners` (one entry per repository, any
    /// order, duplicates allowed), drawn from `run`.
    pub(crate) fn new<'o>(run: &RunBudget, owners: impl IntoIterator<Item = &'o str>) -> Self {
        let distinct: std::collections::HashSet<&str> = owners.into_iter().collect();
        Self {
            run: run.clone(),
            orgs_left: distinct.len(),
            order: Vec::new(),
            orgs: std::collections::HashMap::new(),
        }
    }

    /// The budget for `owner`'s next repository; counts that repository.
    pub(crate) fn budget_for(&mut self, owner: &str) -> &FetchBudget {
        let run = &self.run;
        let orgs_left = &mut self.orgs_left;
        let order = &mut self.order;
        let org = self.orgs.entry(owner.to_string()).or_insert_with(|| {
            // An owner not counted in `new` still gets a share, never a 0 divisor.
            let share = run.shared().remaining() / (*orgs_left).max(1) as u32;
            *orgs_left = orgs_left.saturating_sub(1);
            order.push(owner.to_string());
            OrgListing {
                budget: FetchBudget::scoped(run, share),
                repos: 0,
                truncated: 0,
            }
        });
        org.repos += 1;
        &org.budget
    }

    /// Record that a rate limit truncated one of `owner`'s repositories.
    pub(crate) fn note_truncated(&mut self, owner: &str) {
        if let Some(org) = self.orgs.get_mut(owner) {
            org.truncated += 1;
        }
    }

    /// One PARTIAL line per org with a truncated repository, naming the count.
    pub(crate) fn summaries(&self) -> Vec<String> {
        self.order
            .iter()
            .filter_map(|owner| {
                let org = self.orgs.get(owner)?;
                (org.truncated > 0).then(|| {
                    format!(
                        "GitHub pulls listing for org {owner}: {} of {} repositories truncated \
                         by a rate limit; these results are PARTIAL",
                        org.truncated, org.repos
                    )
                })
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "budget_tests.rs"]
mod budget_tests;
