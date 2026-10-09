//! Linear issue enrichment for the Stage 1 collection pipeline.
//!
//! Why: this was inline in [`crate::collect::collector::CollectionPipeline::run`],
//! which sits over the 500-SLOC production cap on a frozen ratchet budget.
//! Lifting it out mirrors the existing `github_pipeline` split and keeps
//! `collector.rs` shrinking rather than growing (#5197).
//! What: [`fetch_and_store_linear_issues`] — reads every commit, asks the
//! Linear client which referenced issues exist, and persists them into both
//! `linear_issues` and, since #5219, the source-agnostic `work_items` /
//! `commit_work_items` pair that correlation and DOC-70's board axis read.
//! Every failure is non-fatal to the run and lands in `stats.errors`. Since
//! #5655 each one is recorded as a stage failure — every arm here means the
//! Linear corpus is absent, not that one issue was skipped — so `tga collect`
//! exits non-zero after finishing its remaining stages.
//! Test: this module's own `tests` cover the `work_items` projection; the
//! client half is unit-tested in `crate::collect::linear`, and the
//! orchestration is exercised end-to-end by the gated integration tests.

use std::collections::HashMap;

use tracing::info;

use crate::collect::collector::CollectionStats;
use crate::collect::linear::{LinearClient, LinearIssue};
use crate::core::config::Config;
use crate::core::db::{Database, WorkItemRow};

/// `work_items.source` tag for every row this pipeline writes.
///
/// #5219: the source vocabulary is fixed by `sql/0005_work_items.sql:8`
/// (`'azdo' | 'jira' | 'github' | 'linear'`); DOC-70 §11 reuses it as the
/// provider tag of its `{provider, id}` board selection.
const LINEAR_SOURCE: &str = "linear";

/// `work_items.item_type` for a Linear issue.
///
/// Linear has no per-issue type field on the GraphQL surface this crate
/// queries, and the column is `NOT NULL`, so every row carries this constant.
const LINEAR_ITEM_TYPE: &str = "Issue";

