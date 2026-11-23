Fixed
- The safety-net fetch in `collect_window` now applies `fetch_timeout_secs` under the same lock as the pipeline fetch, so it can no longer hang on a stalled remote or inherit another fetch's timeout (#286).
