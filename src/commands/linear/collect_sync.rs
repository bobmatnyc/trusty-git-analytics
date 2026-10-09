//! The bulk Linear sync inside `tga collect` and `tga analyze` (#190 step 5).
//!
//! Why: a Linear engagement had to remember a second command, `tga linear
//! sync`, before every report; `collect` refreshed git and the other
//! providers but left the Linear tables as old as the last manual sync.
//! What: [`sync_on_collect`] runs the work `tga linear sync --entities` does
//! when `linear.sync_on_collect` is on (opt-in, owner ruling D8f): every
//! `linear.team_keys` entry, or every team the key can see when the list is
//! empty, then the reference entities. Failures are recorded on the run's
//! [`CollectionStats`] under the #146 (D28) contract: an HTTP 404 is one
//! counted warning, every other failure one stage failure, which makes the
//! command exit non-zero after its remaining stages.
//! Test: `collect_sync_tests`.

use tga::collect::errors::CollectError;
use tga::collect::linear::LinearClient;
use tga::collect::CollectionStats;
use tga::core::config::Config;
use tga::core::db::Database;

use super::entity_sync::sync_entities;
use super::{build_client, run_sync_with, LinearSyncArgs};

/// Prefix of every fault this module records.
const LABEL: &str = "linear sync";

/// What the reference-entity pass is called in a fault message.
const ENTITIES: &str = "reference entities";

/// `linear.sync_on_collect` is set.
fn enabled(config: &Config) -> bool {
    config.linear.as_ref().is_some_and(|l| l.sync_on_collect)
}

/// The HTTP status Linear answered with, from anywhere in `err`'s chain.
fn http_status(err: &anyhow::Error) -> Option<u16> {
    err.chain()
        .filter_map(|e| e.downcast_ref::<CollectError>())
        .find_map(|e| match e {
            CollectError::LinearApi { status, .. }
            | CollectError::LinearBulkApi { status, .. }
            | CollectError::LinearEntityApi { status, .. }
            | CollectError::LinearActivityApi { status, .. }
            | CollectError::Throttled { status, .. } => Some(*status),
            CollectError::Http(h) => h.status().map(|s| s.as_u16()),
            _ => None,
        })
}

/// Run the bulk Linear sync for `tga collect` when `linear.sync_on_collect`
/// is on.
///
/// Why: see the module header. Off is the default and must stay exactly
/// today's collect.
/// What: returns at once, building no client, when the flag is off or the
/// `linear:` block is absent. Otherwise builds the client (a missing key is
/// a stage failure) and runs [`sync_with`]. `dry_run` fetches and reports
/// without writing, as `tga linear sync --dry-run` does.
/// Test: `collect_sync_tests::sync_on_collect_off_makes_no_linear_call`.
pub async fn sync_on_collect(
    config: &Config,
    db: &mut Database,
    dry_run: bool,
    stats: &mut CollectionStats,
) {
    if !enabled(config) {
        return;
    }
    match build_client(config) {
        Ok(client) => sync_with(&client, config, db, dry_run, stats).await,
        Err(e) => stats.fail_stage(format!("{LABEL}: {e:#}")),
    }
}

/// [`sync_on_collect`] with the client supplied.
///
/// Why: a test drives the scope and the failure arms against a mock Linear.
/// What: no request when the flag is off. Otherwise syncs each scope (one
/// per `team_keys` entry, or one all-teams scope) with [`run_sync_with`],
/// then the reference entities. A scope that answers HTTP 404 is tallied
/// and the run continues; the tally becomes one warning. Any other failure
/// records one stage failure naming the scope and its status, and stops the
/// sync: teams already synced keep their rows and cursors.
/// Test: `collect_sync_tests::sync_on_collect_off_makes_no_linear_call`,
/// `collect_sync_tests::sync_on_collect_syncs_the_configured_team_and_the_entities`,
/// `collect_sync_tests::a_linear_sync_auth_or_server_error_is_a_stage_failure`,
/// `collect_sync_tests::a_linear_sync_404_is_a_counted_warning`.
pub(crate) async fn sync_with(
    client: &LinearClient,
    config: &Config,
    db: &mut Database,
    dry_run: bool,
    stats: &mut CollectionStats,
) {
    // #190: checked here too, so no caller can reach Linear with the flag off.
    if !enabled(config) {
        return;
    }
    let team_keys = config
        .linear
        .as_ref()
        .map(|l| l.team_keys.clone())
        .unwrap_or_default();
    let runs: Vec<(String, LinearSyncArgs)> = if team_keys.is_empty() {
        let args = LinearSyncArgs {
            all_teams: true,
            dry_run,
            ..Default::default()
        };
        vec![("all teams".to_string(), args)]
    } else {
        team_keys
            .iter()
            .map(|k| {
                let args = LinearSyncArgs {
                    team: Some(k.clone()),
                    dry_run,
                    ..Default::default()
                };
                (k.clone(), args)
            })
            .collect()
    };
    let total = runs.len() + 1;
    let mut missing = Vec::new();
    for (scope, args) in &runs {
        if let Err(e) = run_sync_with(client, config, db, args).await {
            if !tally(&mut missing, scope, &e, stats) {
                return;
            }
        }
    }
    if let Err(e) = sync_entities(client, db, dry_run).await {
        tally(&mut missing, ENTITIES, &e, stats);
    }
    if !missing.is_empty() {
        stats.skip_item(format!(
            "{LABEL}: {} of {total} sync scope(s) answered HTTP 404 and got no rows: {}. \
             Linear did not find them; check the team keys and the API key's workspace \
             (see #146)",
            missing.len(),
            missing.join(", ")
        ));
    }
}

/// Record one failed scope. A 404 joins `missing` and returns `true` (carry
/// on); anything else is one stage failure and returns `false` (stop).
fn tally(
    missing: &mut Vec<String>,
    scope: &str,
    err: &anyhow::Error,
    stats: &mut CollectionStats,
) -> bool {
    match http_status(err) {
        Some(404) => {
            missing.push(scope.to_string());
            true
        }
        status => {
            let status =
                status.map_or_else(|| "no HTTP status".to_string(), |s| format!("HTTP {s}"));
            stats.fail_stage(format!(
                "{LABEL}: {scope} failed ({status}): {err:#}. The Linear tables were not \
                 refreshed past this point; an HTTP 401 or 403 means the API key is wrong or \
                 lacks access (see #146)"
            ));
            false
        }
    }
}

#[cfg(test)]
#[path = "collect_sync_tests.rs"]
mod tests;
