Changed
- `tga linear sync` has no default issue cap. `--max-issues N` now fails the run when a team holds more than N issues in the window, naming the team and N, and writes nothing for that team. Before, a default cap of 10,000 cut the walk and the run reported success (#190).
- `tga linear sync --team` includes archived issues by default. Pass `--exclude-archived` for the old behaviour (#190).
- A re-sync no longer rewrites an issue whose `updatedAt` is already stored, in `linear_issues` or `work_items` (#190).
