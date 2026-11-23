//! Tests for the #262 fetch retry loop.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::Duration;

use super::*;

const TIMEOUT: &str = "failed to connect to github.com: Operation timed out; class=Net (12)";
const REFUSED: &str = "failed to connect to 127.0.0.1: Connection refused; class=Net (12)";

fn failed(error: &str) -> FetchOutcome {
    FetchOutcome::Failed {
        remote: "origin".to_string(),
        error: error.to_string(),
    }
}

fn success() -> FetchOutcome {
    FetchOutcome::Success {
        remote: "origin".to_string(),
    }
}

/// A policy whose sleeps are recorded, never slept.
fn policy(max_attempts: u32) -> RetryPolicy {
    RetryPolicy {
        max_attempts,
        base: Duration::from_secs(2),
        cap: Duration::from_secs(30),
    }
}

/// Run `fetch_with_retry` over scripted outcomes; return outcome, attempts, sleeps.
fn run_script(
    script: Vec<FetchOutcome>,
    policy: RetryPolicy,
    budget: &mut RetryBudget,
) -> (FetchOutcome, u32, Vec<Duration>) {
    let queue = RefCell::new(VecDeque::from(script));
    let mut sleeps = Vec::new();
    let (outcome, attempts) = fetch_with_retry(
        || {
            queue
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| failed(TIMEOUT))
        },
        policy,
        budget,
        |d| sleeps.push(d),
    );
    (outcome, attempts, sleeps)
}

