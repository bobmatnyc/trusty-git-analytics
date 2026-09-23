//! The recorded trusty-tools history the two agentic-detection corpus tests read.
//!
//! Why: both tests used to walk the surrounding trusty-tools checkout with
//! git2. This repository was split out of that monorepo at 1c1ce516a and its
//! own history keeps only the ~400 commits that touched tga, below the
//! 2,000-commit floor and the >900 house-footer count the tests assert. The
//! fixture keeps them measuring the same 3,970 real commits, with every
//! assertion unchanged, and without depending on any checkout.
//! What: `fixtures/trusty-tools-history.jsonl` holds one JSON object per commit
//! reachable from trusty-tools 1c1ce516a (`git rev-list` order), with the
//! message, author email and committer email exactly as git2's
//! `Commit::message()`, `author().email()` and `committer().email()` return
//! them. It was produced from raw `git cat-file --batch` objects, so there is
//! no mailmap and no re-wrapping.
//! Test: `agentic_detection_corpus`, `agentic_detection_corpus_configured`.

use std::path::Path;

/// One recorded commit: the three fields the detector reads.
#[derive(serde::Deserialize)]
pub struct Commit {
    pub message: String,
    pub author_email: String,
    pub committer_email: String,
}

/// Every commit in the recorded trusty-tools history.
///
/// Panics when the fixture is missing or a line does not parse, because a
/// silently short corpus would move every percentage the callers assert on.
pub fn history() -> Vec<Commit> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("trusty-tools-history.jsonl");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read the corpus fixture {}: {e}", path.display()));
    text.lines()
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("{}:{}: {e}", path.display(), i + 1))
        })
        .collect()
}
