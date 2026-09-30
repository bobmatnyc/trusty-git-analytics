#!/usr/bin/env bash
# The one local PR gate for trusty-git-analytics.
#
# Why: GitHub Actions runs only for release builds (owner ruling 2026-09-30).
#   PR and push checks run on the developer's macOS host instead, so this
#   script runs exactly what .github/workflows/ci.yml runs, plus the
#   path-scoped checks from website.yml, install-sh.yml, engagement-pins.yml
#   and semver.yml. Its result is posted as the informational `local-gate`
#   commit status; it is not a required check.
#
# What: runs every ci.yml job as one step, then each path-conditional step
#   whose files changed against the merge-base with origin/main (committed,
#   uncommitted and untracked files all count). A failing step does not stop
#   later steps. The summary lists every step as PASS, FAIL, SKIPPED(path) or
#   REPORT-ONLY, and the gate exits non-zero when any blocking step failed.
#   cargo-semver-checks is report-only (standing owner ruling): its failure is
#   printed but never fails the gate. A missing tool or toolchain fails the
#   gate before any step runs; nothing is silently skipped. A change to
#   scripts/gate*.sh also runs scripts/gate-selftest.sh; a gate started by
#   a live run of that selftest (a nonce only that run holds) lists the step
#   as SKIPPED(nested) instead of starting the selftest again.
#
# Usage: scripts/gate.sh [--all] [--post-status]
#   --all          run every path-conditional step, whatever the diff says
#   --post-status  post the result as the `local-gate` commit status on the
#                  HEAD the steps checked (needs `gh` and a clean working
#                  tree; refuses if HEAD or the tree changes during the run)
#
# Exit: 0 gate passed; 1 a blocking step failed; 2 usage, preflight, git or
#   missing-tool failure; 3 posting the commit status failed; 4 HEAD or the
#   working tree changed during a --post-status run, so nothing was posted.
#
# Test: scripts/gate-selftest.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

GH_REPO="bobmatnyc/trusty-git-analytics"
STATUS_CONTEXT="local-gate"
MSRV="1.94"                       # ci.yml `msrv` job toolchain
SEMVER_CHECKS_PIN="0.50.0"        # semver.yml install-action pin
BASE_REF="origin/main"
AUDIT_UI_DIST="crates/trusty-audit/ui/dist"

usage() {
    echo "Usage: scripts/gate.sh [--all] [--post-status]"
    echo "  --all          run every path-conditional step, whatever the diff says"
    echo "  --post-status  post the result as the '${STATUS_CONTEXT}' commit status on HEAD"
}

ALL=0
POST=0
for arg in "$@"; do
    case "${arg}" in
        --all) ALL=1 ;;
        --post-status) POST=1 ;;
        -h | --help) usage; exit 0 ;;
        *) echo "gate: unknown argument '${arg}'" >&2; usage >&2; exit 2 ;;
    esac
done

# Print a command, then run it, so the log shows exactly what ran.
x() {
    printf '+ %s\n' "$*"
    "$@"
}

# ---------------------------------------------------------------------------
# Which path-conditional steps run.
# ---------------------------------------------------------------------------
if ! MERGE_BASE="$(git merge-base "${BASE_REF}" HEAD 2>/dev/null)"; then
    echo "gate: no merge-base with ${BASE_REF}; run 'git fetch origin main' first." >&2
    exit 2
fi
# Each git call is checked on its own: a failed diff must never read as an
# empty change list that skips every path step.
if ! DIFF_NAMES="$(git diff --name-only "${MERGE_BASE}")"; then
    echo "gate: 'git diff --name-only ${MERGE_BASE}' failed; cannot tell which paths changed." >&2
    exit 2
fi
if ! UNTRACKED="$(git ls-files --others --exclude-standard)"; then
    echo "gate: 'git ls-files --others' failed; cannot tell which paths changed." >&2
    exit 2
fi
CHANGED="$(printf '%s\n%s\n' "${DIFF_NAMES}" "${UNTRACKED}" | sort -u)"

changed() {
    [[ ${ALL} -eq 1 ]] && return 0
    grep -qE "$1" <<<"${CHANGED}"
}

