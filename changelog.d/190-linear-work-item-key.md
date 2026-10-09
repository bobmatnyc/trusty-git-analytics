Added
- `work_items.stable_id` holds the provider id that survives a rename — for Linear, the issue UUID (migration v35, additive). Existing Linear rows get it from `linear_issues.linear_id`; rows with no match keep NULL until their issue is synced (#190).
- An issue that moves team keeps one `work_items` row: the row is renamed to the new identifier, and its `commit_work_items`, `fact_pm_work` and `fact_pm_effort` rows move with it. One identifier claimed by two Linear issue ids fails the write instead of overwriting the stored row (#190).
- Rows left by an issue that moved team before its first post-v33 sync are cleaned up through Linear's `previousIdentifiers`: the `linear_issues` row is updated in place or deleted as a stale copy, and the `work_items` row is merged into the issue's row (#190).
- A commit that names an identifier an issue held before a team move links to that issue (#190).
