Changed
- `work_items.project` for a Linear issue holds the Linear project id; the project's name is in `linear_projects`. It was left NULL before. Every Linear row's `item_type` stays `Issue` (#190).
- The one-time full read `tga linear sync` does after an upgrade rewrites every issue's `work_items` row, including issues whose stored node did not change, so existing rows get `project`, `tags` and their `stable_id` key on that sync (#190).