# `+version =` in a manifest's diff is semver.yml's pull_request selection.
# Called in `&&` context, where errexit is off, so a git failure exits here.
version_bumped() {
    local diff
    if ! diff="$(git diff "${MERGE_BASE}" -- "$1")"; then
        echo "gate: 'git diff ${MERGE_BASE} -- $1' failed; cannot tell whether its version changed." >&2
        exit 2
    fi
    grep -q '^+version[[:space:]]*=' <<<"${diff}"
}

RUN_WEBSITE=0
RUN_INSTALL_SH=0
RUN_PINS=0
SEMVER_PKGS=""
changed '^website/' && RUN_WEBSITE=1
changed '^(install\.sh|scripts/install-sh-selftest\.sh)$' && RUN_INSTALL_SH=1
changed '^(scripts/check-engagement-pins(-selftest)?\.sh|crates/trusty-audit/templates/engagement\.template\.toml)$' \
    && RUN_PINS=1
RUN_GATE_SELFTEST=0
changed '^scripts/gate[^/]*\.sh$' && RUN_GATE_SELFTEST=1
# Recursion guard: a nested gate must not start the selftest that started it.
# gate-selftest.sh writes a random nonce to a file in its own temp dir, which
# it deletes on exit, and passes both to each gate it runs. Only a nonce that
# matches a live file counts, so a stray exported value never skips the step.
NESTED=0
if [[ -n "${GATE_SELFTEST_NONCE:-}" ]]; then
    if [[ -f "${GATE_SELFTEST_NONCE_FILE:-}" \
        && "$(cat "${GATE_SELFTEST_NONCE_FILE}")" == "${GATE_SELFTEST_NONCE}" ]]; then
        NESTED=1
    else
        echo "gate: ignoring GATE_SELFTEST_NONCE: no live gate-selftest.sh run owns it." >&2
    fi
fi
if [[ ${ALL} -eq 1 ]]; then
    SEMVER_PKGS="tga trusty-audit"
else
    version_bumped Cargo.toml && SEMVER_PKGS="tga"
    version_bumped crates/trusty-audit/Cargo.toml && SEMVER_PKGS="${SEMVER_PKGS:+${SEMVER_PKGS} }trusty-audit"
fi

# ---------------------------------------------------------------------------
# Preflight: every tool a step that will run needs. No fail-open.
# ---------------------------------------------------------------------------
MISSING=()
need() {
    command -v "$1" >/dev/null 2>&1 || MISSING+=("$1 -- install: $2")
}

need git "xcode-select --install"
need cargo "https://rustup.rs"
need rustup "https://rustup.rs"
if command -v rustup >/dev/null 2>&1; then
    TOOLCHAINS="$(rustup toolchain list 2>/dev/null)" || TOOLCHAINS=""
    HAVE_MSRV=0
    while IFS= read -r tc; do
        [[ "${tc}" == "${MSRV}-"* ]] && HAVE_MSRV=1
    done <<<"${TOOLCHAINS}"
    if [[ ${HAVE_MSRV} -eq 0 ]]; then
        MISSING+=("rust toolchain ${MSRV} (MSRV step) -- install: rustup toolchain install ${MSRV} --profile minimal")
    fi
fi
if [[ ${RUN_WEBSITE} -eq 1 ]]; then
    need node "brew install node@20"
    need pnpm "corepack enable"
    PNPM_PIN="$(sed -n 's/.*"packageManager"[[:space:]]*:[[:space:]]*"pnpm@\([0-9][0-9.]*\).*/\1/p' website/package.json)"
    if [[ -z "${PNPM_PIN}" ]]; then
        MISSING+=("pnpm pin -- no pnpm@<version> in website/package.json packageManager")
    elif command -v pnpm >/dev/null 2>&1; then
        PNPM_FOUND="$(cd website && pnpm --version 2>/dev/null | tail -n 1)" || PNPM_FOUND=""
        if [[ "${PNPM_FOUND}" != "${PNPM_PIN}" ]]; then
            MISSING+=("pnpm ${PNPM_PIN} (website/package.json pin; found '${PNPM_FOUND}') -- install: corepack prepare pnpm@${PNPM_PIN} --activate")
        fi
    fi
fi
if [[ ${RUN_INSTALL_SH} -eq 1 || ${RUN_PINS} -eq 1 ]]; then
    need shellcheck "brew install shellcheck"
