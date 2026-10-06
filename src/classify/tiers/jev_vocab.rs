//! Classification vocabulary the Jev pseudonymizer must never learn as a
//! name (#111, gate B).
//!
//! Why: gate B found 19–23 % of payloads had an ordinary classification
//! word replaced, mostly because a login or e-mail local-part such as
//! `test`, `deploy` or `admin` was learned as a person and then matched in
//! prose. The bar is 2 %.
//! What: [`Vocab`] holds the configured vocabulary — category names (and
//! their `_` parts), category descriptions, and the keyword and pattern
//! words of the loaded rules — plus [`TECH_WORDS`], a built-in list of
//! generic and technical words. [`Vocab::is_common`] answers whether one
//! word is vocabulary: an exact match, a configured word of five or more
//! letters that prefixes it (`refactor` → `refactoring`), or a plural or
//! past form of either.
//! Test: `classify::tiers::jev_gateb_tests::generic_logins_are_not_learned`,
//! `jev_gateb_tests::rule_vocabulary_is_never_a_name`.

use std::collections::HashSet;

/// Generic, dictionary and technical words that are never a person name on
/// their own (#111, gate B).
pub(super) const TECH_WORDS: &[&str] = &[
    "access",
    "account",
    "action",
    "actions",
    "add",
    "admin",
    "agent",
    "alert",
    "alpha",
    "api",
    "app",
    "apply",
    "archive",
    "asset",
    "audit",
    "auth",
    "auto",
    "automation",
    "backend",
    "backup",
    "base",
    "batch",
    "beta",
    "bot",
    "branch",
    "bug",
    "build",
    "builder",
    "bump",
    "cache",
    "change",
    "check",
    "chore",
    "ci",
    "cleanup",
    "cli",
    "client",
    "cloud",
    "cluster",
    "code",
    "commit",
    "config",
    "core",
    "cron",
    "data",
    "database",
    "db",
    "debug",
    "default",
    "demo",
    "deploy",
    "deployer",
    "design",
    "dev",
    "developer",
    "devops",
    "docs",
    "domain",
    "engine",
    "engineering",
    "env",
    "error",
    "event",
    "feat",
    "feature",
    "file",
    "fix",
    "flag",
    "frontend",
    "gateway",
    "git",
    "github",
    "gitlab",
    "guest",
    "helper",
    "hook",
    "hotfix",
    "infra",
    "integration",
    "internal",
    "issue",
    "jenkins",
    "job",
    "lint",
    "local",
    "log",
    "logs",
    "main",
    "maint",
    "maintainer",
    "manager",
    "master",
    "merge",
    "migration",
    "mobile",
    "model",
    "monitor",
    "noreply",
    "ops",
    "owner",
    "package",
    "patch",
    "perf",
    "pipeline",
    "platform",
    "prod",
    "production",
    "project",
    "qa",
    "queue",
    "readme",
    "refactor",
    "release",
    "releases",
    "remote",
    "renovate",
    "repo",
    "report",
    "review",
    "reviewer",
    "robot",
    "role",
    "root",
    "runner",
    "sandbox",
    "script",
    "security",
    "server",
    "service",
    "setup",
    "shared",
    "staging",
    "support",
    "sync",
    "system",
    "task",
    "team",
    "tech",
    "template",
    "test",
    "tester",
    "testing",
    "tests",
    "tool",
    "tools",
    "update",
    "upgrade",
    "user",
    "web",
    "webhook",
    "worker",
];

/// The classification vocabulary of one run.
#[derive(Default)]
pub(super) struct Vocab {
    words: HashSet<String>,
    /// Configured words of five or more letters, matched as prefixes.
    fragments: Vec<String>,
}

/// The lowercase letter runs of `text`, after dropping regex escapes such as
/// `\b` and `\d` so `\bfix` yields `fix`.
fn letter_runs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            chars.next();
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else if c.is_alphanumeric() {
            cur.extend(c.to_lowercase());
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.retain(|w| w.chars().count() >= 2);
    out
}

impl Vocab {
    /// The vocabulary of `texts` (category names and descriptions, rule
    /// keywords and patterns) plus [`TECH_WORDS`].
    pub(super) fn new(texts: &[String]) -> Self {
        let mut words: HashSet<String> = TECH_WORDS.iter().map(|w| (*w).to_string()).collect();
        let mut fragments = Vec::new();
        for w in texts.iter().flat_map(|t| letter_runs(t)) {
            if w.chars().count() >= 5 && w.chars().all(char::is_alphabetic) {
                fragments.push(w.clone());
            }
            words.insert(w);
        }
        fragments.sort_unstable();
        fragments.dedup();
        Self { words, fragments }
    }

    /// Whether one word (any case) is vocabulary.
    pub(super) fn is_common(&self, word: &str) -> bool {
        let w = word.to_lowercase();
        let stems = [
            Some(w.as_str()),
            w.strip_suffix("es"),
            w.strip_suffix('s'),
            w.strip_suffix("ed"),
            w.strip_suffix("ing"),
        ];
        let hit = stems.into_iter().flatten().any(|s| {
            self.words.contains(s) || self.fragments.iter().any(|f| s.starts_with(f.as_str()))
        });
        hit
    }

    /// Whether `name` has a letter or digit and every word of it is
    /// vocabulary (`deploy-bot`, `ci`, `Test User`).
    pub(super) fn is_all_common(&self, name: &str) -> bool {
        let mut ws = name
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .peekable();
        ws.peek().is_some() && ws.all(|w| self.is_common(w))
    }
}
