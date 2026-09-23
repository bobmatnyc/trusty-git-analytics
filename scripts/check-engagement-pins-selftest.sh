#!/usr/bin/env bash
# Selftest for scripts/check-engagement-pins.sh.
#
# Why: the check's value is in its failure arms — an unpublished pin, a yanked
#   pin, a crates.io it cannot read. A gate proven only on the happy path would
#   pass while each of those was broken.
#
# What: builds synthetic repos (a root Cargo.toml and an engagement template)
#   and runs the check with a stub `curl` on PATH that serves crates.io answers
#   from fixture files. No network. Each case asserts the exit status and the
#   line that names the pin.
#
# Usage: scripts/check-engagement-pins-selftest.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/check-engagement-pins.sh"
[[ -f "${SCRIPT}" ]] || { echo "FATAL: no script at ${SCRIPT}" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

PASS=0
FAIL=0
pass() { echo "  PASS: $1"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL: $1" >&2; FAIL=$((FAIL + 1)); }

# ---------------------------------------------------------------------------
# crates.io fixtures. `<crate>.json` answers /crates/<crate>;
# `<crate>@<ver>.json` answers /crates/<crate>/<ver>. Anything else is a 404.
# ---------------------------------------------------------------------------
FIX="${WORK}/fixtures"
mkdir -p "${FIX}"
crate() { printf '{"crate":{"max_stable_version":"%s"}}\n' "$2" >"${FIX}/$1.json"; }
version() { printf '{"version":{"num":"%s","yanked":%s}}\n' "$2" "$3" >"${FIX}/$1@$2.json"; }

crate tga 7.1.0
version tga 7.0.0 false
version tga 7.1.0 false
crate trusty-search 0.54.2
version trusty-search 0.54.2 false
version trusty-search 0.32.0 true
crate trusty-analyze 0.12.6
version trusty-analyze 0.12.6 false
crate trusty-review 0.36.0
version trusty-review 0.35.0 false
version trusty-review 0.36.0 false

STUBS="${WORK}/stubs"
mkdir -p "${STUBS}"
cat >"${STUBS}/curl" <<'STUBEOF'
#!/usr/bin/env bash
out=""; fmt=""; url=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -o) out="$2"; shift 2 ;;
        -w) fmt="$2"; shift 2 ;;
        -A|--connect-timeout|--max-time) shift 2 ;;
        http*) url="$1"; shift ;;
        *) shift ;;
    esac
done
code=200
if [[ -n "${STUB_STATUS:-}" ]]; then
    code="${STUB_STATUS}"
    echo '{}' >"${out}"
else
    path="${url#*/api/v1/crates/}"
    src="${STUB_FIXTURES}/${path/\//@}.json"
    if [[ -f "${src}" ]]; then cp "${src}" "${out}"; else code=404; echo '{"errors":[]}' >"${out}"; fi
fi
[[ "${fmt}" == *http_code* ]] && printf '%s' "${code}"
exit 0
STUBEOF
chmod +x "${STUBS}/curl"

# `make_repo <name> <tga-workspace-version> <tools-table-body>`
make_repo() {
    local dir="${WORK}/$1"
    mkdir -p "${dir}/crates/trusty-audit/templates"
    printf '[workspace]\nmembers = ["."]\n\n[package]\nname = "tga"\nversion = "%s"\n' "$2" >"${dir}/Cargo.toml"
    {
        printf '# header\nclient = "Acme"\n\n[tools]\n%s\n\n' "$3"
        printf '# A digest example that must never be read or rewritten:\n#\n# [tools]\n# trusty-review = { version = "0.1.0", sha256 = "abc" }\n\n[providers]\nreviewer = "x"\n'
    } >"${dir}/crates/trusty-audit/templates/engagement.template.toml"
    echo "${dir}"
}

run() {
    set +e
    OUT="$(env PATH="${STUBS}:${PATH}" STUB_FIXTURES="${FIX}" "$@" 2>&1)"
    STATUS=$?
    set -e
}

CURRENT='tga = "7.1.0"
trusty-search = "0.54.2"
trusty-analyze = "0.12.6"
trusty-review = "0.36.0"'

echo "== check-engagement-pins selftest =="

# 1 — every pin published and current.
repo="$(make_repo current 8.0.0 "${CURRENT}")"
run bash "${SCRIPT}" --repo "${repo}"
if [[ ${STATUS} -eq 0 && "${OUT}" == *"OK    trusty-review 0.36.0"* ]]; then
    pass "current pins exit 0"
else
    fail "current pins: ${STATUS} ${OUT}"
fi

# 2 — the 7.1.1 shape: a pin crates.io has never served.
repo="$(make_repo missing 8.0.0 "${CURRENT/7.1.0/7.1.1}")"
run bash "${SCRIPT}" --repo "${repo}"
if [[ ${STATUS} -eq 1 && "${OUT}" == *"FAIL  tga pinned=7.1.1 target=7.1.0"* ]]; then
    pass "an unpublished pin exits 1 and names tga"
else
    fail "unpublished pin: ${STATUS} ${OUT}"
