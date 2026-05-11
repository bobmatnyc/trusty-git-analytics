//! Per-commit diff statistics computation via `git2`.

use git2::{Commit, Delta, DiffOptions, Repository};
use tga_core::models::ChangeType;

use crate::errors::Result;

/// Aggregated diff stats for a single commit.
#[derive(Debug, Clone, Default)]
pub struct CommitDiff {
    /// Total number of files touched by the commit.
    pub files_changed: u32,

    /// Total lines inserted across all files.
    pub insertions: u32,

    /// Total lines deleted across all files.
    pub deletions: u32,

    /// Per-file change records.
    pub files: Vec<FileDiff>,
}

/// Per-file diff record for storage in the `files` table.
#[derive(Debug, Clone)]
pub struct FileDiff {
    /// Path relative to the repository root.
    pub path: String,

    /// Type of change.
    pub change_type: ChangeType,

    /// Lines inserted in this file.
    pub insertions: u32,

    /// Lines deleted in this file.
    pub deletions: u32,
}

/// Compute the diff between a commit and its first parent (or the empty
/// tree if it's the root commit).
///
/// For merge commits (multiple parents), the diff is computed against the
/// first parent only — matching the conventional "what did this merge
/// introduce on top of its mainline parent" interpretation.
///
/// # Errors
///
/// Propagates any `git2` errors from tree lookups or diff computation.
pub fn compute_commit_diff(repo: &Repository, commit: &Commit<'_>) -> Result<CommitDiff> {
    let tree = commit.tree()?;
    let parent_tree = if commit.parent_count() > 0 {
        Some(commit.parent(0)?.tree()?)
    } else {
        None
    };

    let mut opts = DiffOptions::new();
    opts.include_typechange(true);
    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;

    let stats = diff.stats()?;
    let files_cell: std::cell::RefCell<Vec<FileDiff>> =
        std::cell::RefCell::new(Vec::with_capacity(stats.files_changed()));

    diff.foreach(
        &mut |delta, _progress| {
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let change_type = map_change_type(delta.status());
            files_cell.borrow_mut().push(FileDiff {
                path,
                change_type,
                insertions: 0,
                deletions: 0,
            });
            true
        },
        None,
        None,
        Some(&mut |delta, _hunk, line| {
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut files = files_cell.borrow_mut();
            if let Some(file) = files.iter_mut().find(|f| f.path == path) {
                match line.origin() {
                    '+' => file.insertions = file.insertions.saturating_add(1),
                    '-' => file.deletions = file.deletions.saturating_add(1),
                    _ => {}
                }
            }
            true
        }),
    )?;

    Ok(CommitDiff {
        files_changed: stats.files_changed() as u32,
        insertions: stats.insertions() as u32,
        deletions: stats.deletions() as u32,
        files: files_cell.into_inner(),
    })
}

/// Translate a libgit2 `Delta` enum into our [`ChangeType`].
fn map_change_type(delta: Delta) -> ChangeType {
    match delta {
        Delta::Added | Delta::Copied | Delta::Untracked => ChangeType::Added,
        Delta::Deleted => ChangeType::Deleted,
        Delta::Renamed => ChangeType::Renamed,
        _ => ChangeType::Modified,
    }
}
