//! The reference-entity pass of `tga linear sync --entities` (#190).
//!
//! Why: issue rows hold only ids; the teams, users, labels, projects,
//! milestones and cycles behind them are synced here, after the issues, so
//! the issue path stays as it was when the flag is off.
//! What: [`sync_entities`] and the [`EntityOutcome`] lines it reports.
//! Test: `super::entity_sync_tests`.

use tga::collect::linear::entities::is_archived_node;
use tga::collect::linear::entities::store::commit_entity_sync;
use tga::collect::linear::{EntityKind, LinearClient};
use tga::core::db::Database;

/// What one entity set's sync fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EntityOutcome {
    /// The set.
    pub kind: EntityKind,
    /// Nodes Linear returned, archived included.
    pub fetched: usize,
    /// How many of them are archived.
    pub archived: usize,
    /// Stored rows newly marked removed because the fetch no longer returned
    /// them; `None` on a dry run, which writes nothing.
    pub removed: Option<usize>,
}

/// The one summary line for a whole entity pass, per-entity counts inline.
pub(crate) fn summary_line(outcomes: &[EntityOutcome], dry_run: bool) -> String {
    let parts: Vec<String> = outcomes
        .iter()
        .map(|o| match o.removed {
            // #190: removed counts per set; a dry run computes none.
            Some(removed) => format!(
                "{} {} ({} archived, {} removed)",
                o.kind.name(),
                o.fetched,
                o.archived,
                removed
            ),
            None => format!("{} {} ({} archived)", o.kind.name(), o.fetched, o.archived),
        })
        .collect();
    format!(
        "Linear entities: {}; {} {} set(s){}.",
        parts.join(", "),
        if dry_run { "would write" } else { "wrote" },
        outcomes.len(),
        if dry_run { " [dry-run: no writes]" } else { "" }
    )
}

/// Fetch and store every entity set, one transaction per set.
///
/// Why: #190 — a page error must not leave a partial set reported as
/// success, and must not move the set's sync state.
/// What: for each [`EntityKind::ALL`] set in order, walks every page into
/// memory, then (unless `dry_run`) writes the rows, marks the stored rows
/// the fetch did not return as removed, and writes the set's
/// `linear_entity_sync_state` row, in one transaction. The first set that
/// fails stops the pass with an error naming it; sets written earlier keep
/// their rows.
/// Test: `super::entity_sync_tests::entity_sync_row_counts_match_the_fixture`,
/// `super::entity_sync_tests::mid_pagination_failure_writes_nothing_and_keeps_the_state`,
/// `super::entity_sync_tests::dry_run_then_real_run_through_the_sync_command`,
/// `super::entity_sync_tests::a_row_linear_stops_returning_is_marked_removed`.
///
/// # Errors
///
/// Linear HTTP/auth and paging errors; database errors.
pub(super) async fn sync_entities(
    client: &LinearClient,
    db: &mut Database,
    dry_run: bool,
) -> anyhow::Result<Vec<EntityOutcome>> {
    let mut outcomes = Vec::with_capacity(EntityKind::ALL.len());
    for kind in EntityKind::ALL {
        let done = outcomes.len();
        let fail = |e: anyhow::Error| {
            e.context(format!(
                "tga linear sync failed syncing Linear {}; nothing was written for it, and \
                 {done} earlier entity set(s) kept their rows",
                kind.name()
            ))
        };
        let nodes = client
            .fetch_entity_nodes(kind)
            .await
            .map_err(|e| fail(e.into()))?;
        let removed = if dry_run {
            None
        } else {
            // #190: the set's rows, its removal marks and its state row
            // commit together.
            let commit = commit_entity_sync(db, kind, &nodes).map_err(|e| fail(e.into()))?;
            Some(commit.removed)
        };
        outcomes.push(EntityOutcome {
            kind,
            fetched: nodes.len(),
            archived: nodes.iter().filter(|n| is_archived_node(n)).count(),
            removed,
        });
    }
    println!("{}", summary_line(&outcomes, dry_run));
    Ok(outcomes)
}
