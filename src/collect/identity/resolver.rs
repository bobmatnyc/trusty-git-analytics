//! Developer identity resolution.
//!
//! Given a raw `(name, email)` tuple observed in a git commit, resolve
//! it to a canonical identity using a three-tier strategy:
//! 1. Exact alias match against the configured aliases map.
//! 2. Fuzzy match against team member canonical emails/names using
//!    Jaro-Winkler similarity above a configurable threshold.
//! 3. Fall through and return the raw pair unchanged.

use std::collections::HashMap;

use rusqlite::params;
use strsim::jaro_winkler;
use tracing::debug;

use crate::core::config::TeamConfig;
use crate::core::db::Database;

/// Default Jaro-Winkler threshold for fuzzy identity matching.
pub const DEFAULT_SIMILARITY_THRESHOLD: f64 = 0.85;

/// Resolves observed author identities to canonical `(name, email)` pairs.
pub struct IdentityResolver {
    /// Mapping of alias (lowercased name or email) → canonical name.
    aliases: HashMap<String, String>,
    /// Canonical members: `(canonical_name, canonical_email)`.
    members: Vec<(String, String)>,
    /// Threshold for accepting a fuzzy match.
    threshold: f64,
}

impl IdentityResolver {
    /// Construct a resolver from a [`TeamConfig`].
    pub fn new(team: Option<&TeamConfig>) -> Self {
        let mut aliases: HashMap<String, String> = HashMap::new();
        let mut members: Vec<(String, String)> = Vec::new();
        if let Some(team) = team {
            for (k, v) in &team.aliases {
                aliases.insert(k.to_lowercase(), v.clone());
            }
            for m in &team.members {
                members.push((m.name.clone(), m.email.clone()));
                for a in &m.aliases {
                    aliases.insert(a.to_lowercase(), m.name.clone());
                }
                // Also auto-register the canonical email as an alias to itself.
                aliases.insert(m.email.to_lowercase(), m.name.clone());
            }
        }
        Self {
            aliases,
            members,
            threshold: DEFAULT_SIMILARITY_THRESHOLD,
        }
    }

    /// Construct a resolver from a flat `canonical_name → [aliases]` map.
    ///
    /// This is the format produced by [`crate::core::config::Config::resolved_aliases`]
    /// and matches the Python predecessor's `developer_aliases` YAML key.
    ///
    /// The first entry in each alias list (if any looks like an email — i.e.
    /// contains `@`) is treated as the canonical email; otherwise the
    /// canonical email is left blank.
    pub fn from_alias_map(map: &HashMap<String, Vec<String>>) -> Self {
        let mut aliases: HashMap<String, String> = HashMap::new();
        let mut members: Vec<(String, String)> = Vec::new();
        for (canon_name, alias_list) in map {
            // Pick the first email-looking alias as canonical email.
            let canon_email = alias_list
                .iter()
                .find(|a| a.contains('@'))
                .cloned()
                .unwrap_or_default();
            members.push((canon_name.clone(), canon_email.clone()));
            // Register canonical name + canonical email as self-aliases.
            aliases.insert(canon_name.to_lowercase(), canon_name.clone());
            if !canon_email.is_empty() {
                aliases.insert(canon_email.to_lowercase(), canon_name.clone());
            }
            for a in alias_list {
                aliases.insert(a.to_lowercase(), canon_name.clone());
            }
        }
        Self {
            aliases,
            members,
            threshold: DEFAULT_SIMILARITY_THRESHOLD,
        }
    }

    /// Construct a resolver from a [`crate::core::config::Config`], preferring
    /// the Python-compatible `developer_aliases` map when present, falling
    /// back to `team.members`.
    pub fn from_config(config: &crate::core::config::Config) -> Self {
        let map = config.resolved_aliases();
        if !map.is_empty() {
            Self::from_alias_map(&map)
        } else {
            Self::new(config.team.as_ref())
        }
    }

