Fixed
- The GitHub PR client, org discovery and the reviewer pass now authenticate with `GITHUB_TOKEN` when `github.token` is unset or blank. Before, only config validation read the env var, so PR calls went out unauthenticated: 404 on private repos and an exhausted rate limit. A non-blank `github.token` still wins (#188).
- An unset `${VAR}` placeholder in `github.token` with no `GITHUB_TOKEN` now means no token. Before, it sent an empty `Authorization: Bearer` header and the client reported itself as authenticated (#188).
- Org discovery skips archived repositories, both in `tga collect` with `github.discover_orgs: true` and in `tga install --org` (#188).
