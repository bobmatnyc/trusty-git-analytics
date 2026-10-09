//! The bulk Linear sync inside `tga collect` and `tga analyze` (#190 step 5).
//!
//! Red-test stub; the implementation follows in the next commit.

use tga::collect::linear::LinearClient;
use tga::collect::CollectionStats;
use tga::core::config::Config;
use tga::core::db::Database;

/// Stub.
pub async fn sync_on_collect(
    _config: &Config,
    _db: &mut Database,
    _dry_run: bool,
    _stats: &mut CollectionStats,
) {
}

/// Stub.
pub(crate) async fn sync_with(
    _client: &LinearClient,
    _config: &Config,
    _db: &mut Database,
    _dry_run: bool,
    _stats: &mut CollectionStats,
) {
}

#[cfg(test)]
#[path = "collect_sync_tests.rs"]
mod tests;
