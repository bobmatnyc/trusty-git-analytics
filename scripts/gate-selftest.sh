#!/usr/bin/env bash
# shellcheck disable=SC2016  # stub bodies are single-quoted on purpose: they expand when the stub runs.
# Selftest for scripts/gate.sh, the local PR gate.
#
# Why: the gate replaced CI for PRs, so its value is in its failure arms. A
#   gate that swallowed a failing step, skipped a step whose tool is missing,
#   or let a report-only semver failure block would still print a tidy
#   summary. Only a test that drives each arm catches that.
#
# What: builds a synthetic git repo holding a copy of the gate, with stub
#   `cargo`, `rustup`, `pnpm`, `node`, `shellcheck`, `curl`, `gh` and
#   `cargo-semver-checks` on a PATH of stubs plus /usr/bin:/bin. No network,
#   no real build. Each case asserts the exit status and the summary line.
#   The stub `cargo` logs every invocation, so the ci.yml flags are asserted
#   too.
#
# Usage: scripts/gate-selftest.sh
#   GATE_SCRIPT=<path> runs the cases against another copy of the gate (used
#   to prove this selftest fails when the gate swallows a failure).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="${GATE_SCRIPT:-${REPO_ROOT}/scripts/gate.sh}"
[[ -f "${GATE}" ]] || { echo "FATAL: no gate at ${GATE}" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

PASS=0
FAIL=0
pass() { echo "  PASS: $1"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL: $1" >&2; FAIL=$((FAIL + 1)); }

# ---------------------------------------------------------------------------
# Stubs. STUB_FAIL=<substring>: any stubbed command whose argv contains it
# exits 1. Every call is appended to STUB_LOG.
# ---------------------------------------------------------------------------
STUBS="${WORK}/stubs"
mkdir -p "${STUBS}"
write_stub() {
    local name="$1" body="$2"
    cat >"${STUBS}/${name}" <<STUBEOF
#!/bin/bash
echo "${name} \$*" >>"\${STUB_LOG}"
if [[ -n "\${STUB_FAIL:-}" && "${name} \$*" == *"\${STUB_FAIL}"* ]]; then
    echo "stub ${name}: failing on request" >&2
    exit 1
fi
${body}
exit 0
STUBEOF
    chmod +x "${STUBS}/${name}"
}
# STUB_MIDRUN=commit|dirty: the test step moves HEAD or dirties the tree
# while the gate runs.
write_stub cargo 'case "$1" in
    tree) printf "%s" "${STUB_TREE_OUT:-}" ;;
    semver-checks) [[ "$2" == "--version" ]] && echo "cargo-semver-checks 0.50.0" ;;
    test)
        case "${STUB_MIDRUN:-}" in
            commit) git -c user.name=t -c user.email=t@example.com commit -q --allow-empty -m midrun ;;
            dirty) echo midrun >>midrun.txt ;;
        esac ;;
esac'
write_stub cargo-semver-checks ''
write_stub rustup '[[ "$1" == toolchain ]] && { echo "stable-aarch64-apple-darwin (default)"; [[ -z "${STUB_NO_MSRV:-}" ]] && echo "1.94-aarch64-apple-darwin"; }'
write_stub pnpm '[[ "$1" == --version ]] && echo 9.15.9'
write_stub node '[[ "$1" == --version ]] && echo v20.0.0'
write_stub shellcheck ''
write_stub gh 'exit "${STUB_GH_RC:-0}"'
# crates.io: tga has 1.0.0 and 1.1.0 published; anything else is a 404.
write_stub curl 'out=""; url=""
while [[ $# -gt 0 ]]; do
    case "$1" in -o) out="$2"; shift 2 ;; -A | -w) shift 2 ;; http*) url="$1"; shift ;; *) shift ;; esac
done
if [[ "${url}" == */crates/tga/versions ]]; then
    echo "{\"versions\":[{\"num\":\"1.1.0\",\"yanked\":false},{\"num\":\"1.0.0\",\"yanked\":false}]}" >"${out}"
    printf 200
