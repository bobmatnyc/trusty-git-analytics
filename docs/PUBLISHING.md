# Publishing trusty-git-analytics and trusty-audit

Manual publish checklist for local-ops. Trimmed from trusty-tools'
`.claude/skills/cargo-publish/SKILL.md` and `scripts/preflight-publish.sh`:
this repo carries only the semver check and this checklist, not the full
ten-check script, the accepted-breaks override system, or the
UI-bundle-freshness / changelog-assembler / engagement-pin checks that exist
for crates this repo does not have (a Svelte-embedding crate, a multi-crate
changelog train, trusty-audit's `[tools]` engagement template — the last of
which DOES still apply to trusty-audit; see "Publishing trusty-audit" below).

Releases are manual. CI (`.github/workflows/release.yml`) only builds the
binaries and `.sha256` files and attaches them to the GitHub Release that the
pushed tag names — it never runs `cargo publish` and never holds a
crates.io token.

**Assumption — Immutable Releases setting.** This repo's GitHub "Immutable
releases" setting could not be read via the API while drafting these
workflows; it is assumed OFF (the default for a new repository). If it turns
out to be ON, a first failed or partial release attempt on a real tag can
leave a permanently asset-less immutable release (as happened once for
trusty-installer-v0.5.0 in trusty-tools) — check the repo's release settings
before the first real tag push, and if it is ON, be prepared to cut a new
patch version rather than reuse the stuck tag.

## Publish order

**tga and trusty-audit have no Cargo dependency edge between them** — confirmed
from `crates/trusty-git-analytics/Cargo.toml`: tga's library API has no
external linking consumers; trusty-audit resolves it by version at runtime,
not through a Cargo edge (it shells out to a `tga`/`trusty-audit` binary
resolved by trusty-installer's pinned-version downloader, not `cargo`'s
dependency resolver). **There is no required publish order between the two
crates in this repo** — publish whichever one changed, independently.

`trusty-audit`'s `Cargo.toml` DOES depend on `trusty-common`, `trusty-installer`,
and `trusty-progress` as ordinary crates.io dependencies (not path/workspace
deps — those crates are published from the `bobmatnyc/trusty-tools` repo, not
this one). Before publishing `trusty-audit`, confirm the versions it pins are
already live on crates.io; there is nothing this repo's CI can check for that,
since the dependency lives in a different repo's release process.

## Per-crate checklist

Repeat this for each crate being published (`tga` = repo root, `trusty-audit` =
`crates/trusty-audit`).

```bash
# 0. Work from a dedicated worktree, never the main checkout.
git fetch origin main
git worktree add -b chore/publish-<crate> \
    .worktrees/publish-<crate> origin/main
cd .worktrees/publish-<crate>

# 1. Confirm merged main and a clean tree.
git status --porcelain            # must be empty
git rev-parse HEAD origin/main    # HEAD should equal origin/main

# 2. Confirm the target version is not already live.
curl -s https://crates.io/api/v1/crates/<pkg>/<version> | head -c 200
# 404/"Not Found" -> safe to proceed; a JSON body means this version already shipped

# 3. Quality gates (must all pass; no --allow-dirty / --no-verify / --force).
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p <pkg>
cargo check --workspace

# 4. SemVer gate — the local equivalent of .github/workflows/semver.yml,
#    run BEFORE tagging so a break is caught while nothing is public yet.
cargo install cargo-semver-checks@0.50.0 --locked   # once per machine
BASELINE=$(curl -s https://crates.io/api/v1/crates/<pkg>/versions \
  | jq -r '.versions[] | select(.yanked==false) | select(.num|test("-")|not) | .num' \
  | sort -V | tail -1)
cargo semver-checks check-release -p <pkg> --baseline-version "$BASELINE" --only-explicit-features
# A missing baseline (crate never published) is a clean skip, not a failure.
# A reported break means: bump the breaking version component (0.x -> MINOR,
# 1.x+ -> MAJOR) and re-run, or record the exception per "Accepting a break" below.

# 5. Bump the version in Cargo.toml, commit.
git commit -am "chore: bump <crate> to v<version>"

# 6. Dry run.
cargo publish --dry-run -p <pkg>

# 7. Tag and push. TAG NAME IS THE CRATE DIRECTORY, NEVER THE PACKAGE ALIAS —
#    push trusty-git-analytics-v<version> for tga, never tga-v<version>.
git tag <crate-directory>-v<version>
git push origin <crate-directory>-v<version>

# 8. Wait for .github/workflows/release.yml to finish on this tag and attach
#    every platform asset to the GitHub Release. One-shot check, do not poll:
gh run list --workflow=release.yml --branch <crate-directory>-v<version> --limit 1
gh release view <crate-directory>-v<version> --json assets

# 9. Publish to crates.io.
cargo publish -p <pkg>

# 10. Wait ~60-120s for registry propagation, then verify.
sleep 100
curl -s https://crates.io/api/v1/crates/<pkg>/<version> | head -c 200

# 11. Binaries only: install locally and confirm the version.
cargo install --path <crate-dir> --locked
<binary> --version
```

## Accepting a break (rare)

If a breaking public-API change genuinely ships without a breaking version
bump (owner-authorized only), record it before publishing rather than
disabling the gate: add a short note to the crate's changelog naming the
break and the reason, and get explicit sign-off in the PR. This repo does not
carry trusty-tools' `scripts/semver-accepted-breaks/` machine-readable
override file — that is a deliberate simplification (see NOTES.md); if this
repo starts needing it routinely, port that mechanism rather than skipping
the gate by hand.

## Publishing trusty-audit — one extra step

`trusty-audit`'s engagement template (`crates/trusty-audit/templates/
engagement.template.toml`) pins the versions of its own sibling tools in a
`[tools]` table. Before bumping trusty-audit's version, confirm those pins
are current — a stale pin naming a version that is not what this release
actually ships is a real defect, not cosmetic. trusty-tools carries a
dedicated `scripts/refresh-engagement-pins.sh` for this (CHECK 10 in
`preflight-publish.sh`); this repo has not ported that script — check the
`[tools]` table by hand until it does (see NOTES.md, open question).

## Worktree cleanup

Report the merged PR, the worktree path, and the branch; do not remove the
worktree yourself. Whoever operates this repo's PM/orchestrator reclaims it.

## Cross-references

- CI's semver gate: `.github/workflows/semver.yml`
- Release build/attach pipeline: `.github/workflows/release.yml`
- Asset naming and the trusty-installer contract it must match: `NOTES.md`
