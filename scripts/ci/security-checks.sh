#!/usr/bin/env bash
# scripts/ci/security-checks.sh
#
# Canonical security gate. Runs the full set of mandatory checks. ANY failure
# fails the script (and therefore CI). There is no continue-on-error: a security
# regression must break the build.
#
# Order is deliberate: the security-specific gates (which this PR owns and keeps
# green) run FIRST, so they are validated independently of the repo-wide style
# normalization (fmt/clippy) that runs last.
#
# Checks:
#   1. syscall policy gate            (scripts/ci/check-syscall-policy.sh)
#   2. security-debt / TODO gate      (scripts/ci/check-security-todos.sh)
#   3. unsafe baseline (geiger)       (scripts/ci/geiger-baseline.sh)
#   4. cargo audit                    (advisory database)
#   5. cargo deny check               (licenses / bans / sources / advisories)
#   6. semgrep --config .semgrep/     (syscall-without-gate static analysis)
#   7. cargo fmt --check
#   8. cargo clippy --workspace --all-targets -- -D warnings
#
# In CI (CI=true) a missing external tool is a hard failure — the workflow is
# responsible for installing it. Locally, a missing optional tool is a warning
# so developers can still run the deterministic gates.

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
cd "$REPO_ROOT"

IN_CI="${CI:-false}"

require_tool() {
  local tool="$1"
  if command -v "$tool" >/dev/null 2>&1; then
    return 0
  fi
  if [[ "$IN_CI" == "true" ]]; then
    die "$tool is required in CI but not installed (the workflow must install it)"
  fi
  warn "$tool not installed; skipping locally (CI installs and runs it)"
  return 1
}

header() { echo ""; echo "=========== $* ==========="; echo ""; }

header "1/8 syscall policy gate"
bash scripts/ci/check-syscall-policy.sh

header "2/8 security-debt / TODO gate"
bash scripts/ci/check-security-todos.sh

header "3/8 unsafe baseline (geiger)"
bash scripts/ci/geiger-baseline.sh

header "4/8 cargo audit"
if require_tool cargo-audit; then
  cargo audit
  ok "cargo audit clean"
fi

header "5/8 cargo deny check"
if require_tool cargo-deny; then
  cargo deny check
  ok "cargo deny clean"
fi

header "6/8 semgrep static analysis"
if require_tool semgrep; then
  semgrep --config .semgrep/ --error --quiet
  ok "semgrep clean"
fi

header "7/8 cargo fmt --check"
cargo fmt --all --check
ok "formatting clean"

header "8/8 cargo clippy (-D warnings)"
cargo clippy --workspace --all-targets -- -D warnings
ok "clippy clean"

echo ""
ok "ALL SECURITY CHECKS PASSED"