else
    echo "{}" >"${out}"
    printf 404
fi'
# The gate's semver step needs a real jq, and PATH below holds only the stubs
# plus /usr/bin:/bin.
REAL_JQ="$(command -v jq)" || { echo "FATAL: jq is required (brew install jq)" >&2; exit 1; }
ln -s "${REAL_JQ}" "${STUBS}/jq"
# git passes through to the real git, but STUB_FAIL can make one call fail.
REAL_GIT="$(command -v git)" || { echo "FATAL: git is required" >&2; exit 1; }
write_stub git "exec \"${REAL_GIT}\" \"\$@\""

# The recursion-guard nonce. It lives only as long as this run: the EXIT trap
# removes WORK, so a stale exported value names a file that no longer exists.
NONCE="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
NONCE_FILE="${WORK}/gate-nonce"
printf '%s' "${NONCE}" >"${NONCE_FILE}"
STRAY_ENV=()

# Same stubs without pnpm, for the missing-tool case.
NOPNPM="${WORK}/stubs-nopnpm"
cp -R "${STUBS}" "${NOPNPM}"
rm "${NOPNPM}/pnpm"

# make_repo <name>: a committed repo whose origin/main is its own HEAD.
make_repo() {
    local dir="${WORK}/$1"
    mkdir -p "${dir}/scripts" "${dir}/website" "${dir}/crates/trusty-audit"
    cp "${GATE}" "${dir}/scripts/gate.sh"
    printf '[package]\nname = "tga"\nversion = "1.1.0"\n' >"${dir}/Cargo.toml"
    printf '[package]\nname = "trusty-audit"\nversion = "0.2.0"\n' >"${dir}/crates/trusty-audit/Cargo.toml"
    printf '{\n  "packageManager": "pnpm@9.15.9+sha512.abc"\n}\n' >"${dir}/website/package.json"
    printf '#!/bin/sh\nexit 0\n' >"${dir}/install.sh"
    for s in install-sh-selftest check-engagement-pins check-engagement-pins-selftest; do
        printf '#!/usr/bin/env bash\nexit 0\n' >"${dir}/scripts/${s}.sh"
    done
    printf '#!/usr/bin/env bash\necho "gate-selftest ran" >>"${STUB_LOG}"\n' >"${dir}/scripts/gate-selftest.sh"
    git -C "${dir}" init -q
    git -C "${dir}" add -A
    git -C "${dir}" -c user.name=t -c user.email=t@example.com commit -q -m init
    git -C "${dir}" update-ref refs/remotes/origin/main HEAD
    echo "${dir}"
}

# run_gate <stub dir> <repo> [gate args...]; sets OUT, STATUS, LOG.
# Every run carries this selftest's live nonce (the gate's recursion guard)
# unless a case sets SELFTEST_NESTED=0 to prove the gate-selftest step runs.
# STRAY_ENV adds variables as if exported by the caller's shell; any value
# inherited from the real environment is removed first.
run_gate() {
    local stubs="$1" repo="$2" nest=()
    shift 2
    LOG="${WORK}/calls.log"
    : >"${LOG}"
    if [[ "${SELFTEST_NESTED:-1}" == "1" ]]; then
        nest=(GATE_SELFTEST_NONCE="${NONCE}" GATE_SELFTEST_NONCE_FILE="${NONCE_FILE}")
    fi
    set +e
    OUT="$(env -u GATE_SELFTEST_NONCE -u GATE_SELFTEST_NONCE_FILE -u GATE_NESTED \
        PATH="${stubs}:/usr/bin:/bin" ${nest[@]+"${nest[@]}"} ${STRAY_ENV[@]+"${STRAY_ENV[@]}"} \
        STUB_LOG="${LOG}" STUB_FAIL="${STUB_FAIL:-}" STUB_MIDRUN="${STUB_MIDRUN:-}" \
        STUB_TREE_OUT="${STUB_TREE_OUT:-}" STUB_NO_MSRV="${STUB_NO_MSRV:-}" STUB_GH_RC="${STUB_GH_RC:-0}" \
        bash "${repo}/scripts/gate.sh" "$@" 2>&1)"
    STATUS=$?
    set -e
}

