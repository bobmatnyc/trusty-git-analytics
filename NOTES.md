# CI/release workflow notes — trusty-git-analytics (split-plan step 6)

Scope: `.github/workflows/{ci,release,semver}.yml`, `docs/PUBLISHING.md`, this
file. `trusty-audit-install.yml` is deliberately NOT part of this PR — it
travels with `crates/trusty-audit/install.sh` in a sibling branch that owns
both. Sources read (all read-only): the live `bobmatnyc/trusty-tools`
checkout's `.github/workflows/{release,ci,semver-checks,trusty-audit-install}.yml`,
`scripts/{preflight-publish,check_semver,publish-dry-run-order}.sh`,
`.claude/skills/cargo-publish/SKILL.md`, `crates/trusty-installer/src/download/
{release,pinned}.rs`, `crates/{trusty-git-analytics,trusty-audit}/Cargo.toml`,
`crates/trusty-audit/install.sh`.

## File list (this PR)

```
.github/workflows/ci.yml
.github/workflows/release.yml
.github/workflows/semver.yml
docs/PUBLISHING.md
NOTES.md
```

## YAML validation

`actionlint` (`/opt/homebrew/bin/actionlint`) was run against
`ci.yml`, `release.yml`, and `semver.yml`: exits 0 with no findings — schema,
expression (`${{ }}` context reference), and shellcheck-of-embedded-`run:`-
blocks checks all pass. Also cross-checked with a plain YAML parse (Python's
`yaml.safe_load`): clean, no syntax errors, no duplicate keys. actionlint
cannot verify runtime facts it has no way to know (e.g. that
`ubuntu-24.04-arm` is a real current runner label) — those remain in "Cannot
fully verify from a draft" below.

## Confirmed against the grafted tree