/// Fetch and persist the Linear issues referenced by collected commits.
///
/// Why: Linear identifiers appear in commit messages, and the issue's state and
/// team are what turn a bare `ENG-123` into something a report can group by.
/// What: no-ops unless `linear.fetch_on_reference` is set. Otherwise collects
/// every `(sha, message)` pair, calls `fetch_referenced_issues`, and stores the
/// result into BOTH `linear_issues` and the source-agnostic `work_items` /
/// `commit_work_items` pair. Every failure — client init, query, fetch, either store —
/// is recorded on `stats` as a stage failure and returns without aborting the
/// surrounding run. The command turns that into a non-zero exit at the end
/// (#5655); nothing here decides the exit code itself.
/// Test: `persist_work_items_writes_linear_rows`,
/// `persist_work_items_links_commits`, `persist_work_items_is_idempotent`,
/// `persist_work_items_reports_write_failure`,
/// `linear_stage_faults_are_recorded_as_stage_failures`; the client half is
/// unit-tested in `crate::collect::linear`.
pub(super) async fn fetch_and_store_linear_issues(
    db: &mut Database,
    config: &Config,
    stats: &mut CollectionStats,
) {
    // Optional: Linear issue enrichment.
    if let Some(linear_cfg) = &config.linear {
        if linear_cfg.fetch_on_reference {
            match LinearClient::new(linear_cfg) {
                Ok(client) => {
                    // #5219: `sha` joins the fetch to `commit_work_items`; the
                    // pre-fix query read `message` alone, so no commit link
                    // could be written even once the issues were known.
                    let commits: Vec<(String, String)> = {
                        let conn = db.connection();
                        let mut stmt = match conn.prepare("SELECT sha, message FROM commits") {
                            Ok(s) => s,
                            Err(e) => {
                                stats.fail_stage(format!("Linear: query commits failed: {e}"));
                                return;
                            }
                        };
                        let rows = match stmt.query_map([], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        }) {
                            Ok(r) => r,
                            Err(e) => {
                                stats.fail_stage(format!("Linear: read commits failed: {e}"));
                                return;
                            }
                        };
                        let mut out = Vec::new();
                        for r in rows.flatten() {
                            out.push(r);
                        }
                        out
                    };

                    let messages: Vec<String> =
                        commits.iter().map(|(_, msg)| msg.clone()).collect();
                    let msg_refs: Vec<&str> = messages.iter().map(String::as_str).collect();
                    // #5665: a rejected API key used to arrive here as an empty
                    // vec, so the run wrote zero rows and reported nothing.
                    let issues = match client
                        .fetch_referenced_issues(&msg_refs, &linear_cfg.team_keys)
                        .await
                    {
                        Ok(issues) => issues,
                        Err(e) => {
                            // #5727: the whole stage's data is absent — the walk
                            // stops at the first failure and nothing is written.
                            stats.fail_stage(format!("Linear: fetch issues failed: {e}"));
                            return;
                        }
                    };
                    for issue in &issues {
                        info!(
                            id = %issue.identifier,
                            state = %issue.state,
                            team = %issue.team,
                            "Linear issue fetched"
                        );
                    }
                    match client.store_issues(db, &issues) {
                        Ok(n) => {
                            info!(stored = n, "persisted linear_issues rows");
                            stats.linear_issues_fetched += n;
                        }
                        Err(e) => {
                            stats.fail_stage(format!("Linear: store issues failed: {e}"));
                        }
                    }

                    // #5219: the same issues also land in the source-agnostic
                    // `work_items` corpus, which is what correlation and
                    // DOC-70's board axis read.
                    let commit_refs = build_commit_refs(&commits);
                    match persist_work_items(db, &issues, &commit_refs) {
                        Ok(n) => {
                            info!(
                                stored = n,
                                source = LINEAR_SOURCE,
                                "persisted work_items rows"
                            );
                        }
                        Err(e) => {
                            // #5655: this Err is why the exit code exists — a
                            // dropped work_items write must not report success.
                            stats.fail_stage(format!("Linear: store work_items failed: {e}"));
                        }
                    }
                }
                Err(e) => {
                    stats.fail_stage(format!("Linear client init failed: {e}"));
                }
            }
        }
    }
}

/// Map each commit SHA to the Linear identifiers its message mentions.
///
/// Why: `commit_work_items` is a per-commit join, so the identifiers have to
/// stay attached to the SHA they came from rather than being flattened into one
/// global set the way the fetch path does it.
/// What: runs [`LinearClient::extract_issue_ids`] — the same extractor the
/// fetch uses, so a commit links to exactly the identifiers that fetch could
/// have resolved — over each message, dropping commits that mention none.
/// Test: `persist_work_items_links_commits`.
fn build_commit_refs(commits: &[(String, String)]) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    for (sha, message) in commits {
        let ids = LinearClient::extract_issue_ids(message);
        if !ids.is_empty() {
            out.insert(sha.clone(), ids);
        }
    }
    out
}