# has_line <regex>: the gate output has a line matching the ERE.
has_line() { grep -qE "$1" <<<"${OUT}"; }
logged() { grep -qF -- "$1" "${LOG}"; }

echo "== gate selftest =="

# 1 -- everything passes under --all; the ci.yml commands run with their flags.
repo="$(make_repo pass)"
run_gate "${STUBS}" "${repo}" --all
if [[ ${STATUS} -eq 0 ]] && has_line '^GATE: PASS' && ! has_line 'FAIL' \
    && has_line '^  PASS +semver-checks \(tga trusty-audit\)' && has_line '^  PASS +website-lint' \
    && has_line '^  SKIPPED\(nested\) +gate-selftest' && ! logged "gate-selftest ran" \
    && logged "cargo fmt --all --check" \
    && logged "cargo clippy --workspace --exclude trusty-audit-ui --all-targets -- -D warnings" \
    && logged "cargo test --workspace --exclude trusty-audit-ui" \
    && logged "cargo +1.94 check --workspace --exclude trusty-audit-ui" \
    && logged "cargo clippy -p tga --no-default-features --all-targets -- -D warnings" \
    && logged "cargo test -p tga --no-default-features" \
    && logged "cargo tree --workspace --exclude trusty-audit-ui -d" \
    && logged "cargo clippy -p trusty-audit-ui --all-targets -- -D warnings" \
    && logged "cargo test -p trusty-audit-ui" \
    && logged "pnpm install --frozen-lockfile" \
    && logged "shellcheck --shell=sh install.sh" \
    && logged "cargo semver-checks check-release --package tga --baseline-version 1.0.0 --only-explicit-features"; then
    pass "--all with every step green exits 0, runs the ci.yml commands, and a nested gate skips gate-selftest"
else
    fail "all-green run: status ${STATUS}"$'\n'"${OUT}"
fi
if [[ ! -e "${repo}/crates/trusty-audit/ui/dist/index.html" ]]; then
    pass "the stub ui/dist/index.html is removed after the run"
else
    fail "the gate left its ui/dist stub behind"
fi

# 2 -- one failing step makes the gate exit non-zero, and later steps still run.
STUB_FAIL="clippy --workspace" run_gate "${STUBS}" "${repo}" --all
if [[ ${STATUS} -ne 0 ]] && has_line '^  FAIL +clippy' && has_line '^  PASS +test ' \
    && has_line '^GATE: FAIL'; then
    pass "a failing clippy step exits non-zero and the test step still runs"
else
    fail "failing step: status ${STATUS}"$'\n'"${OUT}"
fi

# 3 -- a duplicate ratatui in cargo tree -d fails dependency-duplicates.
STUB_TREE_OUT=$'ratatui v0.28.1\nratatui v0.29.0\n' run_gate "${STUBS}" "${repo}"
if [[ ${STATUS} -ne 0 ]] && has_line '^  FAIL +dependency-duplicates'; then
    pass "a duplicate ratatui version fails dependency-duplicates"
else
    fail "duplicate ratatui: status ${STATUS}"$'\n'"${OUT}"
fi

# 4 -- a semver-checks failure alone is report-only: exit 0.
STUB_FAIL="check-release" run_gate "${STUBS}" "${repo}" --all
if [[ ${STATUS} -eq 0 ]] && has_line '^  REPORT-ONLY\(failed\) +semver-checks' \
    && has_line '^GATE: PASS'; then
    pass "a semver-checks failure alone exits 0 and shows REPORT-ONLY"
