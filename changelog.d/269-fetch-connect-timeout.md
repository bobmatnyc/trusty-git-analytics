Fixed
- `fetch_timeout_secs` is now enforced: it bounds a git fetch's connect and each socket read/write, so an unreachable or stalled remote fails instead of hanging, and the failure is retried like any other connect failure (#269).