    /// Override the fuzzy-match threshold (0.0–1.0).
    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.threshold = threshold;
        self
    }

    /// Resolve a raw `(name, email)` pair to canonical form.
    ///
    /// Returns the input unchanged if no rule matches.
    pub fn resolve(&self, name: &str, email: &str) -> (String, String) {
        let email_lc = email.to_lowercase();
        let name_lc = name.to_lowercase();

        // 1. Exact alias on email
        if let Some(canon_name) = self.aliases.get(&email_lc) {
            if let Some((cn, ce)) = self.find_member_by_name(canon_name) {
                return (cn, ce);
            }
        }
        // 2. Exact alias on name
        if let Some(canon_name) = self.aliases.get(&name_lc) {
            if let Some((cn, ce)) = self.find_member_by_name(canon_name) {
                return (cn, ce);
            }
        }

        // 3. Fuzzy match against member names/emails
        let mut best: Option<(f64, &(String, String))> = None;
        for m in &self.members {
            let s_name = jaro_winkler(&name_lc, &m.0.to_lowercase());
            let s_email = jaro_winkler(&email_lc, &m.1.to_lowercase());
            let score = s_name.max(s_email);
            if score >= self.threshold && best.map(|(b, _)| score > b).unwrap_or(true) {
                best = Some((score, m));
            }
        }
        if let Some((score, m)) = best {
            debug!(score, member = %m.0, "fuzzy identity match");
            return (m.0.clone(), m.1.clone());
        }

        // 4. Fallback: return as-is.
        (name.to_string(), email.to_string())
    }

    /// Upsert an author into the `authors` table, returning the row id.
    ///
    /// Uses `canonical_email` as the natural key.
    ///
    /// # Errors
    ///
    /// Returns [`crate::core::TgaError::DbError`] on SQL failure.
    pub fn upsert_author(
        &self,
        db: &Database,
        name: &str,
        email: &str,
    ) -> crate::core::Result<i64> {
        let (canon_name, canon_email) = self.resolve(name, email);
        let conn = db.connection();
        conn.execute(
            "INSERT INTO authors (canonical_name, canonical_email, aliases) \
             VALUES (?1, ?2, '[]') \
             ON CONFLICT(canonical_email) DO UPDATE SET canonical_name = excluded.canonical_name",
            params![canon_name, canon_email],
        )?;
        let id: i64 = conn.query_row(
            "SELECT id FROM authors WHERE canonical_email = ?1",
            params![canon_email],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    fn find_member_by_name(&self, name: &str) -> Option<(String, String)> {
        self.members
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{TeamConfig, TeamMember};
    use std::collections::HashMap;

    fn make_team() -> TeamConfig {
        let mut aliases = HashMap::new();
        aliases.insert("bobby".into(), "Bob Smith".into());
        TeamConfig {
            members: vec![TeamMember {
                name: "Bob Smith".into(),
                email: "bob@example.com".into(),
                aliases: vec!["bsmith@example.com".into()],
            }],
            aliases,
        }
    }

    #[test]
    fn exact_email_alias_match() {
        let r = IdentityResolver::new(Some(&make_team()));
        let (n, e) = r.resolve("Whoever", "bsmith@example.com");
        assert_eq!(n, "Bob Smith");
        assert_eq!(e, "bob@example.com");
    }

    #[test]
    fn exact_name_alias_match() {
        let r = IdentityResolver::new(Some(&make_team()));
        let (n, e) = r.resolve("bobby", "x@y.com");
        assert_eq!(n, "Bob Smith");
        assert_eq!(e, "bob@example.com");
    }

    #[test]
    fn fuzzy_match_canonical_name() {
        let r = IdentityResolver::new(Some(&make_team()));
        // Slightly different spelling should still match (jaro_winkler high)
        let (n, _e) = r.resolve("Bob Smyth", "unknown@elsewhere.com");
        assert_eq!(n, "Bob Smith");
    }

    #[test]
    fn no_match_returns_input() {
        let r = IdentityResolver::new(Some(&make_team()));
        let (n, e) = r.resolve("Zelda Q", "zelda@nowhere.test");
        assert_eq!(n, "Zelda Q");
        assert_eq!(e, "zelda@nowhere.test");
    }

    #[test]
    fn empty_team_passthrough() {
        let r = IdentityResolver::new(None);
        let (n, e) = r.resolve("Anyone", "anyone@x.com");
        assert_eq!(n, "Anyone");
        assert_eq!(e, "anyone@x.com");
    }
}