fi
if [[ ${RUN_GATE_SELFTEST} -eq 1 && ${NESTED} -eq 0 ]]; then
    need jq "brew install jq"
fi
if [[ -n "${SEMVER_PKGS}" ]]; then
    need curl "brew install curl"
    need jq "brew install jq"
    need cargo-semver-checks "cargo install cargo-semver-checks@${SEMVER_CHECKS_PIN} --locked"
fi
if [[ ${POST} -eq 1 ]]; then
    need gh "brew install gh && gh auth login"
fi

if [[ ${#MISSING[@]} -gt 0 ]]; then
    echo "gate: FAIL -- missing required tools; no step ran:" >&2
    for m in "${MISSING[@]}"; do echo "  - ${m}" >&2; done
    [[ ${POST} -eq 1 ]] && echo "gate: no commit status was posted." >&2
    exit 2
fi

# The commit and tree the steps check. --post-status compares both again
# before posting, so a status never names a commit the steps did not check.
if ! HEAD_SHA="$(git rev-parse HEAD)" || ! START_STATUS="$(git status --porcelain)"; then
    echo "gate: git rev-parse/status failed; no step ran." >&2
    exit 2
fi
if [[ ${POST} -eq 1 && -n "${START_STATUS}" ]]; then
    echo "gate: --post-status needs a clean tree: the status is posted on HEAD and" >&2
    echo "      must describe HEAD. Commit your changes first. No step ran." >&2
    exit 2
fi

# ---------------------------------------------------------------------------
# Steps. Each runs in a subshell with errexit, so its first failing command
# ends it; the gate itself keeps going.
# ---------------------------------------------------------------------------
step_fmt() { x cargo fmt --all --check; }
step_clippy() { x cargo clippy --workspace --exclude trusty-audit-ui --all-targets -- -D warnings; }
step_test() { x cargo test --workspace --exclude trusty-audit-ui; }
step_msrv() { x cargo "+${MSRV}" check --workspace --exclude trusty-audit-ui; }

# Port of ci.yml `dependency-duplicates`.
step_dep_dupes() {
    local out rc
    set +e
    out="$(cargo tree --workspace --exclude trusty-audit-ui -d 2>&1)"
    rc=$?
    set -e
    echo "${out}"
    if [ "${rc}" -ne 0 ]; then
        echo "error: cargo tree -d exited ${rc} -- could not compute the dependency graph." >&2
        return 1
    fi
    if echo "${out}" | grep -E '^(ratatui|crossterm|libsqlite3-sys) v'; then
        echo "error: Duplicate ratatui, crossterm, or libsqlite3-sys versions in the dependency graph. tga and trusty-audit must agree on one ratatui/crossterm major and one rusqlite/libsqlite3-sys version -- see ci.yml's dependency-duplicates comment and docs/release-assets.md." >&2
        return 1
    fi
    echo "OK -- no duplicate ratatui, crossterm, or libsqlite3-sys version in the graph."
}

step_audit_ui_clippy() { x env SKIP_UI_BUILD=1 cargo clippy -p trusty-audit-ui --all-targets -- -D warnings; }
step_audit_ui_test() { x env SKIP_UI_BUILD=1 cargo test -p trusty-audit-ui; }

step_website_test() {
    cd website
    x node --version
    x pnpm install --frozen-lockfile
    x pnpm run check
    x pnpm exec playwright install --with-deps chromium
    x pnpm run build
    x pnpm run test
}
step_website_lint() {
    cd website
    x pnpm install --frozen-lockfile
    x pnpm run lint
}

step_install_sh() {
    x sh -n install.sh
    x shellcheck --shell=sh install.sh
    x shellcheck --shell=bash scripts/install-sh-selftest.sh
    x bash scripts/install-sh-selftest.sh
}

step_gate_selftest() { x bash scripts/gate-selftest.sh; }

step_engagement_pins() {
    x shellcheck scripts/check-engagement-pins.sh
    x shellcheck scripts/check-engagement-pins-selftest.sh
    x bash scripts/check-engagement-pins-selftest.sh
}

# Port of semver.yml's check: compare each package against its greatest
# non-yanked, non-prerelease crates.io release strictly below its declared
# version. HTTP 404 (never published) is a clean skip.
step_semver() {
    local rc=0 pkg manifest current code baseline
    # Not `local`: the EXIT trap fires after the function returns, and this
    # whole step runs in its own subshell, so nothing leaks.
    versions="$(mktemp)"
    trap 'rm -f "${versions}"' EXIT
    x cargo semver-checks --version
    for pkg in ${SEMVER_PKGS}; do
        case "${pkg}" in
            tga) manifest="Cargo.toml" ;;
            trusty-audit) manifest="crates/trusty-audit/Cargo.toml" ;;
            *) echo "error: unknown package '${pkg}'." >&2; return 1 ;;
        esac
        current="$(grep -m1 -E '^version[[:space:]]*=' "${manifest}" | sed -E 's/^version[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/')"
        echo "== ${pkg} (current: ${current}) =="
        code="$(curl -sS -A "trusty-git-analytics gate (github.com/${GH_REPO})" -o "${versions}" \
            -w '%{http_code}' "https://crates.io/api/v1/crates/${pkg}/versions")" || code=000
        if [[ "${code}" == "404" ]]; then
            echo "SKIP ${pkg}: not on crates.io (never published) -- no baseline to compare."
            continue
        fi
        if [[ "${code}" != "200" ]]; then
            echo "error: crates.io answered HTTP ${code} for ${pkg}; could not read a baseline." >&2
            rc=1
            continue
        fi
        baseline="$(jq -r '.versions[] | select(.yanked == false) | select(.num | test("-") | not) | .num' "${versions}" \
            | sort -V \
            | awk -v cur="${current}" '
                function verlt(a, b,   na, nb, i) {
                  n1 = split(a, na, ".");
                  n2 = split(b, nb, ".");
                  for (i = 1; i <= (n1 > n2 ? n1 : n2); i++) {
                    x = (i <= n1) ? na[i] + 0 : 0;
                    y = (i <= n2) ? nb[i] + 0 : 0;
                    if (x != y) return (x < y);
                  }
                  return 0;
                }
                verlt($0, cur) { best = $0 }
                END { if (best != "") print best }
              ')"
        if [[ -z "${baseline}" ]]; then
            echo "SKIP ${pkg}: no non-yanked release below ${current} -- no baseline exists."
            continue
        fi
        echo "Comparing ${pkg} ${baseline} -> ${current}"
        if ! x cargo semver-checks check-release --package "${pkg}" \
            --baseline-version "${baseline}" --only-explicit-features; then
            echo "error: SemVer break: ${pkg} has a breaking public-API change relative to ${baseline}." >&2
            rc=1
        fi
    done
    return "${rc}"
}

