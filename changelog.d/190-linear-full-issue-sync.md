Added
- `tga linear sync --all-teams` syncs every team the Linear API key can see in one run, each team under its own cursor in `linear_sync_cursor` (#190). `--team` and the single `linear.team_keys` entry still select one team.
- `tga linear sync` fetches archived issues (`includeArchived`) and records, per row, whether the issue is archived and when (`archived`, `archived_at`). `--exclude-archived` leaves them out in single-team mode (#190).
- `linear_issues` stores the full issue: Linear's issue `id` as `linear_id` (UNIQUE), state type, estimate, project, cycle and parent ids, label ids and names, due date, assignee id/name/email, creator id, team id, and `raw_json` with the GraphQL node as returned (migration v33, additive). `work_items.raw_json` for Linear now holds that node too, so the effort extractor reads `estimate` and `parent` (#190).
- An issue that moves team (new identifier, same id) updates its one row instead of adding a second. A row written before v33 is filled in place by the first sync that returns its issue (#190).
- `tga linear sync --dry-run` prints, per team, how many rows it would insert, change, move or leave unchanged (#190).