/// Project fetched Linear issues into `work_items` and link the commits that
/// reference them.
///
/// Why: only Azure DevOps wrote `work_items` in production (#5219), so every
/// consumer of the source-agnostic corpus — commit correlation, and DOC-70's
/// board axis, which defines its corpus as the rows with `source = 'linear'` —
/// saw nothing for Linear no matter how much `linear_issues` held.
/// What: upserts one `work_items` row per issue under `source = 'linear'`,
/// with `id` = [`LinearIssue::identifier`] so it matches what
/// [`LinearClient::extract_issue_ids`] yields and `stable_id` = Linear's issue
/// id. Since #190 step 3 the row is found by that id, so a moved issue's row
/// is renamed rather than duplicated, and its links follow it
/// ([`crate::collect::linear::projection::write_work_item`]). `tags` carries
/// the label names. Then inserts a `commit_work_items` row for every
/// `(sha, identifier)` pair whose identifier — current or one the issue held
/// before a move — was actually fetched. Both halves run in one transaction,
/// so a failure leaves neither table half-written and no dangling join row.
/// An identifier a commit mentions but Linear did not return is skipped,
/// because the join table's foreign key would reject it. Returns the number
/// of `work_items` rows written.
///
/// `project` is left `None`: the issue query carries only the project id since
/// #190, and DOC-70 §6 routes project resolution through trusty-common's
/// client instead. Writing the team name there would fill the column DOC-70
/// filters on with a value that is not a project.
///
/// # Errors
///
/// Returns [`crate::core::TgaError::DbError`] if opening the transaction, any
/// upsert, any link insert, or the commit fails. The error is returned, never
/// downgraded to a warning or a zero count — the caller records it in
/// `stats.errors`.
///
/// Test: `persist_work_items_writes_linear_rows`,
/// `persist_work_items_links_commits`, `persist_work_items_is_idempotent`,
/// `persist_work_items_skips_unfetched_refs`,
/// `persist_work_items_reports_write_failure`,
/// `persist_work_items_rolls_back_a_partial_write`,
/// `moved_issue_keeps_one_work_items_row_and_its_links`,
/// `commit_naming_a_previous_identifier_links_the_moved_issue`,
/// `work_item_tags_carry_the_labels`.
pub fn persist_work_items(
    db: &mut Database,
    issues: &[LinearIssue],
    commit_refs: &HashMap<String, Vec<String>>,
) -> crate::core::Result<usize> {
    if issues.is_empty() {
        return Ok(0);
    }
    let tx = db.connection_mut().transaction()?;
    let written = persist_work_items_in(&tx, issues, commit_refs)?;
    tx.commit()?;
    Ok(written)
}

