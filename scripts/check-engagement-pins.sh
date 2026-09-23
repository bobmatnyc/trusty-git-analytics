#!/usr/bin/env bash
#
# check-engagement-pins.sh — hold the engagement template's tool pins to
# versions crates.io actually serves.
#
# Why: `crates/trusty-audit/templates/engagement.template.toml` pins the four
#   tools an engagement runs (`tga`, `trusty-search`, `trusty-analyze`,
#   `trusty-review`) as literal versions. The file is `include_str!`-ed into
#   `instructions::ENGAGEMENT_TEMPLATE` and written out verbatim by
#   `taudit distribute`, so a bad pin ships inside the binary and into every
#   client package built from it. No Rust constant mirrors the versions
#   (`grounding::Tools::pinned` carries binary PATHS, not versions), so this
#   file is the one place a pin lives.
#
#   Ported from trusty-tools `scripts/refresh-engagement-pins.sh` plus
#   `scripts/preflight-publish.sh` CHECK 10 (trusty-tools#6772). Those compared
#   each pin with a workspace version, which built a pin to a version that was
#   never published: the template split out of trusty-tools pinning tga 7.1.1
#   and trusty-search 0.54.0, and crates.io has neither. Here only tga is in
#   the workspace, so the rule is anchored on crates.io instead.
#
# What: for each pin in the ACTIVE `[tools]` table —
#   1. EXISTS. The pinned version must be published and not yanked on
#      crates.io. A pin that names anything else cannot be installed, so it is
#      a FAIL (exit 1).
#   2. TARGET. `tga`'s target is the root Cargo.toml `[package]` version when
#      that version is on crates.io, else crates.io's newest stable tga (the
#      workspace version has not shipped yet). Every other tool's target is
#      crates.io's newest stable version.
#   3. A pin that exists but differs from its target is a WARN, never a FAIL:
#      an engagement may pin an older published set on purpose.
#
#   `--refresh` rewrites every pin that differs from its target, touching only
#   the version literal, so a second run is a no-op. Both pin spellings the
#   template documents are handled:
#
#       tga = "7.1.0"
#       trusty-review = { version = "0.35.0", sha256 = "…" }
#
#   Fails closed: a missing or empty `[tools]` table, an unreadable root
#   version, or a crates.io answer this script cannot read is never a pass.
#
# Usage:
#   scripts/check-engagement-pins.sh [--refresh] [--repo <dir>]
#
#   --refresh      rewrite stale pins to their targets instead of reporting.
#   --repo <dir>   operate on a different repo root (self-test only).
#   -h|--help      print this header and exit 0.
#
# Exit codes: 0 = every pin is published (WARN lines may note a lag), or
#   `--refresh` wrote/left the template. 1 = at least one pin names a version
#   crates.io does not serve. 2 = usage error or unreadable template/manifest.
#   3 = crates.io could not be asked, so nothing was verified.
#
# Environment: CRATES_IO_API overrides https://crates.io/api/v1 (self-test).
#
# Test: scripts/check-engagement-pins-selftest.sh drives every arm against a
#   stub `curl` with no network.

set -euo pipefail

for arg in "$@"; do
  case "$arg" in
    -h|--help)
      sed -n '2,/^$/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
  esac
done

usage() {
  echo "usage: scripts/check-engagement-pins.sh [--refresh] [--repo <dir>]" >&2
  exit 2
}

REFRESH=0
REPO_ARG=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --refresh) REFRESH=1; shift ;;
    --repo)
      [ "$#" -ge 2 ] || usage
      REPO_ARG="$2"
      shift 2
      ;;
    *)
      echo "check-engagement-pins: unknown argument: $1" >&2
      usage
      ;;
  esac
done

if [ -n "$REPO_ARG" ]; then
  REPO_ROOT="$(cd "$REPO_ARG" && pwd)"
else
  REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi

TEMPLATE_REL="crates/trusty-audit/templates/engagement.template.toml"
TEMPLATE="${REPO_ROOT}/${TEMPLATE_REL}"
ROOT_MANIFEST="${REPO_ROOT}/Cargo.toml"
API="${CRATES_IO_API:-https://crates.io/api/v1}"
UA="trusty-git-analytics check-engagement-pins (github.com/bobmatnyc/trusty-git-analytics)"

[ -f "$TEMPLATE" ] || { echo "check-engagement-pins: ERROR: no template at ${TEMPLATE}" >&2; exit 2; }
[ -f "$ROOT_MANIFEST" ] || { echo "check-engagement-pins: ERROR: no manifest at ${ROOT_MANIFEST}" >&2; exit 2; }

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/check-engagement-pins.XXXXXX")"
trap 'rm -rf "$SCRATCH"' EXIT

