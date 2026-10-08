Fixed
- `tga collect` no longer reports success when a GitHub PR-list or PR-reviewer fetch fails (#146). Before, each failed repository or pull request was only a warning log line, so a run that dropped thousands of HTTP 403s ended with 0 failures. A failed repository or pull request is still skipped, and the pass carries on with the rest. At the end of the pass:
  - An HTTP 403, an HTTP 5xx that outlived its retries, or a transport or parse error fails the `github` (PR list) or `github reviewers` stage once, so the run exits non-zero. The message gives the count per status code and names up to 20 items as `owner/repo (HTTP 403)` or `owner/repo#N (HTTP 403)`.
  - An HTTP 404 (a renamed, deleted or invisible repository or pull request) is one counted warning naming up to 20 items, and the run still exits 0.
  - A rate limit is unchanged: it stays a warning and exit 0 (#6553).
- A PR-list error on page 2 or later no longer discards the pull requests the earlier pages fetched for that repository (#146). They are stored, and the fault names the repository as `owner/repo (pull requests after page N not collected)`. This also applies when the later page was rate-limited.