/// [`persist_work_items`] without its own transaction: the caller owns it.
///
/// Why: #190 — `tga linear sync` commits its `linear_issues` rows and their
/// `work_items` projection in one transaction, so neither can land alone.
/// What: the same upserts and links as [`persist_work_items`], run on `conn`,
/// which should be an open transaction; nothing commits here.
/// Test: `commands::linear::sync_tests::failed_work_items_write_is_repaired_by_the_next_sync`.
///
/// # Errors
///
/// As [`persist_work_items`]. The caller's transaction is left open; drop it
/// to roll back.
pub fn persist_work_items_in(
    conn: &rusqlite::Connection,
    issues: &[LinearIssue],
    commit_refs: &HashMap<String, Vec<String>>,
) -> crate::core::Result<usize> {
    use crate::collect::linear::projection::{identifier_aliases, write_work_item};
    use crate::core::db::work_items::link_commit_work_item;

    let mut written = 0usize;
    for issue in issues {
        let row = WorkItemRow {
            id: issue.identifier.clone(),
            source: LINEAR_SOURCE.to_string(),
            title: issue.title.clone(),
            status: issue.state.clone(),
            item_type: LINEAR_ITEM_TYPE.to_string(),
            // #190 step 3: label names, comma-separated like every provider's.
            tags: (!issue.label_names.is_empty()).then(|| issue.label_names.join(",")),
            project: None,
            url: Some(issue.url.clone()),
            // #190: the GraphQL node as Linear returned it, so the
            // Linear-aware effort and work extractors find `estimate`,
            // `parent`, `creator` and `description` under Linear's own names.
            raw_json: issue.raw_json(),
        };
        // #190 step 3: keyed by the Linear id, so a moved issue keeps one row.
        write_work_item(conn, &row, issue)?;
        written += 1;
    }

    // #190 step 3: a commit naming an identifier the issue held before a
    // move links to the issue under its current identifier.
    let aliases = identifier_aliases(issues);
    for (sha, ids) in commit_refs {
        for id in ids {
            if let Some(current) = aliases.get(id.as_str()) {
                link_commit_work_item(conn, sha, current, LINEAR_SOURCE)?;
            }
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(identifier: &str) -> LinearIssue {
        LinearIssue {
            identifier: identifier.to_string(),
            title: format!("Title for {identifier}"),
            state: "In Progress".to_string(),
            team: "Engineering".to_string(),
            assignee: Some("Alice".to_string()),
            priority: 2,
            url: format!("https://linear.app/x/issue/{identifier}"),
            ..Default::default()
        }
    }

    fn linear_row_count(db: &Database) -> i64 {
        db.connection()
            .query_row(
                "SELECT COUNT(*) FROM work_items WHERE source = 'linear'",
                [],
                |r| r.get(0),
            )
            .expect("count work_items")
    }

    /// The #5219 regression: before the fix nothing ever wrote a `work_items`
    /// row with `source = 'linear'`, so this count stayed at 0 for every run.
    #[test]
    fn persist_work_items_writes_linear_rows() {
        let mut db = Database::open_in_memory().expect("db");
        let n = persist_work_items(&mut db, &[issue("ENG-1"), issue("FE-42")], &HashMap::new())
            .expect("persist");
        assert_eq!(n, 2);
        assert_eq!(linear_row_count(&db), 2);

        let (title, status, item_type, url): (String, String, String, Option<String>) = db
            .connection()
            .query_row(
                "SELECT title, status, item_type, url FROM work_items \
                 WHERE id = ?1 AND source = 'linear'",
                ["ENG-1"],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("query row");
        assert_eq!(title, "Title for ENG-1");
        assert_eq!(status, "In Progress");
        assert_eq!(item_type, "Issue");
        assert_eq!(url.as_deref(), Some("https://linear.app/x/issue/ENG-1"));
    }

    #[test]
    fn persist_work_items_links_commits() {
        let mut db = Database::open_in_memory().expect("db");
        let commits = vec![
            ("sha1".to_string(), "ENG-1: add login".to_string()),
            ("sha2".to_string(), "chore: no ticket here".to_string()),
        ];
        let refs = build_commit_refs(&commits);
        assert_eq!(refs.len(), 1, "only the ticketed commit contributes a ref");

        persist_work_items(&mut db, &[issue("ENG-1")], &refs).expect("persist");

        let linked: Vec<String> = {
            let conn = db.connection();
            let mut stmt = conn
                .prepare(
                    "SELECT commit_sha FROM commit_work_items \
                     WHERE work_item_id = ?1 AND work_item_source = 'linear'",
                )
                .expect("prepare");
            let rows = stmt
                .query_map(["ENG-1"], |r| r.get::<_, String>(0))
                .expect("query");
            rows.flatten().collect()
        };
        assert_eq!(linked, vec!["sha1".to_string()]);
    }

    /// An identifier the extractor found but Linear never returned must not
    /// reach the join table — its foreign key would reject it and abort the
    /// whole transaction, losing the rows that were fine.
    #[test]
    fn persist_work_items_skips_unfetched_refs() {
        let mut db = Database::open_in_memory().expect("db");
        let commits = vec![("sha1".to_string(), "ENG-1 and GONE-9".to_string())];
        let refs = build_commit_refs(&commits);

        persist_work_items(&mut db, &[issue("ENG-1")], &refs).expect("persist");

        let links: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM commit_work_items", [], |r| r.get(0))
            .expect("count links");
        assert_eq!(links, 1, "only the fetched identifier is linked");
    }

    #[test]
    fn persist_work_items_is_idempotent() {
        let mut db = Database::open_in_memory().expect("db");
        let commits = vec![("sha1".to_string(), "ENG-1: work".to_string())];
        let refs = build_commit_refs(&commits);

        persist_work_items(&mut db, &[issue("ENG-1")], &refs).expect("first");
        let mut updated = issue("ENG-1");
        updated.state = "Done".to_string();
        persist_work_items(&mut db, &[updated], &refs).expect("second");

        assert_eq!(linear_row_count(&db), 1);
        let status: String = db
            .connection()
            .query_row(
                "SELECT status FROM work_items WHERE id = ?1 AND source = 'linear'",
                ["ENG-1"],
                |r| r.get(0),
            )
            .expect("query status");
        assert_eq!(
            status, "Done",
            "re-running refreshes rather than duplicates"
        );

        let links: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM commit_work_items", [], |r| r.get(0))
            .expect("count links");
        assert_eq!(links, 1);
    }

    /// The fail-open arm: a write failure must surface as `Err`, never as a
    /// silent `Ok(0)` that lets the run report success with an empty corpus.
    #[test]
    fn persist_work_items_reports_write_failure() {
        let mut db = Database::open_in_memory().expect("db");
        db.connection()
            .execute("DROP TABLE work_items", [])
            .expect("drop table");

        let err = persist_work_items(&mut db, &[issue("ENG-1")], &HashMap::new())
            .expect_err("write against a missing table must fail");
        assert!(
            err.to_string().contains("work_items"),
            "error names the failing table: {err}"
        );
    }

    /// The interleaving the transaction exists for: the issue upsert succeeds,
    /// then the link insert fails. Dropping only `commit_work_items` reaches
    /// the link phase with a `work_items` row already written inside the open
    /// transaction, so the assertion is about rollback rather than about the
    /// error propagating.
    #[test]
    fn persist_work_items_rolls_back_a_partial_write() {
        let mut db = Database::open_in_memory().expect("db");
        db.connection()
            .execute("DROP TABLE commit_work_items", [])
            .expect("drop join table");

        let commits = vec![("sha1".to_string(), "ENG-1: work".to_string())];
        let refs = build_commit_refs(&commits);

        let err = persist_work_items(&mut db, &[issue("ENG-1")], &refs)
            .expect_err("link against a missing table must fail");
        assert!(
            err.to_string().contains("commit_work_items"),
            "error names the failing table: {err}"
        );
        assert_eq!(
            linear_row_count(&db),
            0,
            "the issue upsert that already succeeded must roll back with the transaction"
        );
    }

    /// #5655: every fault this pipeline records is a stage failure — each arm
    /// means the Linear corpus is absent, not that one issue was skipped. The
    /// dropped-`commits` query is the arm reachable without a Linear API round
    /// trip; `store work_items failed` is built by the same constructor.
    #[tokio::test]
    async fn linear_stage_faults_are_recorded_as_stage_failures() {
        use crate::core::config::LinearConfig;

        let mut db = Database::open_in_memory().expect("db");
        db.connection()
            .execute("DROP TABLE commits", [])
            .expect("drop commits");

        let config = Config {
            linear: Some(LinearConfig {
                api_key: Some("test-key".to_string()),
                fetch_on_reference: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut stats = CollectionStats::default();
        fetch_and_store_linear_issues(&mut db, &config, &mut stats).await;

        assert_eq!(stats.errors.len(), 1, "one fault: {:?}", stats.errors);
        assert_eq!(
            stats.stage_failures().len(),
            1,
            "a Linear stage that never wrote must reach the exit code: {:?}",
            stats.errors
        );
    }

    /// An issue parsed from a GraphQL node, as the sync and the per-commit
    /// lookup see it: `lid` is Linear's id, `previous` its
    /// `previousIdentifiers`.
    fn node_issue(lid: &str, identifier: &str, team: &str, previous: &[&str]) -> LinearIssue {
        let mut node = crate::collect::linear::issue::tests::full_node(lid, identifier, team);
        node["previousIdentifiers"] = serde_json::json!(previous);
        crate::collect::linear::issue::parse_issue_node("", &node)
    }

    /// `(id, commit_sha)` for every Linear work item and its links.
    fn linear_ids_and_links(db: &Database) -> (Vec<String>, Vec<(String, String)>) {
        let conn = db.connection();
        let ids = conn
            .prepare("SELECT id FROM work_items WHERE source = 'linear' ORDER BY id")
            .expect("prepare")
            .query_map([], |r| r.get::<_, String>(0))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("ids");
        let links = conn
            .prepare(
                "SELECT commit_sha, work_item_id FROM commit_work_items \
                 WHERE work_item_source = 'linear' ORDER BY commit_sha",
            )
            .expect("prepare")
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("links");
        (ids, links)
    }

    /// #190 step 3: an issue that moves team (same Linear id, new identifier)
    /// keeps ONE `work_items` row, now under its new identifier, and the
    /// commit links follow it. Keyed by identifier alone, the move wrote a
    /// second row and left the links on the stale one. The node carries no
    /// `previousIdentifiers`, so only the stored Linear id can find the row.
    #[test]
    fn moved_issue_keeps_one_work_items_row_and_its_links() {
        let mut db = Database::open_in_memory().expect("db");
        let refs = build_commit_refs(&[("sha1".to_string(), "ENG-5: work".to_string())]);
        persist_work_items(&mut db, &[node_issue("uuid-5", "ENG-5", "ENG", &[])], &refs)
            .expect("first");

        persist_work_items(
            &mut db,
            &[node_issue("uuid-5", "OPS-12", "OPS", &[])],
            &HashMap::new(),
        )
        .expect("move");

        let (ids, links) = linear_ids_and_links(&db);
        assert_eq!(ids, vec!["OPS-12".to_string()]);
        assert_eq!(links, vec![("sha1".to_string(), "OPS-12".to_string())]);
    }

    /// #190 step 3: a row written before the Linear id was stored, under an
    /// identifier the issue has since left, is merged into the issue's row —
    /// its commit links and its `fact_pm_work` row move with it.
    #[test]
    fn legacy_work_item_under_a_previous_identifier_is_merged() {
        use crate::core::db::work_items::{link_commit_work_item, upsert_work_item};
        let mut db = Database::open_in_memory().expect("db");
        let mut legacy = issue("ENG-5");
        legacy.title = "Legacy row".to_string();
        let row = WorkItemRow {
            id: "ENG-5".to_string(),
            source: LINEAR_SOURCE.to_string(),
            title: legacy.title.clone(),
            status: legacy.state.clone(),
            item_type: LINEAR_ITEM_TYPE.to_string(),
            tags: None,
            project: None,
            url: Some(legacy.url.clone()),
            raw_json: legacy.raw_json(),
        };
        upsert_work_item(db.connection(), &row).expect("legacy work item");
        link_commit_work_item(db.connection(), "sha1", "ENG-5", LINEAR_SOURCE).expect("link");
        db.connection()
            .execute(
                "INSERT INTO fact_pm_work (work_item_id, work_item_source, is_meaningful, \
                 exclusion_reason, title_word_count, body_word_count, computed_at) \
                 VALUES ('ENG-5', 'linear', 1, 'NONE', 2, 0, 0)",
                [],
            )
            .expect("legacy fact row");

        persist_work_items(
            &mut db,
            &[node_issue("uuid-5", "OPS-12", "OPS", &["ENG-5"])],
            &HashMap::new(),
        )
        .expect("sync of the moved issue");

        let (ids, links) = linear_ids_and_links(&db);
        assert_eq!(ids, vec!["OPS-12".to_string()]);
        assert_eq!(links, vec![("sha1".to_string(), "OPS-12".to_string())]);
        let fact: String = db
            .connection()
            .query_row("SELECT work_item_id FROM fact_pm_work", [], |r| r.get(0))
            .expect("fact row");
        assert_eq!(fact, "OPS-12");
    }

    /// #190 step 3: a commit that names an issue by an identifier it has
    /// since left links to the issue. Linear resolves the old identifier to
    /// the moved issue, and the link used to be dropped because the fetched
    /// identifier differed from the one the commit named.
    #[test]
    fn commit_naming_a_previous_identifier_links_the_moved_issue() {
        let mut db = Database::open_in_memory().expect("db");
        let refs = build_commit_refs(&[("sha9".to_string(), "ENG-5: old name".to_string())]);
        persist_work_items(
            &mut db,
            &[node_issue("uuid-5", "OPS-12", "OPS", &["ENG-5"])],
            &refs,
        )
        .expect("persist");
        let (_, links) = linear_ids_and_links(&db);
        assert_eq!(links, vec![("sha9".to_string(), "OPS-12".to_string())]);
    }

    /// #190 step 3 (Fail-Open): one identifier claimed by two Linear ids is
    /// an error, and the row already stored is not overwritten.
    #[test]
    fn identifier_held_by_another_linear_id_errors() {
        let mut db = Database::open_in_memory().expect("db");
        persist_work_items(
            &mut db,
            &[node_issue("uuid-a", "ENG-1", "ENG", &[])],
            &HashMap::new(),
        )
        .expect("first issue");
        let mut other = node_issue("uuid-b", "ENG-1", "ENG", &[]);
        other.title = "A different issue".to_string();

        let err = persist_work_items(&mut db, &[other], &HashMap::new())
            .expect_err("a second Linear id under one identifier must fail");
        assert!(err.to_string().contains("ENG-1"), "{err}");
        let title: String = db
            .connection()
            .query_row(
                "SELECT title FROM work_items WHERE id = 'ENG-1' AND source = 'linear'",
                [],
                |r| r.get(0),
            )
            .expect("row");
        assert_eq!(title, "Title for ENG-1");
    }

    /// #190 step 3 (Fail-Open): a merge that fails part-way rolls the whole
    /// batch back; the stale row and its link stay where they were.
    #[test]
    fn failed_merge_rolls_back() {
        let mut db = Database::open_in_memory().expect("db");
        let refs = build_commit_refs(&[("sha1".to_string(), "ENG-5: work".to_string())]);
        persist_work_items(&mut db, &[node_issue("uuid-5", "ENG-5", "ENG", &[])], &refs)
            .expect("first");
        db.connection()
            .execute_batch(
                "CREATE TEMP TRIGGER fail_delete BEFORE DELETE ON work_items \
                 BEGIN SELECT RAISE(ABORT, 'injected delete failure'); END;",
            )
            .expect("trigger");

        let err = persist_work_items(
            &mut db,
            &[node_issue("uuid-5", "OPS-12", "OPS", &[])],
            &HashMap::new(),
        )
        .expect_err("a failed merge must fail the write");
        assert!(err.to_string().contains("injected"), "{err}");
        let (ids, links) = linear_ids_and_links(&db);
        assert_eq!(ids, vec!["ENG-5".to_string()]);
        assert_eq!(links, vec![("sha1".to_string(), "ENG-5".to_string())]);
    }

    /// #190 step 3: `work_items.tags` carries the issue's label names, the
    /// way the other providers' rows carry theirs.
    #[test]
    fn work_item_tags_carry_the_labels() {
        let mut db = Database::open_in_memory().expect("db");
        persist_work_items(
            &mut db,
            &[node_issue("uuid-1", "ENG-1", "ENG", &[])],
            &HashMap::new(),
        )
        .expect("persist");
        let tags: Option<String> = db
            .connection()
            .query_row(
                "SELECT tags FROM work_items WHERE id = 'ENG-1' AND source = 'linear'",
                [],
                |r| r.get(0),
            )
            .expect("row");
        assert_eq!(tags.as_deref(), Some("bug,api"));
    }

    /// #190 step 3 (Architect ruling): `work_items.project` holds the Linear
    /// project id — the name is reached through `linear_projects` — and every
    /// Linear row's `item_type` is `Issue`.
    #[test]
    fn work_item_carries_the_project_id_and_issue_type() {
        let mut db = Database::open_in_memory().expect("db");
        let mut no_project = node_issue("uuid-2", "ENG-2", "ENG", &[]);
        no_project.project_id = None;
        persist_work_items(
            &mut db,
            &[node_issue("uuid-1", "ENG-1", "ENG", &[]), no_project],
            &HashMap::new(),
        )
        .expect("persist");
        let rows: Vec<(String, Option<String>, String)> = db
            .connection()
            .prepare(
                "SELECT id, project, item_type FROM work_items \
                 WHERE source = 'linear' ORDER BY id",
            )
            .expect("prepare")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .collect::<std::result::Result<_, _>>()
            .expect("rows");
        assert_eq!(
            rows,
            vec![
                ("ENG-1".into(), Some("project-1".into()), "Issue".into()),
                ("ENG-2".into(), None, "Issue".into()),
            ]
        );
    }

    #[test]
    fn persist_work_items_no_issues_is_a_noop() {
        let mut db = Database::open_in_memory().expect("db");
        assert_eq!(
            persist_work_items(&mut db, &[], &HashMap::new()).expect("persist"),
            0
        );
        assert_eq!(linear_row_count(&db), 0);
    }
}