The root `Cargo.toml` at `24ac2b7` declares
`[workspace] members = [".", "crates/trusty-audit"]` with
`exclude = ["crates/trusty-audit/ui"]` — the Tauri GUI crate is excluded from
the workspace in the manifest itself, not merely from `default-members`. A
bare `cargo build/test/clippy --workspace` therefore never touches it and no
`--exclude` flag is needed anywhere in `ci.yml`. MSRV 1.94 is confirmed via
`clippy.toml` (`msrv = "1.94.0"`) and the root package's `rust-version`.
`crates/trusty-audit/README.md` exists; `crates/trusty-audit/LICENSE` does
not (only the repo-root `LICENSE`), matching the "presence-checked `cp ... ||
true`" packaging step in `release.yml`. No pre-existing `.github/workflows/`
directory or `scripts/` directory exists in this repo as of `24ac2b7` — there
is nothing from a standalone era to remove.

## Asset-name mapping table (trusty-tools vs this repo) — proving identity

trusty-installer's `download/release.rs` builds the download URL as
`<RELEASE_DL_BASE>/<tag>/<asset_name_for_tag(crate)>-<version>-<target>.tar.gz`
(+`.sha256`), where `asset_name_for_tag("tga") == "trusty-git-analytics"` and
every other crate defaults to its own name unchanged (confirmed:
`asset_name_for_tag_resolves_tga_alias` / `asset_name_for_tag_defaults_to_
crate_name` tests, `crates/trusty-installer/src/download/release.rs:98-104`).

| Crate (Cargo pkg) | trusty-tools asset prefix | This repo's asset prefix | Target suffixes | Match? |
|---|---|---|---|---|
| `tga` | `trusty-git-analytics` | `trusty-git-analytics` | `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` | Yes |
| `trusty-audit` | `trusty-audit` | `trusty-audit` | same three | Yes |

Checksum file: `<asset>.tar.gz.sha256`, produced by `sha256sum <file> >
<file>.sha256` (or `shasum -a 256` on macOS) — identical command, identical
one-line `<hex>  <filename>` format, in both the trusty-tools workflow and
this repo's `release.yml` ("Strip, package, and checksum" step, copied
byte-for-byte in structure). `sha256_url()` in `pinned.rs`/`release.rs` is
just `asset_url() + ".sha256"`, so no separate naming rule applies there.

Archive contents: stripped binary(ies) + `LICENSE` + a README, no other
files. Matches. (trusty-tools also writes an `ORT-RUNTIME-NOTE.txt` for
load-dynamic ORT builds — not applicable here, neither crate bundles ORT.)

Binaries per crate (from trusty-tools' release.yml per-crate config table,
`crates/*/Cargo.toml` `[[bin]]` sections):

| Crate | Binaries in the tarball |
|---|---|
| `tga` | `tga` |
| `trusty-audit` | `trusty-audit`, `taudit` |

Tag pattern: `<crate-directory-name>-v<version>` for both — `trusty-git-
analytics-v<version>` and `trusty-audit-v<version>`. This repo's workflows
accept ONLY that spelling, never the `tga-v*` package-name alias trusty-tools
also accepts (see "Deliberately not carried over" below).

## Everything that had to work in dependency order to write this

1. Confirmed via `crates/trusty-installer/src/download/release.rs` that the
   asset filename prefix for `tga` is the crate DIRECTORY name
   (`trusty-git-analytics`), not the Cargo package name (`tga`) — a filename
   built from `tga` alone would 404 against the installer's own resolution
   logic. `release.yml` keys its asset name off `matrix.crate` (the
   directory-derived value from the parsed tag), never off `matrix.cargo_pkg`,
   matching this.
2. Confirmed trusty-audit is NOT a Cargo dependency of tga (comment in
   `crates/trusty-git-analytics/Cargo.toml`: "trusty-audit resolves it by
   version at runtime, not through a Cargo edge") — so `docs/PUBLISHING.md`
   states no required publish order between the two, rather than inventing
   one.
3. Confirmed neither crate needs `SKIP_UI_BUILD`, the AL2023 load-dynamic
   variant, or `NATIVE_LINUX_LINK` (trusty-tools' own per-crate config table
   sets all three to false/empty for both `trusty-git-analytics` and
   `trusty-audit`) — so `release.yml`'s build matrix carries only the three
   "standard"/"linux-zigbuild" legs and drops the entire AL2023 / ORT /
   numkong branch of trusty-tools' workflow.

## Deliberately not carried over (and why)

- **Homebrew formula bump.** trusty-tools' `homebrew-bump` job renders and
  pushes a formula to `bobmatnyc/homebrew-trusty`. Not in the task's
  deliverable list; omitted. If wanted, port it as a fifth job gated on the
  same `HOMEBREW_TAP_ENABLED` repo variable / `HOMEBREW_TAP_TOKEN` secret —
  those secrets are scoped per-repo, so they would need re-provisioning here
  regardless.
- **Release-completeness-audit job + immutable-release stuck-asset repair.**
  trusty-tools added these after concrete incidents (four silent
  arm64-less releases; a permanently asset-less immutable release). This
  repo has no history of either yet and immutable releases may not even be
  enabled here (open question below). Kept the simpler "require the primary
  macOS artifact, else fail" check from the older trusty-tools shape instead.
- **git-cliff changelog generation.** Replaced with softprops'
  `generate_release_notes: true` (plain GitHub auto-notes) to avoid porting
  `cliff.toml` and its per-crate `--include-path`/`--tag-pattern` scoping for
  a two-crate repo. Revisit if release-note quality/format matters more than
  this assumes.
- **The `tga-v*` package-name tag alias.** trusty-tools accepts both
  `tga-v*` and `trusty-git-analytics-v*` (issue #1128) because its
  `cargo-publish` skill doc explicitly says "push ONLY the canonical
  `trusty-git-analytics-v<version>` tag; never push a `tga-v*` alias" — the
  alias-acceptance existed only to tolerate historical mistakes. This repo's
  workflows accept only the canonical spelling from day one, so there is no
  alias-tolerance code to carry over.
- **preflight-publish.sh's other nine checks** (identity/gh-account gate,
  tag-publish-commit parity, UI-bundle-freshness, changelog-assembler ran
  check, engagement-pin refresh for non-trusty-audit crates). Per the task's
  scope ("carry over only the semver check and a short publish checklist"),
  these are summarized as manual steps in `docs/PUBLISHING.md` rather than
  ported as scripts. `docs/PUBLISHING.md` flags the one exception
  (trusty-audit's engagement-pin table) as an open item rather than silently
  dropping it.
- **cargo-semver-checks self-tests, accepted-breaks override file,
  dynamic PR-check-name `verdict` job.** All exist in trusty-tools to answer
  specific dated incidents recorded in that workflow's comments; none has
  happened in this repo yet. `semver.yml`'s header comment says so
  explicitly so a future maintainer knows this was a deliberate cut, not an
  oversight.
- **`trusty-audit-install.yml`.** Owned by a sibling branch alongside the
  `install.sh` move; landing it here would create a duplicate/conflicting PR
  surface. Its selftest script (`scripts/check_trusty_audit_install_selftest.sh`)
  is expected to travel with that same branch.

## Assumptions (mark false if reality differs)

1. **tga's edition stays 2021, trusty-audit's stays 2024** (per the task
   description) — nothing in the CI pins an edition explicitly (that lives in
   each crate's `Cargo.toml`), but the MSRV job's toolchain
   (`dtolnay/rust-toolchain@1.94`) must support both editions, which 1.94
   does.
2. **`jq` is present on `ubuntu-latest`** for `semver.yml`'s baseline
   resolution — true on GitHub-hosted runners today; would need
   `apt-get install jq` added if a self-hosted runner is ever substituted.
3. **crates.io publishing continues to use the `bobmatnyc` gh identity** —
   not encoded anywhere in these workflows (no CI job runs `cargo publish` at
   all, per the owner ruling), but `docs/PUBLISHING.md` inherits this
   assumption implicitly from trusty-tools' process.

## Cannot fully verify from this environment

- **trusty-installer needs a follow-up change of its own**, separate from
  this PR. Its `RELEASE_DL_BASE` constant
  (`https://github.com/bobmatnyc/trusty-tools/releases/download`,
  `crates/trusty-installer/src/download/release.rs:27-28`) is a single
  hardcoded repo for EVERY crate it downloads, including `tga` and
  `trusty-audit`. Splitting those two crates into their own repo means
  trusty-installer must learn a per-crate repository mapping (or a second
  base constant) before it can resolve a `tga`/`trusty-audit` release from
  the new location — this is exactly the "trusty-installer / tctl need only
  a repository change" the owner ruling names, but it is a change IN
  trusty-installer, which lives in `trusty-tools`, not in this repo. Flagging
  it here because these release workflows are worthless to the installer
  until that companion change ships.
- **Whether GitHub's Immutable Releases setting is enabled on the new repo.**
  trusty-tools has it on repo-wide, which is why its release.yml carries the
  stuck-asset-less-immutable-release detection this workflow omits. If the
  new repo inherits the same org-wide setting, a first failed/partial release
  attempt on a real tag could get stuck the same way trusty-installer-v0.5.0
  did — worth checking repo settings before the first real tag push (also
  flagged in `docs/PUBLISHING.md`).
- **The exact runner name `ubuntu-24.04-arm` is still the current
  GitHub-hosted arm64 label** as of trusty-tools' own release.yml (dated
  comments reference issue #2533, 2026). Unverified independently here beyond
  trusting that source.

## Open questions for the split-plan owner

1. Is trusty-audit's `[tools]` engagement-pin refresh
   (`scripts/refresh-engagement-pins.sh`, preflight CHECK 10) wanted in this
   repo at all, given trusty-audit's sibling tools (`trusty-common`,
   `trusty-installer`, `trusty-search`, …) are published from a DIFFERENT
   repo now? `docs/PUBLISHING.md` flags this as manual-for-now; porting the
   script is a reasonable follow-up once the cross-repo version-pin story is
   settled.
2. Does the new repo want a Homebrew formula for `tga`/`trusty-audit` at all
   (see "Deliberately not carried over")? If yes, this needs its own repo
   variable/secret provisioning in `bobmatnyc/homebrew-trusty`'s target
   config before a `homebrew-bump` job would do anything but fail.
