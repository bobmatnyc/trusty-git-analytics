//! #111: regression tests for the code-critic's WARN on 92cbde0, one per
//! privacy finding. Each fails against 92cbde0. No network; every name is
//! synthetic.

use rusqlite::params;

use super::jev_tests::{
    bodies, classifier, probs, reply, run_pipeline_cfg, run_pipeline_with, server_with, RULES,
};
use super::llm_prompt::LlmOutcome;
use crate::core::db::Database;

fn assert_absent(out: &str, secrets: &[&str]) {
    let lower = out.to_lowercase();
    for s in secrets {
        assert!(
            !lower.contains(&s.to_lowercase()),
            "{s:?} survived in {out:?}"
        );
    }
}

/// An earlier, already-classified commit in `db` with `message`, stored
/// under `repository`.
fn classified_commit(db: &Database, sha: &str, message: &str, repository: &str) {
    let c = db.connection();
    c.execute(
        "INSERT INTO classifications (category, confidence, method) \
         VALUES ('bugfix', 0.9, 'exact')",
        [],
    )
    .expect("classification");
    let cid = c.last_insert_rowid();
    c.execute(
        "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
         repository, classification_id) VALUES (?1, 'Old Author', 'old@corp.test', \
         '2023-01-01T00:00:00Z', ?2, ?3, ?4)",
        params![sha, message, repository, cid],
    )
    .expect("old commit");
}

/// Why (critic HIGH 1): the run learns trailer names only from the commits it
/// classifies, which default to the unclassified ones and narrow further
/// under `--since`, `--repos` or `--shas`. A person named only in a trailer
/// of an older, classified commit was sent in clear when a new message
/// mentioned them.
/// What: the database holds a classified commit whose trailers name the
/// Phabricator reviewers `qzelvin` and `qmarrow` and the co-author `Quorra
/// Bexley <qbexley@corp.test>`; nobody else records them. The run's only
/// commit names all of them in prose. The sent message carries none.
/// Test: this function.
#[tokio::test]
async fn trailer_names_from_stored_commits_are_redacted() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let setup = |db: &Database| {
        classified_commit(
            db,
            "sha-old",
            "fix: cache\n\nReviewers: qzelvin, qmarrow\n\
             Co-authored-by: Quorra Bexley <qbexley@corp.test>",
            "ledger-core",
        );
    };
    let rows = [(
        "zzz follow-up for qzelvin and qmarrow; Quorra Bexley and qbexley agree",
        "b",
        "b@x",
        false,
    )];
    let (_, _db) = run_pipeline_with(&server, &rows, setup).await;
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 1);
    let message = sent[0]["state"]["commit"]["message"]
        .as_str()
        .expect("message");
    assert_absent(message, &["qzelvin", "qmarrow", "quorra", "bexley"]);
    assert!(message.contains("PERSON_"), "{message}");
}

/// Why (critic HIGH 2): repository, org and workspace names came only from
/// `repositories[]`. Every other configured org-like field, and every
/// repository the database stores, names the client too.
/// What: `repositories[].org` is cleared, so `acme-fin` is known only from
/// `github.org`. Names come from `github.org`, `github.orgs`, `github.repo`
/// (both slug parts), `bitbucket.workspace`, `workspaces` and `repo_slug`,
/// the Azure DevOps organisation URL and project list, the Jira site name,
/// a `repo_categories` key, and the `repository` column of `commits` and
/// `pull_requests`. The sent message names none of them; the hyphenated
/// org in prose (`port acme-fin invoicing-api client`) is a `REPO_n`.
/// Test: this function.
#[tokio::test]
async fn org_and_repo_names_from_every_source_are_redacted() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let setup = |db: &Database| {
        classified_commit(db, "sha-old", "fix: cache", "qdb-repo");
        db.connection()
            .execute(
                "INSERT INTO pull_requests (pr_number, title, author, state, created_at, \
                 repository) VALUES (1, 't', 'qprauthor', 'merged', '2023-01-01', 'qpr-repo')",
                [],
            )
            .expect("pull request");
    };
    let tweak = |c: &mut crate::core::config::Config| {
        c.repositories[0].org = None;
        c.github = Some(
            serde_yaml::from_str(
                "org: acme-fin\norgs: [qorvex-ops]\nrepo: acme-fin/invoicing-api\n",
            )
            .expect("github"),
        );
        c.bitbucket = Some(
            serde_yaml::from_str(
                "workspace: qbit-team\nworkspaces: [qbit-two]\nrepo_slug: qslug-svc\n",
            )
            .expect("bitbucket"),
        );
        c.pm = Some(
            serde_yaml::from_str(
                "azure_devops:\n  organization_url: https://dev.azure.com/qazorg\n  \
                 pat: not-a-real-pat\n  projects: [qproj-alpha]\n", // pragma: allowlist secret
            )
            .expect("pm"),
        );
        c.jira =
            Some(serde_yaml::from_str("url: https://qjirasite.atlassian.net\n").expect("jira"));
        let cls = c.classification.as_mut().expect("section");
        cls.repo_categories
            .insert("qcat-repo".into(), "platform".into());
    };
    let names = [
        "acme-fin",
        "invoicing-api",
        "qorvex-ops",
        "qbit-team",
        "qbit-two",
        "qslug-svc",
        "qazorg",
        "qproj-alpha",
        "qjirasite",
        "qcat-repo",
        "qdb-repo",
        "qpr-repo",
    ];
    let text = format!(
        "port acme-fin invoicing-api client; ping {}",
        names[2..].join(", ")
    );
    let rows = [(text.as_str(), "b", "b@x", false)];
    let (_, _db) = run_pipeline_cfg(&server, &rows, RULES, setup, tweak).await;
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 1);
    let message = sent[0]["state"]["commit"]["message"]
        .as_str()
        .expect("message");
    assert_absent(message, &names);
    assert!(message.starts_with("port REPO_"), "{message}");
    assert!(message.contains(" client; ping "), "{message}");
}

/// Why (critic LOW, fail-open path): a panic while the pseudonymizer's lock
/// was held left it poisoned, and `classify` recovered the inner value with
/// `into_inner`, so a half-updated name set could still send.
/// What: poisons the lock of a prepared classifier; the next call is
/// `failed` and nothing is sent, and a later `prepare` is an error.
/// Test: this function.
#[tokio::test]
async fn poisoned_obfuscator_lock_sends_nothing() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let jev = classifier(&server, 1.0);
    jev.poison_obfuscator_lock();
    let call = jev.classify("zzz tidy").await;
    assert_eq!(call.outcome, LlmOutcome::Failed);
    assert!(call.verdict.is_none());
    assert!(bodies(&server).await.is_empty(), "a request was sent");
    assert!(
        jev.prepare(&["zzz"], &[]).is_err(),
        "prepare ran on a poisoned lock"
    );
}
