//! Draining and persisting the concurrent pull-request fetch.
//!
//! Why: the fetch fans out across providers on a `JoinSet` and every result has
//! four possible dispositions — the task panicked, no provider matched the
//! returned name, the fetch failed, or the store failed. Each maps to a
//! different [`crate::collect::CollectionFault`] severity, and keeping that
//! mapping in one place is what stops a half-persisted run from reporting
//! success. Extracted from [`crate::collect::collector`] for the same reason
//! [`crate::collect::github_pipeline`] was: that module is on a frozen SLOC
//! budget.
//!
//! What: [`drain_and_store_pull_requests`] awaits every spawned fetch, matches
//! each result back to the provider that produced it, and persists the batch.
//!
//! Test: `crate::collect::correlate::tests` covers what the persisted rows then
//! feed; the fault severities are covered by
//! `crate::commands::collect::tests::a_failed_stage_makes_collect_exit_non_zero`.

use std::sync::Arc;

use tokio::task::JoinSet;
use tracing::info;

use crate::collect::collector::CollectionStats;
use crate::collect::errors::Result;
use crate::collect::pr_provider::PrProvider;
use crate::core::db::Database;
use crate::core::models::PullRequest;

/// Await every spawned pull-request fetch and persist what came back.
///
/// Why: see the module header — one place decides which failures reach the
/// process exit code. A fetch or store that failed wholesale is
/// [`CollectionStats::fail_stage`], because that provider's data is absent from
/// the database; a payload anomaly that cost only some harvested detail is
/// [`CollectionStats::skip_item`].
///
/// What: drains `set`, matches each `(provider_name, result)` pair back to its
/// entry in `providers`, records an empty-source-branch anomaly (#5734) and
/// the provider's per-repository fetch faults (#146), then calls that
/// provider's `store_pull_requests`. Every disposition is recorded; none
/// aborts the drain, so one bad provider cannot cost the others.
///
/// #5734: a provider that answers with `Some("")` for a head ref made a claim
/// and the claim was empty, which is an anomaly worth reporting. `None` means
/// the provider never claimed to supply one — Bitbucket today — and is silent
/// by design. Collapsing the two would make a broken payload indistinguishable
/// from a branch harvest that legitimately found nothing.
///
/// Test: `tests::blank_head_ref_is_recorded_as_a_skipped_item`,
/// `tests::a_pr_list_404_is_a_counted_warning_not_a_stage_failure`,
/// `tests::a_pr_list_403_fails_the_stage_once`.
pub(super) async fn drain_and_store_pull_requests(
    mut set: JoinSet<(String, Result<Vec<PullRequest>>)>,
    providers: &[Arc<dyn PrProvider + Send + Sync>],
    db: &mut Database,
    stats: &mut CollectionStats,
) {
    while let Some(joined) = set.join_next().await {
        let (provider_name, fetch_result) = match joined {
            Ok(t) => t,
            Err(e) => {
                stats.fail_stage(format!("PR fetch task panicked: {e}"));
                continue;
            }
        };
        let prs = match fetch_result {
            Ok(prs) => prs,
            Err(e) => {
                stats.fail_stage(format!("{provider_name} PR fetch failed: {e}"));
                continue;
            }
        };
        let Some(provider) = providers.iter().find(|p| p.name() == provider_name) else {
            stats.fail_stage(format!(
                "internal: no provider registered for '{provider_name}' when storing PRs"
            ));
            continue;
        };
        record_blank_head_refs(stats, &provider_name, &prs);
        // #6084: a walk that stopped at a cap returns rows that look complete.
        // Recording each notice is what keeps the shortfall visible.
        for notice in provider.fetch_notices() {
            stats.skip_item(format!("{provider_name}: {notice}"));
        }
        // #146: a skipped repository is a fault at the severity the provider
        // chose (404 a warning, anything else a stage failure), never silent.
        stats.errors.extend(provider.fetch_faults());
        match provider.store_pull_requests(db, &prs) {
            Ok(n) => {
                info!(provider = %provider_name, prs = n, "stored pull requests");
                stats.prs_fetched += n;
            }
            Err(e) => {
                stats.fail_stage(format!("{provider_name} PR store failed: {e}"));
            }
        }
    }
}