# ---------------------------------------------------------------------------
# The ACTIVE [tools] table as `pkg<TAB>version` lines. `in_tools` flips only on
# a bare `[tools]` header at column 0, so the commented `# [tools]` digest
# example is never entered, and the table ends at the next real header.
# ---------------------------------------------------------------------------
PINS="${SCRATCH}/pins.tsv"
awk '
  /^\[/ { in_tools = ($0 == "[tools]"); next }
  !in_tools { next }
  /^[[:space:]]*#/ { next }
  match($0, /^[A-Za-z0-9_.-]+/) {
    name = substr($0, RSTART, RLENGTH)
    rest = substr($0, RSTART + RLENGTH)
    if (rest !~ /^[[:space:]]*=/) next
    # Inline-table spelling: narrow to the inner `version` key first, so a
    # sha256 digest that follows it is never mistaken for the version.
    if (rest ~ /^[[:space:]]*=[[:space:]]*\{/) {
      if (!match(rest, /version[[:space:]]*=[[:space:]]*"[^"]*"/)) next
      rest = substr(rest, RSTART, RLENGTH)
    }
    sub(/^[^"]*"/, "", rest)
    sub(/".*$/, "", rest)
    if (rest != "") printf "%s\t%s\n", name, rest
  }
' "$TEMPLATE" > "$PINS"

if [ ! -s "$PINS" ]; then
  echo "check-engagement-pins: ERROR: ${TEMPLATE_REL} has no readable pins in a" >&2
  echo "       [tools] table. The table is REQUIRED (the template says there is no" >&2
  echo "       \"latest\"), so an empty or missing one is a template defect." >&2
  exit 2
fi

# tga's own version: the first `version = "…"` inside the root [package] table.
WORKSPACE_TGA="$(awk '
  /^\[/ { in_pkg = ($0 == "[package]"); next }
  in_pkg && /^version[[:space:]]*=/ {
    v = $0; sub(/^[^"]*"/, "", v); sub(/".*$/, "", v); print v; exit
  }
' "$ROOT_MANIFEST")"
if [ -z "$WORKSPACE_TGA" ]; then
  echo "check-engagement-pins: ERROR: no [package] version in ${ROOT_MANIFEST}" >&2
  exit 2
fi

# ---------------------------------------------------------------------------
# crates.io. `fetch <url> <out>` prints the HTTP status; 000 on transport error.
# ---------------------------------------------------------------------------
fetch() {
  local code
  code="$(curl -sS -A "$UA" --connect-timeout 10 --max-time 30 -o "$2" -w '%{http_code}' "$1" \
    2> "${SCRATCH}/curl-err.txt")" || code=000
  echo "$code"
}

# `published <crate> <version>`: prints ok / missing / yanked / error:<code>.
published() {
  local body="${SCRATCH}/version.json" code
  code="$(fetch "${API}/crates/$1/$2" "$body")"
  case "$code" in
    200)
      if python3 -c 'import json,sys; sys.exit(0 if json.load(open(sys.argv[1]))["version"]["yanked"] else 1)' "$body" 2>/dev/null; then
        echo yanked
      else
        echo ok
      fi
      ;;
    404) echo missing ;;
    *) echo "error:${code}" ;;
  esac
}

# `newest_stable <crate>`: prints crates.io's max_stable_version, or nothing.
newest_stable() {
  local body="${SCRATCH}/crate.json" code
  code="$(fetch "${API}/crates/$1" "$body")"
  [ "$code" = "200" ] || return 1
  python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["crate"]["max_stable_version"] or "")' "$body" 2>/dev/null
}

RESULTS="${SCRATCH}/results.tsv"   # pkg, pinned, target, state
: > "$RESULTS"
FAILS=0
UNVERIFIED=0

while IFS=$'\t' read -r name pinned; do
  [ -n "$name" ] || continue

  state="$(published "$name" "$pinned")"
  target="$(newest_stable "$name" || true)"
  if [ -z "$target" ]; then
    echo "UNVERIFIED ${name}: crates.io did not report a newest stable version (${API}/crates/${name})"
    UNVERIFIED=$((UNVERIFIED + 1))
    continue
  fi

  note=""
  if [ "$name" = "tga" ]; then
    ws_state="$(published tga "$WORKSPACE_TGA")"
    case "$ws_state" in
      ok) target="$WORKSPACE_TGA" ;;
      missing) note=" (workspace tga ${WORKSPACE_TGA} is not on crates.io yet, so the target is the newest published tga)" ;;
      yanked) note=" (workspace tga ${WORKSPACE_TGA} is yanked, so the target is the newest published tga)" ;;
      *)
        echo "UNVERIFIED tga: crates.io answered ${ws_state#error:} for the workspace version ${WORKSPACE_TGA}"
        UNVERIFIED=$((UNVERIFIED + 1))
        continue
        ;;
    esac
  fi

  case "$state" in
    ok)
      if [ "$pinned" = "$target" ]; then
        echo "OK    ${name} ${pinned}"
      else
        echo "WARN  ${name} pinned=${pinned} target=${target} — published, but not the target${note}"
      fi
      ;;
    missing|yanked)
      echo "FAIL  ${name} pinned=${pinned} target=${target} — ${pinned} is ${state} on crates.io, so it cannot be installed${note}"
      FAILS=$((FAILS + 1))
      ;;
    *)
      echo "UNVERIFIED ${name}: crates.io answered ${state#error:} for ${name} ${pinned}"
      UNVERIFIED=$((UNVERIFIED + 1))
      continue
      ;;
  esac
  printf '%s\t%s\t%s\t%s\n' "$name" "$pinned" "$target" "$state" >> "$RESULTS"
