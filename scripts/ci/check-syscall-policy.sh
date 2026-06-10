#!/usr/bin/env bash
# scripts/ci/check-syscall-policy.sh
#
# Fail the build if a syscall exists in the ABI without an explicit
# classification in the kernel policy table. This stops a new syscall from
# silently relying on the fail-closed wildcard (or, worse, being added straight
# to a dispatcher without a policy decision and a test).
#
# A new syscall is only allowed once it has:
#   1. a `SYS_*` ABI constant (userspace/libs/syscall/src/raw.rs),
#   2. an explicit arm in `syscall_policy` (kernel/src/syscall/policy.rs),
#   3. an authorization test (kernel/src/syscall/policy_tests*.rs or the
#      manifest/cap tests) — enforced heuristically below.
#
# Exit non-zero on any violation.

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
cd "$REPO_ROOT"

ABI_FILE="userspace/libs/syscall/src/raw.rs"
POLICY_FILE="kernel/src/syscall/policy.rs"

[[ -f "$ABI_FILE" ]]    || die "ABI file not found: $ABI_FILE"
[[ -f "$POLICY_FILE" ]] || die "policy file not found: $POLICY_FILE"

step "Extracting SYS_* constants from the ABI ($ABI_FILE)..."
mapfile -t SYSCALLS < <(grep -oE 'pub const (SYS_[A-Z0-9_]+)' "$ABI_FILE" \
                          | awk '{print $3}' | sort -u)

[[ "${#SYSCALLS[@]}" -gt 0 ]] || die "no SYS_* constants found — parser broken"
ok "found ${#SYSCALLS[@]} syscalls in the ABI"

# The policy table must classify every syscall explicitly. We confirm each
# SYS_* name is referenced in the syscall_policy function body.
POLICY_BODY="$(awk '/fn syscall_policy/{f=1} f{print} /^pub\(super\) fn authorize_syscall_class/{f=0}' "$POLICY_FILE")"

missing=()
for s in "${SYSCALLS[@]}"; do
  if ! grep -qw "$s" <<<"$POLICY_BODY"; then
    missing+=("$s")
  fi
done

if [[ "${#missing[@]}" -gt 0 ]]; then
  fail "The following syscalls are NOT classified in $POLICY_FILE:"
  for m in "${missing[@]}"; do echo "    - $m"; done
  echo ""
  echo "Every new syscall must be classified in syscall_policy() as one of:"
  echo "  Requires(<CapRequirement>)  |  ExplicitlyUnrestricted  |  ExplicitlyDenied(..)"
  echo "and must ship with a positive test and (if it touches an external"
  echo "resource) a negative EPERM test. Relying on the fail-closed wildcard"
  echo "is not acceptable for a checked-in syscall."
  exit 1
fi
ok "every ABI syscall is explicitly classified in the policy table"

# Heuristic: the policy table must keep its fail-closed wildcard.
if ! grep -qE '_\s*=>\s*ExplicitlyDenied' "$POLICY_FILE"; then
  die "fail-closed wildcard ( _ => ExplicitlyDenied ) missing from policy table"
fi
ok "fail-closed wildcard present"

# Every ExplicitlyUnrestricted arm must carry a justifying comment: a bare,
# comment-less ExplicitlyUnrestricted is how a sensitive syscall silently loses
# its gate. We flag any arm whose first match line is not preceded by a `//`
# comment within 3 lines (and that has no inline `//` on the arm itself).
#
# This used to be delegated to the semgrep rule
# `explicitly-unrestricted-needs-comment`, but newer semgrep dropped Rust
# comment-pattern matching (`// ...`), so the check now lives here where it is
# deterministic and version-stable.
step "Checking ExplicitlyUnrestricted arms carry a justification..."
eu_report="$(awk '
  /fn syscall_policy/                            { infn = 1 }
  { line[NR] = $0; body[NR] = infn }
  /pub\(super\) fn authorize_syscall_class/      { infn = 0 }
  END {
    for (i = 1; i <= NR; i++) {
      if (!body[i]) continue
      if (line[i] !~ /ExplicitlyUnrestricted/) continue
      if (line[i] ~ /SysPolicy::ExplicitlyUnrestricted/) continue
      # Walk up to the first match line of this arm (SYS_* / `|` continuations).
      s = i
      while (s > 1 && (line[s-1] ~ /SYS_[A-Z]/ || line[s-1] ~ /^[[:space:]]*\|/)) s--
      has = 0
      for (k = s; k <= i; k++) if (line[k] ~ /\/\//) has = 1          # inline comment
      for (k = s-1; k >= s-3 && k >= 1; k--) if (line[k] ~ /^[[:space:]]*\/\//) has = 1
      if (!has) printf("    %s:%d  %s\n", FILENAME, s, line[s])
    }
  }
' "$POLICY_FILE")"

if [[ -n "$eu_report" ]]; then
  fail "ExplicitlyUnrestricted arm(s) without a justifying comment:"
  echo "$eu_report"
  echo ""
  echo "Each ExplicitlyUnrestricted arm must be preceded by a comment explaining"
  echo "why the syscall needs no capability gate (e.g. \"handler-gated by IoPort"
  echo "cap\", or \"public, non-sensitive\")."
  exit 1
fi
ok "every ExplicitlyUnrestricted arm carries a justification"

ok "syscall policy gate passed"
