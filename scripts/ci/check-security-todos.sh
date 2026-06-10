#!/usr/bin/env bash
# scripts/ci/check-security-todos.sh
#
# Fail the build when a security-relevant TODO/FIXME or an insecure-shortcut
# phrase is committed without explicit tracking. A loose comment is not an
# acceptable home for a security exception: it must either be fixed, carry a
# tracking reference (issue id `#123`, `ATOM-123`, or the token SECURITY_DEBT),
# or be recorded in SECURITY_DEBT.md and exempted via .security-debt-allowlist.
#
# Scans tracked source only (kernel/, userspace/, shared/, tools/, arch/,
# build.sh). The pipeline-definition files (scripts/ci, .semgrep, docs,
# SECURITY*.md) legitimately mention these phrases and are not scanned.

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
cd "$REPO_ROOT"

# Forbidden patterns (case-insensitive, ERE). `\bprivileged process` uses a word
# boundary so it does NOT match the legitimate word "unprivileged".
PATTERNS='TODO[ :_-]*security|TODO[ :_-]*auth|TODO[ :_-]*cap|TODO[ :_-]*unsafe|FIXME[ :_-]*security|temporary allow|debug bypass|allow unsigned|fallback insecure|\bprivileged process'

# Tracking tokens that make an in-code exception acceptable.
TRACKING='#[0-9]+|ATOM-[0-9]+|SECURITY_DEBT'

SCAN_PATHS=(kernel userspace shared tools arch build.sh)

ALLOWLIST_FILE=".security-debt-allowlist"
mapfile -t ALLOW < <(grep -vE '^\s*#|^\s*$' "$ALLOWLIST_FILE" 2>/dev/null || true)

step "Scanning for untracked security debt..."
violations=0

# Collect matches, skipping build artifacts.
while IFS= read -r line; do
  [[ -z "$line" ]] && continue
  # line is "path:lineno:content"
  content="${line#*:*:}"

  # Exempt if a tracking token is present on the line.
  if grep -qE "$TRACKING" <<<"$content"; then
    continue
  fi

  # Exempt if an allowlist substring is present on the line.
  exempt=0
  for sub in "${ALLOW[@]}"; do
    if [[ "$content" == *"$sub"* ]]; then
      exempt=1
      break
    fi
  done
  [[ "$exempt" -eq 1 ]] && continue

  fail "untracked security debt: $line"
  violations=$((violations + 1))
done < <(grep -rniE "$PATTERNS" "${SCAN_PATHS[@]}" 2>/dev/null | grep -v '/target/' || true)

if [[ "$violations" -gt 0 ]]; then
  echo ""
  fail "$violations untracked security-debt marker(s) found."
  echo "Resolve them, add a tracking ref (#123 / ATOM-123 / SECURITY_DEBT),"
  echo "or record the exception in SECURITY_DEBT.md and .security-debt-allowlist."
  exit 1
fi

ok "no untracked security debt"
