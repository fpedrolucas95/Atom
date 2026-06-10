#!/usr/bin/env bash
# scripts/ci/geiger-baseline.sh
#
# Enforce an `unsafe` baseline across the critical crates so the amount of
# unsafe code in the TCB cannot grow silently. Counts `unsafe` tokens under each
# crate's src/ and fails if the live count EXCEEDS the committed baseline in
# scripts/ci/unsafe_baseline.txt. Decreases are always allowed.
#
# This is a deterministic, dependency-free proxy for `cargo geiger` that runs in
# any CI runner. When cargo-geiger is installed, security-checks.sh additionally
# produces the full geiger report as an artifact; this gate is the hard fail.

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
cd "$REPO_ROOT"

BASELINE="scripts/ci/unsafe_baseline.txt"
[[ -f "$BASELINE" ]] || die "baseline not found: $BASELINE"

count_unsafe() {
  local crate="$1"
  if [[ -d "$crate/src" ]]; then
    # `|| true` so a zero-match grep (exit 1) does not trip set -e/pipefail.
    { grep -rwoE "unsafe" "$crate/src" 2>/dev/null || true; } | wc -l | tr -d ' '
  else
    echo 0
  fi
}

step "Enforcing unsafe baseline for critical crates..."
violations=0
while read -r max crate; do
  [[ -z "${max:-}" || "$max" == \#* ]] && continue
  live="$(count_unsafe "$crate")"
  if (( live > max )); then
    fail "unsafe grew in $crate: baseline=$max live=$live (+$((live - max)))"
    violations=$((violations + 1))
  else
    printf '    %-44s baseline=%-4s live=%s\n' "$crate" "$max" "$live"
  fi
done < "$BASELINE"

if (( violations > 0 )); then
  echo ""
  fail "unsafe count increased in $violations crate(s) above baseline."
  echo "If the new unsafe is justified, raise the number in $BASELINE and"
  echo "document the justification in SECURITY_DEBT.md (the diff is the audit"
  echo "trail). Silent growth of unsafe in the TCB is not allowed."
  exit 1
fi

ok "unsafe baseline satisfied (no unjustified growth)"

# Optional richer report when cargo-geiger is available (non-fatal: the hard
# gate above already ran).
if command -v cargo-geiger >/dev/null 2>&1; then
  step "cargo-geiger available — emitting workspace report"
  cargo geiger --workspace --output-format Ascii || warn "cargo geiger report failed (non-fatal; baseline gate already enforced)"
else
  warn "cargo-geiger not installed; deterministic baseline gate used instead"
fi