# ---------------------------------------------------------------------------
# Runner and summary.
# ---------------------------------------------------------------------------
RESULTS=()
NPASS=0
NFAIL=0
NSKIP=0
SEMVER_FAILED=0

record() { RESULTS+=("$(printf '  %-20s %-32s %s' "$2" "$1" "$3")"); }

# run_step <name> <function> [report-only]
run_step() {
    local name="$1" fn="$2" mode="${3:-blocking}" start rc
    printf '\n==> %s\n' "${name}"
    start=${SECONDS}
    set +e
    (
        IN_STEP=1
        set -euo pipefail
        "${fn}"
    )
    rc=$?
    set -e
    local took="$((SECONDS - start))s"
    if [[ ${rc} -eq 0 ]]; then
        record "${name}" PASS "${took}"
        NPASS=$((NPASS + 1))
    elif [[ "${mode}" == "report-only" ]]; then
        record "${name}" "REPORT-ONLY(failed)" "${took}  not blocking"
        SEMVER_FAILED=1
    else
        record "${name}" FAIL "${took}"
        NFAIL=$((NFAIL + 1))
    fi
}

skip_step() {
    record "$1" "SKIPPED(path)" "no matching change vs ${BASE_REF}"
    NSKIP=$((NSKIP + 1))
}

