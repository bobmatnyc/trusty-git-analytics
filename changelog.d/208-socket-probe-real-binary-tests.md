Fixed
- The `#[ignore]`d real-binary audit tests probed trusty-search at `http://127.0.0.1:7878/health`, a listener trusty-search no longer serves. They now call `search.health` on the trusty-search socket (`TRUSTY_SEARCH_SOCKET` or the standard path) and pass that socket to `trusty-analyze` as `TRUSTY_SEARCH_SOCKET` ([#208](https://github.com/bobmatnyc/trusty-git-analytics/issues/208)).
- Moved to trusty-common 0.58, the release trusty-installer 0.14 requires ([#208](https://github.com/bobmatnyc/trusty-git-analytics/issues/208)).
