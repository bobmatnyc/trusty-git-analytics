Changed
- The Linear issue query asks for `description` and `previousIdentifiers`, so `work_items.raw_json` for a Linear issue now carries the ticket description, as a JIRA payload does. `fact_pm_work` and `fact_pm_effort` score Linear issues from it; `tga inspect attest` scans that prose like any other `raw_json` (#190).
- `work_items.tags` holds a Linear issue's label names, comma-separated (#190).
- `tga linear sync` treats an issue as unchanged only when its stored GraphQL node is identical, not when `updatedAt` matches, so a field added to the query is written to issues that did not change (#190).
- The first `tga linear sync` of each team after this upgrade reads the team's whole history once, so stored issues get the new fields; `linear_sync_cursor.fields_version` records it and later runs resume from the cursor. A `--since` or `--exclude-archived` run does not count as the full read (#190).