/// Record pull requests whose provider claimed a source branch and gave an
/// empty one (#5734).
///
/// Why: without this the branch harvest fails open — a provider returning blank
/// refs yields zero keys, which looks exactly like a repository whose branches
/// carry no ticket keys.
/// What: counts `Some("")` head refs and records ONE `skip_item` naming the
/// count. `None` is skipped: that is "no claim made", not a fault.
/// Test: `tests::blank_head_ref_is_recorded_as_a_skipped_item`,
/// `tests::absent_head_ref_is_not_a_fault`.
fn record_blank_head_refs(stats: &mut CollectionStats, provider: &str, prs: &[PullRequest]) {
    let blank = prs
        .iter()
        .filter(|p| p.head_ref.as_deref() == Some(""))
        .count();
    if blank > 0 {
        stats.skip_item(format!(
            "{provider}: {blank} pull request(s) reported an empty source branch; \
             no branch ticket key harvested for them"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::PrState;
    use chrono::Utc;

    fn pr(head_ref: Option<&str>) -> PullRequest {
        PullRequest {
            id: 0,
            pr_number: 1,
            repository: "acme/widgets".into(),
            title: "T".into(),
            author: "ada".into(),
            state: PrState::Merged,
            created_at: Utc::now(),
            merged_at: None,
            commit_shas: "[]".into(),
            fetched_at: "2026-01-01T00:00:00Z".into(),
            head_ref: head_ref.map(str::to_string),
            body_ticket_id: None,
        }
    }

    /// Why: #5734 — an empty source branch harvests nothing, and without a
    /// recorded fault that is indistinguishable from a repository whose
    /// branches simply carry no keys. This is the fail-open the check exists
    /// to close.
    /// What: two blank refs produce one `ItemSkipped` fault naming the count,
    /// and no stage failure — the rest of the batch still persisted.
    /// Test: this test itself.
    #[test]
    fn blank_head_ref_is_recorded_as_a_skipped_item() {
        let mut stats = CollectionStats::default();
        record_blank_head_refs(
            &mut stats,
            "github",
            &[pr(Some("")), pr(Some("feature/PROJ-1")), pr(Some(""))],
        );
        assert_eq!(
            stats.errors.len(),
            1,
            "one aggregated fault, not one per PR"
        );
        assert!(
            stats.stage_failures().is_empty(),
            "a payload anomaly must not reach the exit code"
        );
        let msg = stats.errors[0].message.clone();
        assert!(msg.contains("github"), "{msg}");
        assert!(msg.contains('2'), "the count must be named: {msg}");
    }

    /// Why: `None` means the provider never claimed to supply a source branch —
    /// Bitbucket today. Reporting that as a fault would make every Bitbucket
    /// collection noisy about a feature it does not implement.
    /// What: absent and non-empty head refs record nothing.
    /// Test: this test itself.
    #[test]
    fn absent_head_ref_is_not_a_fault() {
        let mut stats = CollectionStats::default();
        record_blank_head_refs(
            &mut stats,
            "bitbucket",
            &[pr(None), pr(None), pr(Some("feature/PROJ-1"))],
        );
        assert!(stats.errors.is_empty());
    }

    /// `count` open pull requests numbered from `first`, in the list shape.
    fn pulls_json(first: usize, count: usize) -> serde_json::Value {
        (first..first + count)
            .map(|n| {
                serde_json::json!({
                    "number": n, "title": "T", "user": { "login": "ada" },
                    "state": "open", "created_at": "2026-01-01T00:00:00Z",
                    "merged_at": null
                })
            })
            .collect()
    }

    /// A non-success reply with no rate-limit headers: the SAML / token-scope /
    /// renamed-repository shape.
    fn denied(status: u16) -> wiremock::ResponseTemplate {
        wiremock::ResponseTemplate::new(status)
            .set_body_json(serde_json::json!({ "message": "denied or missing" }))
    }

    /// Run the GitHub PR-list fetch for `slugs` against `server` through the
    /// real drain, and return the stats.
    async fn drain_github_repos(server: &wiremock::MockServer, slugs: &[&str]) -> CollectionStats {
        use crate::collect::github::GitHubClient;
        use crate::core::config::GithubConfig;

        let cfg = GithubConfig {
            token: None,
            org: None,
            orgs: vec![],
            repo: None,
            fetch_prs: true,
            fetch_pr_reviews: false,
            review_fetch_concurrency: 1,
            ticket_regex: None,
            fetch_on_reference: false,
            work_items_unavailable: None,
        };
        let repos = slugs
            .iter()
            .filter_map(|slug| slug.split_once('/'))
            .map(|(o, r)| (o.to_string(), r.to_string()))
            .collect();
        let client = GitHubClient::new_for_prs(&cfg, repos)
            .expect("client builds")
            .with_api_base(server.uri());
        let provider: Arc<dyn PrProvider + Send + Sync> = Arc::new(client);
        let providers = vec![Arc::clone(&provider)];

        let mut set = JoinSet::new();
        set.spawn(async move { ("github".to_string(), provider.fetch_pull_requests().await) });
        let mut db = Database::open_in_memory().expect("open db");
        let mut stats = CollectionStats::default();
        drain_and_store_pull_requests(set, &providers, &mut db, &mut stats).await;
        stats
    }

    /// Run the GitHub PR-list fetch over `answers` — `(owner/repo, status)`,
    /// where 200 carries one pull request on a single page — and return the
    /// stats.
    async fn drain_github_against(answers: &[(&str, u16)]) -> CollectionStats {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        for (i, (slug, status)) in answers.iter().enumerate() {
            let reply = if *status == 200 {
                ResponseTemplate::new(200).set_body_json(pulls_json(i + 1, 1))
            } else {
                denied(*status)
            };
            Mock::given(method("GET"))
                .and(path(format!("/repos/{slug}/pulls")))
                .respond_with(reply)
                .mount(&server)
                .await;
        }
        let slugs: Vec<&str> = answers.iter().map(|(slug, _)| *slug).collect();
        drain_github_repos(&server, &slugs).await
    }

    /// Run the GitHub PR-list fetch for one repository whose page 1 is a full
    /// page of pull requests and whose page 2 answers `page2_status`; return
    /// the stats.
    async fn drain_github_page2_fails(page2_status: u16) -> CollectionStats {
        use crate::collect::github::client::PAGE_SIZE;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets/pulls"))
            .and(query_param("page", "1"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(pulls_json(1, PAGE_SIZE as usize)),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets/pulls"))
            .and(query_param("page", "2"))
            .respond_with(denied(page2_status))
            .mount(&server)
            .await;
        drain_github_repos(&server, &["acme/widgets"]).await
    }

    /// Why: #146 (D28) — the PR-list fetch dropped 119 HTTP 404s (renamed or
    /// deleted repositories) as warnings and the run reported 0 failures. A
    /// 404 cannot be fixed by re-running, so it must not fail the stage; it
    /// must still be counted.
    /// What: repositories answer 200, 404, 200. Asserts both live repositories
    /// stored their pull request, no stage failed (`tga collect` exits 0), and
    /// exactly one warning names the 404'd repository and the count.
    /// Test: this test itself. Catches "404 fails the stage" and "404 silent".
    #[tokio::test]
    async fn a_pr_list_404_is_a_counted_warning_not_a_stage_failure() {
        let stats = drain_github_against(&[
            ("acme/widgets", 200),
            ("acme/gone", 404),
            ("acme/zeta", 200),
        ])
        .await;

        assert_eq!(stats.prs_fetched, 2, "the repos around the 404 still store");
        assert!(
            stats.stage_failures().is_empty(),
            "a 404 must not fail the PR stage; got: {:?}",
            stats.errors
        );
        assert_eq!(
            stats.errors.len(),
            1,
            "a 404 must be one counted warning, not silent; got: {:?}",
            stats.errors
        );
        let msg = &stats.errors[0].message;
        for needle in ["1 of 3", "HTTP 404", "acme/gone"] {
            assert!(msg.contains(needle), "missing `{needle}` in: {msg}");
        }
    }

    /// Why: #146 (D28) — a 403 on one repository's PR list means the token
    /// cannot see it, and the run must not report success over that gap.
    /// What: repositories answer 200, plain 403, 200. Asserts both live
    /// repositories stored their pull request (no abort) and exactly one stage
    /// failure names the repository and its status code.
    /// Test: this test itself. Catches "403 only warns" and "abort on 403".
    #[tokio::test]
    async fn a_pr_list_403_fails_the_stage_once() {
        let stats = drain_github_against(&[
            ("acme/widgets", 200),
            ("acme/secret", 403),
            ("acme/zeta", 200),
        ])
        .await;

        assert_eq!(stats.prs_fetched, 2, "the repos around the 403 still store");
        let failures = stats.stage_failures();
        assert_eq!(
            failures.len(),
            1,
            "a 403 must fail the PR stage exactly once; got: {:?}",
            stats.errors
        );
        assert_eq!(stats.errors.len(), 1, "no extra faults: {:?}", stats.errors);
        let msg = &failures[0].message;
        for needle in ["1 of 3", "HTTP 403: 1", "acme/secret (HTTP 403)"] {
            assert!(msg.contains(needle), "missing `{needle}` in: {msg}");
        }
    }

    /// Why: #146 — a page-2 error used to return `Err` for the whole
    /// repository, discarding every pull request page 1 had already fetched.
    /// Under D28 a 404 exits 0, so that loss would go unnoticed.
    /// What: page 1 is a full page, page 2 answers 404. Asserts all page-1
    /// pull requests are stored, the stage is not failed, and one counted
    /// warning says which pages were lost.
    /// Test: this test itself. Catches "earlier pages dropped" and "partial
    /// fetch recorded with no fault".
    #[tokio::test]
    async fn a_page_2_404_keeps_page_1_and_is_a_counted_warning() {
        use crate::collect::github::client::PAGE_SIZE;

        let stats = drain_github_page2_fails(404).await;

        assert_eq!(
            stats.prs_fetched, PAGE_SIZE as usize,
            "page 1's pull requests must be stored"
        );
        assert!(
            stats.stage_failures().is_empty(),
            "a page-2 404 must not fail the PR stage; got: {:?}",
            stats.errors
        );
        assert_eq!(
            stats.errors.len(),
            1,
            "a partial fetch must be one counted warning; got: {:?}",
            stats.errors
        );
        let msg = &stats.errors[0].message;
        for needle in [
            "1 of 1",
            "HTTP 404",
            "acme/widgets (pull requests after page 1 not collected)",
        ] {
            assert!(msg.contains(needle), "missing `{needle}` in: {msg}");
        }
    }

    /// Why: #146 — as above, but a 403 on page 2 must still fail the stage
    /// while keeping page 1.
    /// What: page 1 is a full page, page 2 answers a plain 403. Asserts all
    /// page-1 pull requests are stored and exactly one stage failure names the
    /// repository, the lost pages and the status code.
    /// Test: this test itself. Catches "earlier pages dropped" and "partial
    /// fetch recorded with no fault".
    #[tokio::test]
    async fn a_page_2_403_keeps_page_1_and_fails_the_stage_once() {
        use crate::collect::github::client::PAGE_SIZE;

        let stats = drain_github_page2_fails(403).await;

        assert_eq!(
            stats.prs_fetched, PAGE_SIZE as usize,
            "page 1's pull requests must be stored"
        );
        let failures = stats.stage_failures();
        assert_eq!(
            failures.len(),
            1,
            "a page-2 403 must fail the PR stage exactly once; got: {:?}",
            stats.errors
        );
        assert_eq!(stats.errors.len(), 1, "no extra faults: {:?}", stats.errors);
        let msg = &failures[0].message;
        for needle in [
            "1 of 1",
            "HTTP 403: 1",
            "acme/widgets (pull requests after page 1 not collected) (HTTP 403)",
        ] {
            assert!(msg.contains(needle), "missing `{needle}` in: {msg}");
        }
    }
}