else
    fail "report-only semver: status ${STATUS}"$'\n'"${OUT}"
fi

# 5 -- a missing MSRV toolchain fails preflight; no cargo step runs.
STUB_NO_MSRV=1 run_gate "${STUBS}" "${repo}"
if [[ ${STATUS} -ne 0 ]] && has_line 'rust toolchain 1\.94 .*rustup toolchain install 1\.94' \
    && ! logged "cargo fmt"; then
    pass "a missing 1.94 toolchain exits non-zero before any step"
else
    fail "missing toolchain: status ${STATUS}"$'\n'"${OUT}"
fi

# 6 -- a missing pnpm fails preflight when the website steps must run.
run_gate "${NOPNPM}" "${repo}" --all
if [[ ${STATUS} -ne 0 ]] && has_line '^  - pnpm -- install:' && ! logged "cargo fmt"; then
    pass "a missing pnpm exits non-zero with an install hint"
else
    fail "missing pnpm: status ${STATUS}"$'\n'"${OUT}"
fi

# 7 -- path conditions: an install.sh edit and a tga version bump select
#      their steps; untouched paths are listed as SKIPPED(path).
repo7="$(make_repo paths)"
run_gate "${STUBS}" "${repo7}"
if [[ ${STATUS} -eq 0 ]] && has_line '^  SKIPPED\(path\) +install-sh' \
    && has_line '^  SKIPPED\(path\) +semver-checks' && has_line '^  SKIPPED\(path\) +gate-selftest' \
    && ! logged "shellcheck"; then
    pass "an empty diff skips every path-conditional step"
else
    fail "empty diff: status ${STATUS}"$'\n'"${OUT}"
fi
printf '#!/bin/sh\n# edited\nexit 0\n' >"${repo7}/install.sh"
sed -i.bak 's/^version = "1.1.0"/version = "1.2.0"/' "${repo7}/Cargo.toml" && rm "${repo7}/Cargo.toml.bak"
run_gate "${STUBS}" "${repo7}"
if [[ ${STATUS} -eq 0 ]] && has_line '^  PASS +install-sh' && has_line '^  PASS +semver-checks \(tga\)' \
    && has_line '^  SKIPPED\(path\) +website-test' && has_line '^  SKIPPED\(path\) +engagement-pins' \
    && logged "--baseline-version 1.1.0"; then
    pass "changed paths select install-sh and semver-checks (tga) only"
else
    fail "path selection: status ${STATUS}"$'\n'"${OUT}"
fi

# 7b -- a scripts/gate*.sh edit runs gate-selftest when the gate is not
#       nested (the outer, real gate's view of this same change).
repo7b="$(make_repo gate-edit)"
echo '# edited' >>"${repo7b}/scripts/gate-selftest.sh"
SELFTEST_NESTED=0 run_gate "${STUBS}" "${repo7b}"
if [[ ${STATUS} -eq 0 ]] && has_line '^  PASS +gate-selftest' && logged "gate-selftest ran" \
    && has_line '^  SKIPPED\(path\) +install-sh'; then
    pass "a scripts/gate*.sh edit runs gate-selftest in a non-nested gate"
else
    fail "gate-selftest step: status ${STATUS}"$'\n'"${OUT}"
fi

# 8 -- --post-status posts on HEAD; a gh failure exits non-zero and never
#      claims it posted; a dirty tree is refused.
sha="$(git -C "${repo}" rev-parse HEAD)"
run_gate "${STUBS}" "${repo}" --post-status
if [[ ${STATUS} -eq 0 ]] && logged "gh api --method POST repos/bobmatnyc/trusty-git-analytics/statuses/${sha} -f state=success -f context=local-gate" \
    && has_line "^Posted local-gate=success on ${sha}"; then
    pass "--post-status posts local-gate=success on HEAD"
else
    fail "post success: status ${STATUS}"$'\n'"${OUT}"
