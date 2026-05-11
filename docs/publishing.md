# Publishing to crates.io

Step-by-step guide for releasing trusty-git-analytics crates.

## Pre-publish Checklist

Before creating any release tag:

1. **All tests pass** on the target crate and the workspace:
   ```bash
   cargo test --workspace
   ```

2. **Clippy is clean** (zero warnings):
   ```bash
   cargo clippy --workspace --all-targets -- -D warnings
   ```

3. **Formatting is clean**:
   ```bash
   cargo fmt --check
   ```

4. **Docs build clean**:
   ```bash
   RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
   ```

5. **CHANGELOG.md** has an entry for the version under `## [x.y.z]`.

6. **Version number** in the workspace `Cargo.toml` `[workspace.package]` section matches what you intend to publish.

## Publish Order

The crate dependency graph dictates the order. A crate cannot be published until all of its dependencies are already on crates.io with the version it requires.

```
1. tga-core           (no internal deps)
        │
2a. tga-collect       (depends on tga-core)
2b. tga-classify      (depends on tga-core)    ← these three can publish in parallel
2c. tga-report        (depends on tga-core)
        │
3. tga-cli            (depends on all four)
```

## Updating Path Dependencies Before Publishing

The workspace `Cargo.toml` uses `version.workspace = true` for all crates. When publishing a dependent crate (e.g. `tga-cli`), its `Cargo.toml` has path dependencies like:

```toml
tga-core = { path = "../tga-core" }
```

Before publishing `tga-cli` you must also specify the version so crates.io can resolve the dependency for downstream users who do not have the path available:

```toml
tga-core = { path = "../tga-core", version = "0.1.0" }
```

The publish workflow handles this for `tga-core` automatically. For the other crates, make sure `tga-core`'s crates.io version matches the `version` field in the path dependency before pushing the publish tag.

## Tag Naming Convention

Each crate has its own tag, matching the pattern `<crate-name>-v<semver>`:

| Crate | Example tag |
|-------|-------------|
| `tga-core` | `tga-core-v0.1.0` |
| `tga-collect` | `tga-collect-v0.1.0` |
| `tga-classify` | `tga-classify-v0.1.0` |
| `tga-report` | `tga-report-v0.1.0` |
| `tga-cli` | `tga-cli-v0.1.0` |

Create a tag locally and push it to trigger the publish workflow:

```bash
git tag tga-core-v0.1.0
git push origin tga-core-v0.1.0
```

## GitHub Actions: Which Workflow Triggers on Which Tag

Currently only `tga-core` has an automated publish workflow (`publish-tga-core.yml`). It triggers on `tga-core-v*` tag pushes.

The other four crates (`tga-collect`, `tga-classify`, `tga-report`, `tga-cli`) do not yet have publish workflows and must be published manually (see below). Add a `publish-<crate>.yml` for each following the same pattern as `publish-tga-core.yml`.

### publish-tga-core.yml pipeline

1. **Dry-run gate**: `cargo publish --dry-run -p tga-core` — catches missing metadata, license issues, file size limits.
2. **Clippy gate**: `cargo clippy -p tga-core -- -D warnings` — ensures the published crate compiles cleanly.
3. **Publish**: `cargo publish -p tga-core --no-verify` — `--no-verify` skips the local build (already done in step 1); `CARGO_REGISTRY_TOKEN` is read from the `CARGO_REGISTRY_TOKEN` repository secret.

### Manual dry-run

To test a publish without uploading, use `workflow_dispatch` with `dry_run: true`, or run locally:

```bash
cargo publish --dry-run -p tga-core
```

## Manual Publish Steps (for crates without a workflow)

```bash
# Ensure you are on main and everything is clean
git checkout main
git pull

# Verify the dry run passes
cargo publish --dry-run -p tga-collect

# Publish
cargo publish -p tga-collect

# Repeat for tga-classify and tga-report (can be done in either order,
# both only depend on tga-core which is already published)

cargo publish --dry-run -p tga-classify
cargo publish -p tga-classify

cargo publish --dry-run -p tga-report
cargo publish -p tga-report

# After all three are on crates.io, publish tga-cli
cargo publish --dry-run -p tga-cli
cargo publish -p tga-cli
```

## Required Repository Secrets

| Secret | Used by |
|--------|---------|
| `CARGO_REGISTRY_TOKEN` | `publish-tga-core.yml`; required for all publish workflows |

Generate a token at https://crates.io/settings/tokens with the "publish-new" and "publish-update" scopes. Add it in GitHub under Settings → Secrets and variables → Actions.

## Verifying a Successful Publish

After the workflow completes (or `cargo publish` returns without error):

```bash
# Search crates.io index (may take a minute to propagate)
cargo search tga-core

# Or check directly
open https://crates.io/crates/tga-core
```

Verify the version number and README content on the crates.io page match expectations.

## Version Bumping Workflow

1. Update `[workspace.package] version` in the root `Cargo.toml` to the new version.
2. Update `CHANGELOG.md`: move `[Unreleased]` content to `[x.y.z] - YYYY-MM-DD`.
3. Commit: `git commit -m "chore: bump version to x.y.z"`.
4. Push the commit.
5. Tag and push in publish order (core first, then parallel crates, then cli).

Because all crates share `version.workspace = true`, a single version bump in the workspace manifest applies to all five crates simultaneously.