#[test]
fn retryable_failure_then_success() {
    let (outcome, attempts, sleeps) = run_script(
        vec![failed(TIMEOUT), success()],
        policy(3),
        &mut RetryBudget::default(),
    );
    assert!(
        matches!(outcome, FetchOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(attempts, 2);
    assert_eq!(sleeps.len(), 1);
}

#[test]
fn non_retryable_error_gets_one_attempt() {
    for error in [
        "authentication required but no callback set; class=Ssh (23); code=Auth (-16)",
        "no non-interactive credentials available (tried SSH agent); class=Callback (26)",
        "unexpected http status code: 404; class=Http (34)",
        "unsupported URL protocol; class=Net (12)",
        "failed to open repo for fetch: could not find repository; class=Repository (6)",
        "the SSL certificate is invalid; class=Net (12); code=Certificate (-17)",
        "object file is corrupt; class=Odb (9)",
    ] {
        let (outcome, attempts, sleeps) = run_script(
            vec![failed(error), success()],
            policy(3),
            &mut RetryBudget::default(),
        );
        assert_eq!(attempts, 1, "{error}");
        assert!(sleeps.is_empty(), "{error}");
        match outcome {
            FetchOutcome::Failed { error: e, .. } => assert_eq!(e, error),
            other => panic!("expected Failed for {error}, got {other:?}"),
        }
    }
}

#[test]
fn persistent_timeout_keeps_last_error() {
    let last = "failed to connect to github.com: Connection reset by peer; class=Net (12)";
    let (outcome, attempts, sleeps) = run_script(
        vec![failed(TIMEOUT), failed(REFUSED), failed(last)],
        policy(3),
        &mut RetryBudget::default(),
    );
    assert_eq!(attempts, 3);
    assert_eq!(sleeps.len(), 2);
    match outcome {
        FetchOutcome::Failed { remote, error } => {
            assert_eq!(remote, "origin");
            assert_eq!(error, format!("{last} (after 3 attempts)"));
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn sleeps_are_non_decreasing_and_bounded() {
    let p = policy(10);
    let (_, attempts, sleeps) = run_script(Vec::new(), p, &mut RetryBudget::default());
    assert_eq!(attempts, 10);
    assert_eq!(sleeps.len(), 9);
    assert!(sleeps[0] >= p.base, "{sleeps:?}");
    for pair in sleeps.windows(2) {
        assert!(pair[0] <= pair[1], "decreasing sleeps: {sleeps:?}");
    }
    for s in &sleeps {
        assert!(*s <= p.cap + p.cap / 4, "sleep {s:?} over cap plus jitter");
    }
    assert!(sleeps[8] >= p.cap, "the cap is reached: {sleeps:?}");
}

#[test]
fn budget_stops_retries_after_consecutive_exhausted_repos() {
    let mut budget = RetryBudget::default();
    for repo in 0..DEFAULT_EXHAUSTED_REPO_LIMIT {
        assert!(!budget.is_tripped(), "tripped early at repo {repo}");
        let (_, attempts, _) = run_script(Vec::new(), policy(3), &mut budget);
        assert_eq!(attempts, 3, "repo {repo} retried");
    }
    assert!(budget.is_tripped());
    let (outcome, attempts, sleeps) = run_script(Vec::new(), policy(3), &mut budget);
    assert_eq!(attempts, 1);
    assert!(sleeps.is_empty());
    match outcome {
        FetchOutcome::Failed { error, .. } => assert_eq!(error, TIMEOUT),
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn a_success_resets_the_consecutive_count() {
    let mut budget = RetryBudget::new(2);
    run_script(Vec::new(), policy(2), &mut budget);
    run_script(vec![success()], policy(2), &mut budget);
    run_script(Vec::new(), policy(2), &mut budget);
    assert!(!budget.is_tripped());
    run_script(Vec::new(), policy(2), &mut budget);
    assert!(budget.is_tripped());
}

#[test]
fn config_keys_map_to_the_policy() {
    let d = RetryPolicy::default();
    assert_eq!(d.max_attempts, 3);
    assert_eq!(d.base, Duration::from_secs(2));
    assert_eq!(d.cap, Duration::from_secs(30));
    let off = RetryPolicy::from_config(Some(0), Some(0));
    assert_eq!(off.max_attempts, 1);
    assert_eq!(off.base, Duration::ZERO);
}

#[test]
fn classifier_table() {
    for (error, want) in [
        (TIMEOUT, true),
        (REFUSED, true),
        ("connect timeout; class=Http (34)", true),
        ("Failed to connect to host; class=Ssh (23)", true),
        ("connection reset by peer; class=Net (12)", true),
        // The exact shape of every failure in the #262 incident log.
        (
            "failed to connect to github.com: Operation timed out; class=Os (2)",
            true,
        ),
        (
            "failed to resolve path '/gone': No such file; class=Os (2)",
            false,
        ),
        ("Connection refused; class=Repository (6)", false),
        ("timed out; class=Callback (26)", false),
        (
            "failed to connect: authentication failed; class=Ssh (23)",
            false,
        ),
        ("unsupported URL protocol; class=Net (12)", false),
        ("repository not found; class=Http (34)", false),
    ] {
        assert_eq!(is_retryable_fetch_error(error), want, "{error}");
    }
}

/// A loopback port with nothing listening.
fn closed_loopback_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    port
}

/// The real libgit2 error for a refused connection is classified retryable,
/// and the real fetch path retries it to exhaustion and fails closed.
#[test]
fn real_connection_refused_is_retried_to_exhaustion() {
    let td = tempfile::tempdir().expect("tempdir");
    let repo = git2::Repository::init(td.path()).expect("init");
    let url = format!("http://127.0.0.1:{}/org/repo.git", closed_loopback_port());
    repo.remote("origin", &url).expect("remote");
    let fast = RetryPolicy {
        max_attempts: 3,
        base: Duration::from_millis(1),
        cap: Duration::from_millis(4),
    };
    let (prf, attempts) = crate::collect::git::fetch::fetch_and_record_with_retry(
        &repo,
        "refused",
        "origin",
        fast,
        &mut RetryBudget::default(),
        None,
    );
    assert_eq!(attempts, 3, "{:?}", prf.outcome);
    match prf.outcome {
        FetchOutcome::Failed { error, .. } => {
            assert!(error.ends_with("(after 3 attempts)"), "{error}");
            let first = error.trim_end_matches(" (after 3 attempts)");
            assert!(is_retryable_fetch_error(first), "{first}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}
