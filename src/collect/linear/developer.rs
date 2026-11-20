//! Linear identity → tga canonical developer (#272).
//!
//! Why: cto-reports counts closed Linear issues per engineer, keyed on tga's
//! canonical developer (`authors.id`). A Linear assignee carries its own
//! name and email, which can differ from the commit identity tga stored.
//! What: [`DeveloperIndex`] reads the `authors` table once and maps a Linear
//! `(email, name)` — or a provider login — to one `authors` row. It never
//! writes `authors`, so a Linear identity cannot create a second row for a
//! person.
//!
//! Match rules, first hit wins:
//! 1. `email` equals an author's `canonical_email`, ignoring case.
//! 2. `email` is in an author's `aliases` array, ignoring case.
//! 3. The configured identity aliases (`developer_aliases`, `team.members`,
//!    `team.aliases`; exact entries only, never the fuzzy tiers) map the
//!    `(name, email)` to a canonical identity whose email, else whose unique
//!    name, is an author.
//! 4. `name` is a non-email entry in an author's `aliases` array (a login or
//!    a display name an operator merged), ignoring case.
//!
//! No rule matches: the identity is unmatched and the caller reports it.
//! Test: `developer_tests`.

use std::collections::HashMap;

use rusqlite::Connection;

use crate::collect::identity::IdentityResolver;
use crate::core::errors::{Result, TgaError};

/// Which rule matched a Linear identity to a developer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchRule {
    /// Rule 1: the canonical email.
    Email,
    /// Rules 2–4: an alias (stored or configured).
    Alias,
}

/// One matched tga developer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeveloperMatch {
    /// `authors.id`.
    pub id: i64,
    /// `authors.canonical_name`.
    pub name: String,
    /// The rule that matched.
    pub rule: MatchRule,
}

/// The `authors` table, indexed for the match rules in the module doc.
pub struct DeveloperIndex {
    /// Lower-cased canonical email → (id, name).
    by_email: HashMap<String, (i64, String)>,
    /// Lower-cased email alias → (id, name).
    by_email_alias: HashMap<String, (i64, String)>,
    /// Lower-cased non-email alias (login, display name) → (id, name).
    by_name_alias: HashMap<String, (i64, String)>,
    /// Lower-cased canonical name → (id, name); `None` when two authors
    /// share the name, so a name match is never a guess between them.
    by_name: HashMap<String, Option<(i64, String)>>,
    /// Configured aliases, fuzzy tiers off.
    resolver: IdentityResolver,
}

impl DeveloperIndex {
    /// Read `authors` and index it.
    ///
    /// `resolver` carries the configured aliases; its fuzzy fallback is
    /// turned off here, so rule 3 only follows declared aliases.
    ///
    /// # Errors
    ///
    /// [`TgaError::DbError`] on a failed read; [`TgaError::ValidationError`]
    /// when an author's `aliases` is not a JSON array of strings (the error
    /// names the row).
    pub fn load(conn: &Connection, resolver: IdentityResolver) -> Result<Self> {
        let mut stmt = conn.prepare(
            "SELECT id, canonical_name, canonical_email, aliases FROM authors ORDER BY id",
        )?; // #272: deterministic first claimant
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut index = Self {
            by_email: HashMap::new(),
            by_email_alias: HashMap::new(),
            by_name_alias: HashMap::new(),
            by_name: HashMap::new(),
            resolver: resolver.with_fuzzy_fallback(false),
        };
        for row in rows {
            let (id, name, email, aliases) = row?;
            let entry = (id, name.clone());
            if !email.trim().is_empty() {
                index
                    .by_email
                    .entry(email.trim().to_lowercase())
                    .or_insert_with(|| entry.clone());
            }
            index
                .by_name
                .entry(name.trim().to_lowercase())
                .and_modify(|e| *e = None)
                .or_insert_with(|| Some(entry.clone()));
            // #272: a malformed alias list is an error naming the row, not a
            // silent "no aliases" that would mark a known developer unmatched.
            let aliases: Vec<String> = serde_json::from_str(aliases.as_deref().unwrap_or("[]"))
                .map_err(|e| {
                    TgaError::ValidationError(format!(
                        "authors.aliases for author id {id} is not a JSON array of strings: {e}"
                    ))
                })?;
            for alias in aliases {
                let key = alias.trim().to_lowercase();
                if key.is_empty() {
                    continue;
                }
                let map = if key.contains('@') {
                    &mut index.by_email_alias
                } else {
                    &mut index.by_name_alias
                };
                map.entry(key).or_insert_with(|| entry.clone());
            }
        }
        Ok(index)
    }

    /// Match a Linear `(email, name)` to a developer (rules 1–4).
    pub fn match_identity(
        &self,
        email: Option<&str>,
        name: Option<&str>,
    ) -> Option<DeveloperMatch> {
        let email = email.map(str::trim).filter(|e| !e.is_empty());
        let name = name.map(str::trim).filter(|n| !n.is_empty());
        if let Some(e) = email.map(str::to_lowercase) {
            if let Some(hit) = self.by_email.get(&e) {
                return Some(found(hit, MatchRule::Email));
            }
            if let Some(hit) = self.by_email_alias.get(&e) {
                return Some(found(hit, MatchRule::Alias));
            }
        }
        if let Some(hit) = self.configured_alias(email.unwrap_or(""), name.unwrap_or("")) {
            return Some(hit);
        }
        let hit = self.by_name_alias.get(&name?.to_lowercase())?;
        Some(found(hit, MatchRule::Alias))
    }

    /// Match a provider login (`pull_requests.author`) to a developer: a
    /// stored alias, else a configured alias.
    pub fn match_login(&self, login: &str) -> Option<DeveloperMatch> {
        let login = login.trim();
        if login.is_empty() {
            return None;
        }
        if login.contains('@') {
            return self.match_identity(Some(login), None);
        }
        if let Some(hit) = self.by_name_alias.get(&login.to_lowercase()) {
            return Some(found(hit, MatchRule::Alias));
        }
        self.configured_alias("", login)
    }

    /// Rule 3: a declared alias that resolves to a stored author.
    fn configured_alias(&self, email: &str, name: &str) -> Option<DeveloperMatch> {
        if email.is_empty() && name.is_empty() {
            return None;
        }
        let (canon_name, canon_email) = self.resolver.resolve(name, email);
        // The resolver returns its input unchanged when no alias matched.
        if canon_name == name && canon_email == email {
            return None;
        }
        if let Some(hit) = self.by_email.get(&canon_email.trim().to_lowercase()) {
            return Some(found(hit, MatchRule::Alias));
        }
        let hit = self
            .by_name
            .get(&canon_name.trim().to_lowercase())?
            .as_ref()?;
        Some(found(hit, MatchRule::Alias))
    }
}

fn found(hit: &(i64, String), rule: MatchRule) -> DeveloperMatch {
    DeveloperMatch {
        id: hit.0,
        name: hit.1.clone(),
        rule,
    }
}

#[cfg(test)]
#[path = "developer_tests.rs"]
pub(crate) mod tests;