done < "$PINS"

if [ "$UNVERIFIED" -gt 0 ]; then
  echo "check-engagement-pins: ${UNVERIFIED} pin(s) could not be verified against ${API}." >&2
  echo "       Refusing to pass (or rewrite) what was not checked." >&2
  exit 3
fi

if [ "$REFRESH" -eq 0 ]; then
  if [ "$FAILS" -gt 0 ]; then
    echo "check-engagement-pins: ${FAILS} pin(s) in ${TEMPLATE_REL} name a version crates.io" >&2
    echo "       does not serve. Fix: scripts/check-engagement-pins.sh --refresh, or edit" >&2
    echo "       the pin to a published version, then rebuild trusty-audit." >&2
    exit 1
  fi
  echo "check-engagement-pins: OK — every [tools] pin in ${TEMPLATE_REL} is published."
  exit 0
fi

# ---------------------------------------------------------------------------
# --refresh: rewrite the version literal on each pin whose target differs.
# ---------------------------------------------------------------------------
UPDATED="${SCRATCH}/engagement.template.toml"
awk -v want_file="$RESULTS" '
  BEGIN {
    while ((getline line < want_file) > 0) {
      n = split(line, f, "\t")
      if (n >= 3) want[f[1]] = f[3]
    }
  }
  /^\[/ { in_tools = ($0 == "[tools]"); print; next }
  !in_tools || /^[[:space:]]*#/ { print; next }
  {
    if (!match($0, /^[A-Za-z0-9_.-]+/)) { print; next }
    name = substr($0, RSTART, RLENGTH)
    if (!(name in want)) { print; next }
    head = substr($0, 1, RSTART + RLENGTH - 1)
    rest = substr($0, RSTART + RLENGTH)
    if (rest !~ /^[[:space:]]*=/) { print; next }
    if (rest ~ /^[[:space:]]*=[[:space:]]*\{/) {
      if (!match(rest, /version[[:space:]]*=[[:space:]]*"[^"]*"/)) { print; next }
      pre = substr(rest, 1, RSTART - 1)
      mid = substr(rest, RSTART, RLENGTH)
      post = substr(rest, RSTART + RLENGTH)
      sub(/"[^"]*"$/, "\"" want[name] "\"", mid)
      print head pre mid post
    } else {
      if (!match(rest, /"[^"]*"/)) { print; next }
      pre = substr(rest, 1, RSTART - 1)
      post = substr(rest, RSTART + RLENGTH)
      print head pre "\"" want[name] "\"" post
    }
  }
' "$TEMPLATE" > "$UPDATED"

if cmp -s "$TEMPLATE" "$UPDATED"; then
  echo "check-engagement-pins: no change — every [tools] pin already names its target."
  exit 0
fi

cp "$UPDATED" "$TEMPLATE"
echo "check-engagement-pins: rewrote ${TEMPLATE_REL}:"
while IFS=$'\t' read -r name pinned target _state; do
  [ "$pinned" = "$target" ] && continue
  echo "  ${name}: ${pinned} -> ${target}"
done < "$RESULTS"
echo "  Rebuild trusty-audit so instructions::ENGAGEMENT_TEMPLATE picks up the new bytes."
exit 0