fi
STUB_FAIL="clippy --workspace" run_gate "${STUBS}" "${repo}" --post-status
if [[ ${STATUS} -eq 1 ]] && logged "-f state=failure -f context=local-gate"; then
    pass "--post-status posts failure for a failed gate and exits 1"
else
    fail "post failure: status ${STATUS}"$'\n'"${OUT}"
fi
STUB_GH_RC=1 run_gate "${STUBS}" "${repo}" --post-status
if [[ ${STATUS} -ne 0 ]] && has_line 'nothing was posted' && ! has_line '^Posted'; then
    pass "a gh failure exits non-zero and does not claim a post"
else
    fail "gh failure: status ${STATUS}"$'\n'"${OUT}"
fi
run_gate "${STUBS}" "${repo7}" --post-status
if [[ ${STATUS} -ne 0 ]] && has_line 'needs a clean tree' && ! logged "gh api"; then
    pass "--post-status refuses a dirty tree"
else
    fail "dirty tree: status ${STATUS}"$'\n'"${OUT}"
fi

# 9 -- HEAD moving or the tree changing mid-run: --post-status posts nothing.
for how in commit dirty; do
    repo9="$(make_repo "midrun-${how}")"
    STUB_MIDRUN="${how}" run_gate "${STUBS}" "${repo9}" --post-status
    if [[ ${STATUS} -ne 0 ]] && has_line 'changed during the run' && ! logged "gh api" \
        && ! has_line '^Posted'; then
        pass "a mid-run ${how} makes --post-status exit non-zero and post nothing"
    else
        fail "mid-run ${how}: status ${STATUS}"$'\n'"${OUT}"
    fi
done

# 10 -- a failing `git diff --name-only` is fatal, never an empty change list.
STUB_FAIL="diff --name-only" run_gate "${STUBS}" "${repo}"
if [[ ${STATUS} -ne 0 ]] && has_line "git diff --name-only .*failed" && ! logged "cargo fmt"; then
    pass "a failing git diff for the changed paths exits non-zero before any step"
else
    fail "changed-paths git failure: status ${STATUS}"$'\n'"${OUT}"
fi

# 11 -- a failing manifest diff is fatal, never a silent semver skip.
STUB_FAIL="-- Cargo.toml" run_gate "${STUBS}" "${repo}"
if [[ ${STATUS} -ne 0 ]] && has_line "Cargo\.toml' failed" && ! logged "cargo fmt"; then
    pass "a failing git diff for a manifest version exits non-zero before any step"
else
    fail "version-bump git failure: status ${STATUS}"$'\n'"${OUT}"
fi

# 12 -- a stray exported guard value never skips gate-selftest: a stale nonce
#       file, a wrong token, and the old GATE_NESTED=1 all still run it.
repo12="$(make_repo stray-guard)"
echo '# edited' >>"${repo12}/scripts/gate-selftest.sh"
for stray in "stale nonce file|GATE_SELFTEST_NONCE_FILE=${WORK}/no-such-nonce GATE_SELFTEST_NONCE=${NONCE}" \
    "wrong nonce token|GATE_SELFTEST_NONCE_FILE=${NONCE_FILE} GATE_SELFTEST_NONCE=wrong-token" \
    "GATE_NESTED=1|GATE_NESTED=1"; do
    read -r -a STRAY_ENV <<<"${stray#*|}"
    SELFTEST_NESTED=0 run_gate "${STUBS}" "${repo12}"
    STRAY_ENV=()
    if [[ ${STATUS} -eq 0 ]] && has_line '^  PASS +gate-selftest' && logged "gate-selftest ran"; then
        pass "a stray ${stray%%|*} does not skip gate-selftest"
    else
        fail "stray ${stray%%|*}: status ${STATUS}"$'\n'"${OUT}"
    fi
done

echo "== ${PASS} passed, ${FAIL} failed =="
[[ ${FAIL} -eq 0 ]]