fi
if [[ "${OUT}" == *"workspace tga 8.0.0 is not on crates.io yet"* ]]; then
    pass "an unpublished workspace tga falls back to the newest published tga"
else
    fail "workspace fallback note missing: ${OUT}"
fi

# 3 — a yanked pin cannot be installed either.
repo="$(make_repo yanked 8.0.0 "${CURRENT/0.54.2/0.32.0}")"
run bash "${SCRIPT}" --repo "${repo}"
if [[ ${STATUS} -eq 1 && "${OUT}" == *"0.32.0 is yanked"* ]]; then
    pass "a yanked pin exits 1"
else
    fail "yanked pin: ${STATUS} ${OUT}"
fi

# 4 — a published pin behind its target is a WARN, not a failure.
repo="$(make_repo lag 8.0.0 "${CURRENT/0.36.0/0.35.0}")"
run bash "${SCRIPT}" --repo "${repo}"
if [[ ${STATUS} -eq 0 && "${OUT}" == *"WARN  trusty-review pinned=0.35.0 target=0.36.0"* ]]; then
    pass "a lagging published pin warns and exits 0"
else
    fail "lagging pin: ${STATUS} ${OUT}"
fi

# 5 — a published workspace tga is the target, even below crates.io's newest.
repo="$(make_repo wspub 7.0.0 "${CURRENT}")"
run bash "${SCRIPT}" --repo "${repo}"
if [[ ${STATUS} -eq 0 && "${OUT}" == *"WARN  tga pinned=7.1.0 target=7.0.0"* ]]; then
    pass "a published workspace tga version is tga's target"
else
    fail "workspace target: ${STATUS} ${OUT}"
fi

# 6/7 — --refresh fixes both spellings, leaves the digest and the commented
# example alone, and a second run changes nothing.
INLINE='tga = "7.1.1"
trusty-search = { version = "0.54.0", sha256 = "deadbeef" }
trusty-analyze = "0.12.6"
trusty-review = "0.35.0"'
repo="$(make_repo refresh 8.0.0 "${INLINE}")"
tpl="${repo}/crates/trusty-audit/templates/engagement.template.toml"
run bash "${SCRIPT}" --repo "${repo}" --refresh
if [[ ${STATUS} -eq 0 ]] && grep -qx 'tga = "7.1.0"' "${tpl}" &&
    grep -qx 'trusty-search = { version = "0.54.2", sha256 = "deadbeef" }' "${tpl}" &&
    grep -qx 'trusty-review = "0.36.0"' "${tpl}" &&
    grep -qx '# trusty-review = { version = "0.1.0", sha256 = "abc" }' "${tpl}"; then
    pass "--refresh rewrites both spellings and nothing else"
else
    fail "--refresh: ${STATUS} ${OUT} $(cat "${tpl}")"
fi
before="$(shasum -a 256 "${tpl}")"
run bash "${SCRIPT}" --repo "${repo}" --refresh
if [[ ${STATUS} -eq 0 && "${OUT}" == *"no change"* && "$(shasum -a 256 "${tpl}")" == "${before}" ]]; then
    pass "a second --refresh is a byte-identical no-op"
else
    fail "second refresh: ${STATUS} ${OUT}"
fi
run bash "${SCRIPT}" --repo "${repo}"
if [[ ${STATUS} -eq 0 ]]; then
    pass "the refreshed template passes the check"
else
    fail "post-refresh check: ${OUT}"
fi

# 8 — an empty [tools] table and a missing template fail closed.
repo="$(make_repo empty 8.0.0 "# nothing pinned")"
run bash "${SCRIPT}" --repo "${repo}"
if [[ ${STATUS} -eq 2 ]]; then
    pass "an empty [tools] table exits 2"
else
    fail "empty table: ${STATUS} ${OUT}"
fi
mkdir -p "${WORK}/notemplate"
printf '[package]\nname = "tga"\nversion = "1.0.0"\n' >"${WORK}/notemplate/Cargo.toml"
run bash "${SCRIPT}" --repo "${WORK}/notemplate"
if [[ ${STATUS} -eq 2 ]]; then
    pass "a missing template exits 2"
else
    fail "missing template: ${STATUS} ${OUT}"
fi

# 9 — crates.io answering 500 verifies nothing, and --refresh writes nothing.
repo="$(make_repo down 8.0.0 "${CURRENT/7.1.0/7.1.1}")"
tpl="${repo}/crates/trusty-audit/templates/engagement.template.toml"
before="$(shasum -a 256 "${tpl}")"
run env STUB_STATUS=500 bash "${SCRIPT}" --repo "${repo}" --refresh
if [[ ${STATUS} -eq 3 && "$(shasum -a 256 "${tpl}")" == "${before}" ]]; then
    pass "an unreadable crates.io exits 3 and rewrites nothing"
else
    fail "crates.io down: ${STATUS} ${OUT}"
fi

echo
echo "passed: ${PASS}  failed: ${FAIL}"
[[ ${FAIL} -eq 0 ]] || exit 1
echo "OK"
