Breaking
- `tga::collect::github::resolve_github_repos` and `org_discovery::resolve_github_repos_with_discovered` now return `Result<Vec<(String, String)>, CollectError>` instead of `Vec`. A repository whose `github_repo` is not `owner/name` is an error, where it was previously dropped from collection silently. `tga collect` reports it as a failed GitHub stage (#266).
