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

/// Lower fuzzy-match threshold applied to *normalized* comparisons (email
/// local-part vs canonical name with punctuation stripped). The normalization
/// step removes a lot of cosmetic differences, so we accept a slightly
/// lower raw similarity score when matching on the normalized form.
pub const NORMALIZED_SIMILARITY_THRESHOLD: f64 = 0.82;

/// Normalize a string for fuzzy comparison by:
/// 1. Lowercasing
/// 2. Replacing `.`, `-`, `_` with spaces (common email/login separators)
/// 3. Collapsing repeated whitespace
///
/// Examples:
/// - `"Bob.Matsuoka"` → `"bob matsuoka"`
/// - `"alice_smith-c"` → `"alice smith c"`
/// - `"Bob   M"`       → `"bob m"`
fn normalize_for_fuzzy(s: &str) -> String {
    s.to_lowercase()
        .replace(['.', '-', '_'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extract the local-part (before `@`) of an email address, lowercased.
/// Returns the whole input lowercased if no `@` is present.
fn email_local_part(email: &str) -> String {
    match email.find('@') {
        Some(i) => email[..i].to_lowercase(),
        None => email.to_lowercase(),
    }
}

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

    /// Register an alias → canonical-name mapping after construction.
    ///
    /// Used by external-system ingestion helpers (e.g.
    /// [`crate::collect::azdo::feed_azdo_users`]) to seed the resolver with
    /// directory-derived identities discovered at runtime. Aliases are
    /// stored lowercased; subsequent [`Self::resolve`] calls treat the
    /// canonical name as authoritative.
    ///
    /// If `canonical_name` matches an existing canonical name on a member
    /// in `members`, `resolve()` will return that member's
    /// canonical email. Otherwise the canonical name is preserved but no
    /// canonical email is registered (callers can resolve by name only).
    ///
    /// Empty `alias` or `canonical_name` values are ignored.
    ///
    /// If `canonical_name` is not already known as a member, a synthetic
    /// member entry is registered with the alias as its canonical email
    /// (if the alias looks like an email — i.e. contains `@`) so that
    /// [`Self::resolve`] can return the canonical pair. If no existing
    /// member is found and the alias is not an email, the synthetic
    /// member is registered with an empty email.
    pub fn add_alias(&mut self, alias: &str, canonical_name: &str) {
        let alias = alias.trim();
        let canonical = canonical_name.trim();
        if alias.is_empty() || canonical.is_empty() {
            return;
        }
        self.aliases
            .insert(alias.to_lowercase(), canonical.to_string());
        if self.find_member_by_name(canonical).is_none() {
            let canonical_email = if alias.contains('@') {
                alias.to_string()
            } else {
                String::new()
            };
            self.members.push((canonical.to_string(), canonical_email));
        }
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

        // 3. Fuzzy match against member names/emails (raw Jaro-Winkler).
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

        // 4. Normalized fuzzy: compare the email local-part and the raw name
        //    against canonical names and member emails after stripping
        //    punctuation (`.`, `-`, `_`). This catches cases like
        //    `"Bob M" <bob.matsuoka@co.com>` → `"Bob Matsuoka"`, where the
        //    raw name is too short for Jaro-Winkler to clear 0.85 but the
        //    email local-part `bob.matsuoka` normalizes to `bob matsuoka`,
        //    which is an exact match for the canonical name.
        let name_norm = normalize_for_fuzzy(name);
        let local_norm = normalize_for_fuzzy(&email_local_part(email));
        let mut best_norm: Option<(f64, &(String, String))> = None;
        for m in &self.members {
            let canon_name_norm = normalize_for_fuzzy(&m.0);
            let canon_local_norm = normalize_for_fuzzy(&email_local_part(&m.1));
            // Try all pairings; take the best score for this member.
            let candidates = [
                jaro_winkler(&local_norm, &canon_name_norm),
                jaro_winkler(&local_norm, &canon_local_norm),
                jaro_winkler(&name_norm, &canon_name_norm),
                jaro_winkler(&name_norm, &canon_local_norm),
            ];
            let score = candidates.iter().cloned().fold(0.0_f64, f64::max);
            if score >= NORMALIZED_SIMILARITY_THRESHOLD
                && best_norm.map(|(b, _)| score > b).unwrap_or(true)
            {
                best_norm = Some((score, m));
            }
        }
        if let Some((score, m)) = best_norm {
            debug!(score, member = %m.0, "normalized fuzzy identity match");
            return (m.0.clone(), m.1.clone());
        }

        // 5. Fallback: return as-is.
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

    /// All aliases — emails AND non-email login handles — must be indexed
    /// in the lookup map so every variant resolves to the canonical name.
    #[test]
    fn all_aliases_registered() {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        map.insert(
            "Alice Smith".to_string(),
            vec![
                "alice@company.com".into(),
                "alice.smith@personal.com".into(),
                "asmith".into(), // non-email login handle
            ],
        );
        let r = IdentityResolver::from_alias_map(&map);

        // Primary email → canonical name + primary email.
        let (n, e) = r.resolve("whoever", "alice@company.com");
        assert_eq!(n, "Alice Smith");
        assert_eq!(e, "alice@company.com");

        // Secondary email → canonical name + canonical (first) email.
        let (n, e) = r.resolve("whoever", "alice.smith@personal.com");
        assert_eq!(n, "Alice Smith");
        assert_eq!(e, "alice@company.com");

        // Non-email handle as name → canonical name.
        let (n, e) = r.resolve("asmith", "noise@nowhere.test");
        assert_eq!(n, "Alice Smith");
        assert_eq!(e, "alice@company.com");
    }

    /// Email local-part fuzzy: a short raw name like `"Bob M"` plus an
    /// email `<bob.matsuoka@co.com>` should resolve to `"Bob Matsuoka"`
    /// via the normalized fuzzy pass even when raw Jaro-Winkler on the
    /// short name falls below the strict 0.85 threshold.
    #[test]
    fn email_local_part_fuzzy_match() {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        map.insert(
            "Bob Matsuoka".to_string(),
            vec!["bob.matsuoka@duettoresearch.com".into()],
        );
        let r = IdentityResolver::from_alias_map(&map);

        // Different email + truncated name — only the email local-part
        // normalizes to "bob matsuoka" which matches the canonical name.
        let (n, e) = r.resolve("Bob M", "bob.matsuoka@otherdomain.com");
        assert_eq!(n, "Bob Matsuoka");
        assert_eq!(e, "bob.matsuoka@duettoresearch.com");
    }

    /// Email case must not affect lookup — `ALICE@COMPANY.COM` resolves
    /// the same as `alice@company.com`.
    #[test]
    fn case_insensitive_email_lookup() {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        map.insert("Alice Smith".to_string(), vec!["alice@company.com".into()]);
        let r = IdentityResolver::from_alias_map(&map);

        let (n, e) = r.resolve("Whoever", "ALICE@COMPANY.COM");
        assert_eq!(n, "Alice Smith");
        assert_eq!(e, "alice@company.com");

        let (n2, e2) = r.resolve("WhoEver", "Alice@Company.Com");
        assert_eq!(n2, "Alice Smith");
        assert_eq!(e2, "alice@company.com");
    }

    /// Short truncated display names should still fuzzy-match the
    /// canonical form when the email local-part backs them up.
    #[test]
    fn short_name_fuzzy() {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        map.insert(
            "Bob Matsuoka".to_string(),
            vec!["bob.matsuoka@co.com".into()],
        );
        let r = IdentityResolver::from_alias_map(&map);

        // The unknown email forces fuzzy. "bob m" alone is too short for
        // raw Jaro-Winkler to clear 0.85 against "bob matsuoka", but the
        // local-part `bobm` normalizes and the normalized threshold (0.82)
        // accepts the match.
        let (n, _e) = r.resolve("Bob M", "bobm@unknown.test");
        assert_eq!(n, "Bob Matsuoka");
    }

    /// A completely unknown identity returns the raw input unchanged.
    #[test]
    fn unknown_author_passthrough() {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        map.insert("Alice Smith".to_string(), vec!["alice@company.com".into()]);
        let r = IdentityResolver::from_alias_map(&map);

        let (n, e) = r.resolve("Zelda Q", "zelda@nowhere.test");
        assert_eq!(n, "Zelda Q");
        assert_eq!(e, "zelda@nowhere.test");
    }

    /// Multiple distinct emails for the same person all collapse onto a
    /// single canonical name and email pair.
    #[test]
    fn multiple_emails_same_person() {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        map.insert(
            "Andre Ramos".to_string(),
            vec![
                "andre.ramos@duettoresearch.com".into(),
                "129991831+andreramosduetto@users.noreply.github.com".into(),
                "andre@personal.dev".into(),
            ],
        );
        let r = IdentityResolver::from_alias_map(&map);

        let (n1, e1) = r.resolve("Andre Ramos", "andre.ramos@duettoresearch.com");
        let (n2, e2) = r.resolve(
            "andreramosduetto",
            "129991831+andreramosduetto@users.noreply.github.com",
        );
        let (n3, e3) = r.resolve("A. Ramos", "andre@personal.dev");

        assert_eq!(n1, "Andre Ramos");
        assert_eq!(n2, "Andre Ramos");
        assert_eq!(n3, "Andre Ramos");
        // Canonical email is the first email-looking alias for each.
        assert_eq!(e1, "andre.ramos@duettoresearch.com");
        assert_eq!(e2, "andre.ramos@duettoresearch.com");
        assert_eq!(e3, "andre.ramos@duettoresearch.com");
    }

    /// Verify resolution end-to-end through `Config::load` + external
    /// `aliases_file`, mirroring the shape of the deployed
    /// `configs/duetto-contractors.yaml` setup. This guards against
    /// regressions in YAML schema, path resolution, or resolver wiring.
    ///
    /// The fixture is materialized into a temp dir so the test is
    /// hermetic: it does not depend on absolute paths outside the repo
    /// (which would fail in CI).
    #[test]
    fn duetto_contractors_config_resolves() {
        let unique = format!(
            "tga-duetto-contractors-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let tmp = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&tmp).expect("create tmp");

        // Aliases file with a subset of the real Duetto contractor map,
        // including the cases the assertions below exercise (case-folding,
        // fuzzy match on email-local-part, non-email handle alias).
        let aliases_yaml = r#"
developers:
  - name: "Andre Ramos"
    primary_email: "andre.ramos@duettoresearch.com"
    aliases:
      - "129991831+andreramosduetto@users.noreply.github.com"
  - name: "Akash Arora"
    primary_email: "akash.arora@duettoresearch.com"
    aliases:
      - "Akash.Arora-c@duettoresearch.com"
      - "akash-duetto"
  - name: "Janga Vinod Kumar Reddy"
    primary_email: "janga.reddy@duettoresearch.com"
    aliases:
      - "jangareddy-duetto"
      - "164324948+jangareddy-duetto@users.noreply.github.com"
"#;
        let aliases_path = tmp.join("aliases.yaml");
        std::fs::write(&aliases_path, aliases_yaml).expect("write aliases");

        let config_yaml = format!(
            "version: \"1.0\"\naliases_file: \"{}\"\n",
            aliases_path.to_string_lossy()
        );
        let config_path = tmp.join("duetto-contractors.yaml");
        std::fs::write(&config_path, config_yaml).expect("write config");

        let cfg =
            crate::core::config::Config::load(&config_path).expect("load duetto-contractors yaml");
        let r = IdentityResolver::from_config(&cfg);

        // Known mapping from the YAML (canonical email match).
        let (n, _) = r.resolve("whoever", "andre.ramos@duettoresearch.com");
        assert_eq!(n, "Andre Ramos");

        // Case-insensitive variant of an explicitly listed alias.
        let (n, _) = r.resolve("whoever", "Akash.Arora-c@duettoresearch.com");
        assert_eq!(n, "Akash Arora");

        // Non-email login handle alias.
        let (n, _) = r.resolve("jangareddy-duetto", "noise@nowhere.test");
        assert_eq!(n, "Janga Vinod Kumar Reddy");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn normalize_for_fuzzy_basic() {
        assert_eq!(normalize_for_fuzzy("Bob.Matsuoka"), "bob matsuoka");
        assert_eq!(normalize_for_fuzzy("alice_smith-c"), "alice smith c");
        assert_eq!(normalize_for_fuzzy("  Foo   Bar  "), "foo bar");
    }

    #[test]
    fn email_local_part_basic() {
        assert_eq!(email_local_part("Bob@Example.COM"), "bob");
        assert_eq!(email_local_part("no-at-symbol"), "no-at-symbol");
    }
}
