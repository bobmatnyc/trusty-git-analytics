//! `classification.repo_categories` keys: `<repo>` and `<repo>:<prefix>` (#167).
//!
//! Why: a monorepo holds commits of several categories and needs a
//! category per subdirectory, so a key may name a path prefix.
//! What: [`parse_key`] splits and checks one key; [`RepoKeys`] holds one
//! repository's keys and resolves a commit's changed paths to one category
//! with a deterministic rule (see [`RepoKeys::resolve`]).
//! Test: `classify::pipeline_repo_map_floor_tests`.

use std::collections::{BTreeMap, HashSet};

/// One parsed `classification.repo_categories` key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct MapKey {
    /// The repository name, as stored in `commits.repository`.
    pub(crate) repo: String,
    /// The normalized path prefix (no leading `./` or `/`, no trailing
    /// `/`), or `None` for a bare `<repo>` key.
    pub(crate) prefix: Option<String>,
}

/// Parse `raw` as `<repo>` or `<repo>:<prefix>`.
///
/// What: splits at the first `:`. The prefix loses a leading `./`, and
/// leading and trailing `/`. A key holding `*` is checked by the caller.
/// The repository may hold a `/` only when it is one of `configured`, the
/// `repositories[].name` values (#167 review: `org/repo` names).
///
/// # Errors
///
/// The reason the key is malformed: a `/` in a repository part no
/// configured name matches (a bare `acme-mono/api` means `acme-mono:api`),
/// or an empty repository or prefix.
pub(crate) fn parse_key(
    raw: &str,
    configured: &HashSet<&str>,
) -> std::result::Result<MapKey, String> {
    let (repo, prefix) = match raw.split_once(':') {
        Some((repo, prefix)) => (repo, Some(prefix)),
        None => (raw, None),
    };
    if repo.is_empty() {
        return Err("empty repository name".into());
    }
    if repo.contains('/') && !configured.contains(repo) {
        return Err(format!(
            "a '/' in the repository name, and no repositories[].name is '{repo}'; \
             a path prefix follows a ':'"
        ));
    }
    let prefix = match prefix {
        None => None,
        Some(p) => {
            let p = p.trim_start_matches("./").trim_matches('/');
            if p.is_empty() {
                return Err("empty path prefix after ':'".into());
            }
            Some(p.to_string())
        }
    };
    Ok(MapKey {
        repo: repo.to_string(),
        prefix,
    })
}

/// Whether `path` lies under `prefix`, matched whole path segments:
/// `services` holds `services` and `services/a.rs`, not `services-legacy/`.
pub(crate) fn is_under(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// One repository's keys.
#[derive(Debug, Clone, Default)]
pub(crate) struct RepoKeys {
    /// The bare `<repo>` key's category.
    bare: Option<String>,
    /// `(prefix, category)`, longest prefix first.
    prefixes: Vec<(String, String)>,
}

/// One category's share of a commit's paths in [`RepoKeys::resolve`].
struct Tally<'a> {
    votes: usize,
    /// Prefix length + 1 of the most specific key that voted; 0 = bare.
    specificity: usize,
    prefix: Option<&'a str>,
}

impl RepoKeys {
    /// Add one key's category. The caller rejects duplicate keys.
    pub(crate) fn insert(&mut self, prefix: Option<String>, category: String) {
        match prefix {
            None => self.bare = Some(category),
            Some(p) => {
                self.prefixes.push((p, category));
                self.prefixes
                    .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
            }
        }
    }

    /// Whether any key names a path prefix, so resolution needs paths.
    pub(crate) fn has_prefixes(&self) -> bool {
        !self.prefixes.is_empty()
    }

    /// The category `paths` resolve to, with the prefix of the key that
    /// decided it (`None` for the bare key); `None` when unmapped.
    ///
    /// Why (#167 criterion 4): a commit may touch several prefixes, and the
    /// answer must not depend on path order.
    /// What: each path votes for the category of the longest prefix key it
    /// lies under, else the bare key's category, else "unmapped". The
    /// category with the most votes wins. A tie goes to the category whose
    /// most specific voting key is longest (a bare key is least specific),
    /// then to the alphabetically first category. "Unmapped" wins only with
    /// strictly more votes than every category. With no prefix keys, or no
    /// stored paths, the bare key alone decides. The returned prefix is the
    /// winning category's most specific voting key.
    /// Test: `pipeline_repo_map_floor_tests::a_commit_spanning_prefixes_takes_the_category_most_paths_resolve_to`,
    /// `pipeline_repo_map_floor_tests::the_longest_matching_prefix_wins`,
    /// `pipeline_repo_map_floor_tests::a_bare_repo_key_is_the_fallback`.
    pub(crate) fn resolve(&self, paths: &[String]) -> Option<(Option<&str>, &str)> {
        if self.prefixes.is_empty() || paths.is_empty() {
            return self.bare.as_deref().map(|c| (None, c));
        }
        let mut by_category: BTreeMap<&str, Tally<'_>> = BTreeMap::new();
        let mut unmapped = 0_usize;
        for path in paths {
            let hit = self.prefixes.iter().find(|(p, _)| is_under(path, p));
            let (prefix, category, specificity) = match (hit, self.bare.as_deref()) {
                (Some((p, c)), _) => (Some(p.as_str()), c.as_str(), p.len() + 1),
                (None, Some(c)) => (None, c, 0),
                (None, None) => {
                    unmapped += 1;
                    continue;
                }
            };
            let t = by_category.entry(category).or_insert(Tally {
                votes: 0,
                specificity,
                prefix,
            });
            t.votes += 1;
            // #167 review: an equal-length key breaks on the prefix string,
            // so the deciding key never depends on path order.
            if (specificity, std::cmp::Reverse(prefix))
                > (t.specificity, std::cmp::Reverse(t.prefix))
            {
                t.specificity = specificity;
                t.prefix = prefix;
            }
        }
        // Ascending category order; only a strictly better tally replaces
        // the current one, so equal tallies keep the alphabetically first.
        let mut best: Option<(&str, &Tally<'_>)> = None;
        for (category, t) in &by_category {
            if best.is_none_or(|(_, b)| (t.votes, t.specificity) > (b.votes, b.specificity)) {
                best = Some((*category, t));
            }
        }
        best.filter(|(_, t)| t.votes >= unmapped)
            .map(|(category, t)| (t.prefix, category))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (#167 review): two equal-length prefix keys of one category must
    /// name the same deciding key whatever the path order.
    /// What: keys `bb` and `aa`, both `qa`; both path orders resolve to
    /// `(Some("aa"), "qa")`.
    /// Test: this test.
    #[test]
    fn equal_length_keys_of_one_category_break_on_the_prefix() {
        let mut keys = RepoKeys::default();
        keys.insert(Some("bb".into()), "qa".into());
        keys.insert(Some("aa".into()), "qa".into());
        for order in [["bb/1", "aa/1"], ["aa/1", "bb/1"]] {
            let paths: Vec<String> = order.iter().map(|p| p.to_string()).collect();
            assert_eq!(keys.resolve(&paths), Some((Some("aa"), "qa")), "{order:?}");
        }
    }
}
