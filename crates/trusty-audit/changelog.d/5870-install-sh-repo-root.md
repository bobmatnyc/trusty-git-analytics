Changed

- The one-line macOS installer moved from `crates/trusty-audit/install.sh` in
  bobmatnyc/trusty-tools to `install.sh` at the root of
  bobmatnyc/trusty-git-analytics, and now resolves `trusty-audit-v*` releases
  from this repository:
  `curl -fsSL https://raw.githubusercontent.com/bobmatnyc/trusty-git-analytics/main/install.sh | sh`.
  Tags and asset names are unchanged. It installs nothing until this repository
  publishes its first `trusty-audit-v*` release.