# The Tauri shell embeds ui/dist at compile time; CI stubs it. Stub it only
# when absent, and remove only what this run created.
IN_STEP=0
STUBBED_DIST=0
STUBBED_DIST_DIR=0
cleanup() {
    [[ ${IN_STEP} -eq 1 ]] && return 0
    if [[ ${STUBBED_DIST} -eq 1 ]]; then
        rm -f "${AUDIT_UI_DIST}/index.html"
        [[ ${STUBBED_DIST_DIR} -eq 1 ]] && rmdir "${AUDIT_UI_DIST}" 2>/dev/null
    fi
    return 0
}
trap cleanup EXIT

run_step fmt step_fmt
run_step clippy step_clippy
run_step test step_test
run_step "msrv (${MSRV})" step_msrv
run_step dependency-duplicates step_dep_dupes

if [[ ! -e "${AUDIT_UI_DIST}/index.html" ]]; then
    [[ -d "${AUDIT_UI_DIST}" ]] || STUBBED_DIST_DIR=1
    mkdir -p "${AUDIT_UI_DIST}"
    printf '<!doctype html><html><head><meta charset="utf-8"><title>trusty-audit</title></head><body></body></html>\n' \
        >"${AUDIT_UI_DIST}/index.html"
    STUBBED_DIST=1
fi
run_step audit-ui-clippy step_audit_ui_clippy
run_step audit-ui-test step_audit_ui_test
cleanup
STUBBED_DIST=0

if [[ ${RUN_WEBSITE} -eq 1 ]]; then
    run_step website-test step_website_test
    run_step website-lint step_website_lint
else
    skip_step website-test
    skip_step website-lint
fi
if [[ ${RUN_INSTALL_SH} -eq 1 ]]; then run_step install-sh step_install_sh; else skip_step install-sh; fi
if [[ ${RUN_PINS} -eq 1 ]]; then run_step engagement-pins step_engagement_pins; else skip_step engagement-pins; fi
if [[ ${RUN_GATE_SELFTEST} -eq 0 ]]; then
    skip_step gate-selftest
elif [[ ${NESTED} -eq 1 ]]; then
    record gate-selftest "SKIPPED(nested)" "this gate was started by gate-selftest.sh"
else
    run_step gate-selftest step_gate_selftest
fi
if [[ -n "${SEMVER_PKGS}" ]]; then
    run_step "semver-checks (${SEMVER_PKGS})" step_semver report-only
else
    skip_step semver-checks
fi

echo
echo "== local gate summary: HEAD ${HEAD_SHA:0:12}, base ${MERGE_BASE:0:12}$([[ ${ALL} -eq 1 ]] && echo ', --all') =="
for line in "${RESULTS[@]}"; do echo "${line}"; done
DESC="${NPASS} passed, ${NFAIL} failed, ${NSKIP} skipped by path"
[[ ${SEMVER_FAILED} -eq 1 ]] && DESC="${DESC}; semver-checks failed (report-only)"
if [[ ${NFAIL} -eq 0 ]]; then
    STATE=success
    echo "GATE: PASS (${DESC})"
else
    STATE=failure
    echo "GATE: FAIL (${DESC})"
fi

if [[ ${POST} -eq 1 ]]; then
    END_HEAD="$(git rev-parse HEAD)" || END_HEAD="(git rev-parse failed)"
    END_STATUS="$(git status --porcelain)" || END_STATUS="(git status failed)"
    if [[ "${END_HEAD}" != "${HEAD_SHA}" || "${END_STATUS}" != "${START_STATUS}" ]]; then
        echo "gate: HEAD or the working tree changed during the run (HEAD ${HEAD_SHA:0:12} -> ${END_HEAD:0:12})." >&2
        echo "      The steps may not have checked what HEAD names now. Nothing was posted." >&2
        exit 4
    fi
    if ! OUT="$(gh api --method POST "repos/${GH_REPO}/statuses/${HEAD_SHA}" \
        -f state="${STATE}" -f context="${STATUS_CONTEXT}" \
        -f description="scripts/gate.sh: ${DESC}" 2>&1)"; then
        echo "gate: posting the ${STATUS_CONTEXT} status failed; nothing was posted:" >&2
        echo "${OUT}" >&2
        exit 3
    fi
    echo "Posted ${STATUS_CONTEXT}=${STATE} on ${HEAD_SHA}."
fi

[[ ${NFAIL} -eq 0 ]] || exit 1
