//! Tier 0: manual classification override lookup.
//!
//! Why: Operators occasionally need to hand-correct a misclassification or
//! pre-seed a verdict for a known commit. A row in `classification_overrides`
//! short-circuits the entire cascade with confidence 1.0, preserving the
//! human judgment regardless of how rules or the LLM behave on later runs.
//!
//! What: Looks up `(commit_sha, repo_path)` in the `classification_overrides`
//! table and constructs a [`ClassificationResult`] with
//! [`ClassificationMethod::Manual`] on hit, returning `None` on miss.
//!
//! Test: Insert a row into the table on an in-memory DB and assert that
//! [`OverrideTier::lookup`] returns the expected category/subcategory pair
//! with confidence 1.0; assert `None` is returned for an unknown SHA.

use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};
use tracing::warn;

use crate::classify::taxonomy::TaxonomyRegistry;
use crate::classify::tiers::ClassificationResult;
use crate::core::models::ClassificationMethod;

/// Tier-0 override lookup. Backed by the `classification_overrides` table.
///
/// The connection is held behind an `Arc<Mutex<_>>` because the tier may be
/// shared across the parallel Rayon batch in
/// [`crate::classify::classifier::ClassificationEngine::classify_batch`].
/// Lookups are short and infrequent (only commits that have an explicit
/// override pay the lock cost) so contention is negligible in practice.
pub struct OverrideTier {
    conn: Arc<Mutex<Connection>>,
    taxonomy: TaxonomyRegistry,
}

impl OverrideTier {
    /// Construct a new tier bound to `conn`.
    ///
    /// Uses the built-in taxonomy registry to resolve the override's
    /// `change_type` back to a [`crate::classify::taxonomy::TopLevelCategory`]. Callers wanting to
    /// honor user-defined subcategories should use [`Self::with_taxonomy`].
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self {
            conn,
            taxonomy: TaxonomyRegistry::with_builtins(),
        }
    }

    /// Construct a new tier with a caller-provided taxonomy registry.
    pub fn with_taxonomy(conn: Arc<Mutex<Connection>>, taxonomy: TaxonomyRegistry) -> Self {
        Self { conn, taxonomy }
    }

    /// Look up a manual override for `(commit_sha, repo_path)`.
    ///
    /// Returns `Some(ClassificationResult)` with confidence 1.0 on hit, or
    /// `None` if no row exists or the DB query fails (failures are logged
    /// at WARN level — they should not abort the cascade).
    pub fn lookup(&self, commit_sha: &str, repo_path: &str) -> Option<ClassificationResult> {
        let guard = match self.conn.lock() {
            Ok(g) => g,
            Err(e) => {
                warn!(error = %e, "override tier mutex poisoned");
                return None;
            }
        };

        let row = guard
            .query_row(
                "SELECT work_type, change_type FROM classification_overrides \
                 WHERE commit_sha = ?1 AND repo_path = ?2",
                params![commit_sha, repo_path],
                |row| {
                    let work_type: String = row.get(0)?;
                    let change_type: String = row.get(1)?;
                    Ok((work_type, change_type))
                },
            )
            .optional();

        match row {
            Ok(Some((work_type, change_type))) => {
                // `work_type` is the subcategory name. `change_type` is the
                // intended top-level. Try to resolve the top-level either
                // from the change_type explicitly, or via the taxonomy
                // registry keyed by work_type.
                let top_level = self
                    .taxonomy
                    .resolve(&change_type)
                    .or_else(|| self.taxonomy.resolve(&work_type));
                Some(ClassificationResult {
                    category: work_type,
                    subcategory: Some(change_type),
                    top_level,
                    confidence: 1.0,
                    method: ClassificationMethod::Manual,
                    ticket_id: None,
                    complexity: None,
                })
            }
            Ok(None) => None,
            Err(e) => {
                warn!(error = %e, commit_sha, "override lookup failed");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_conn() -> Arc<Mutex<Connection>> {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        crate::core::db::migrations::run(&mut conn).expect("run migrations");
        Arc::new(Mutex::new(conn))
    }

    #[test]
    fn lookup_returns_result_on_hit() {
        let conn = fresh_conn();
        conn.lock()
            .expect("lock")
            .execute(
                "INSERT INTO classification_overrides \
                 (commit_sha, repo_path, work_type, change_type) \
                 VALUES (?1, ?2, ?3, ?4)",
                params!["abc123", "/tmp/repo", "feature", "feature"],
            )
            .expect("insert override");

        let tier = OverrideTier::new(conn);
        let r = tier.lookup("abc123", "/tmp/repo").expect("hit");
        assert_eq!(r.category, "feature");
        assert_eq!(r.subcategory.as_deref(), Some("feature"));
        assert!((r.confidence - 1.0).abs() < 1e-9);
        assert_eq!(r.method, ClassificationMethod::Manual);
    }

    #[test]
    fn lookup_returns_none_on_miss() {
        let conn = fresh_conn();
        let tier = OverrideTier::new(conn);
        assert!(tier.lookup("missing", "/tmp/repo").is_none());
    }

    #[test]
    fn lookup_different_repo_misses() {
        let conn = fresh_conn();
        conn.lock()
            .expect("lock")
            .execute(
                "INSERT INTO classification_overrides \
                 (commit_sha, repo_path, work_type, change_type) \
                 VALUES (?1, ?2, ?3, ?4)",
                params!["sha1", "/repo/a", "bugfix", "bugfix"],
            )
            .expect("insert");
        let tier = OverrideTier::new(conn);
        assert!(tier.lookup("sha1", "/repo/b").is_none());
        assert!(tier.lookup("sha1", "/repo/a").is_some());
    }
}
