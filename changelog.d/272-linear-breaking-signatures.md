Breaking
- `classify::sources::linear::fetch_issue` and `fetch_issues_batch` take a new last parameter, `guard: Option<&WorkspaceGuard>`; pass `None` for the previous behaviour (#272).
- `CollectError` and `ClassifyError` each gain the variant `LinearWorkspace`; an exhaustive `match` on either enum needs a new arm (#272).
