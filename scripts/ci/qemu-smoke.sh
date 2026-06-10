#!/usr/bin/env bash
# scripts/ci/qemu-smoke.sh
#
# Adversarial QEMU smoke runner for the security milestone (PR1–PR5).
#
# Builds a *smoke* image (init auto-launches the unprivileged security_smoke app
# via app_launcher), boots it headless under QEMU for SMP=1, 2 and 4, and FAILS
# unless every boot:
#   * prints  SECURITY_SMOKE PASS all
#   * does NOT print  SECURITY_SMOKE FAIL
#   * does NOT print  PANIC
#   * does NOT print  Fatal kernel page fault
#
# This is a hard gate: a regression that re-opens any PR1–PR5 hole turns a
# PASS line into FAIL (or removes PASS all) and fails CI. No continue-on-error.
#
# Usage:
#   ./scripts/ci/qemu-smoke.sh                 # SMP 1,2,4 (default)
#   ./scripts/ci/qemu-smoke.sh --smp "1 2"     # custom set
#   BOOT_TIMEOUT=90 ./scripts/ci/qemu-smoke.sh # override per-boot timeout (s)

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
cd "$REPO_ROOT"

SMP_SET="1 2 4"
[[ "${1:-}" == "--smp" && -n "${2:-}" ]] && SMP_SET="$2"
BOOT_TIMEOUT="${BOOT_TIMEOUT:-90}"

command -v qemu-system-x86_64 >/dev/null 2>&1 \
  || die "qemu-system-x86_64 not installed (CI must provide QEMU)"

DISK_IMG="build/atom_disk.img"
PASS_MARKER="SECURITY_SMOKE PASS all"
FAIL_MARKERS=("SECURITY_SMOKE FAIL" "PANIC" "Fatal kernel page fault")

step "Building smoke image (SMOKE_BUILD=1 ./build.sh)..."
SMOKE_BUILD=1 ./build.sh
[[ -f "$DISK_IMG" ]] || die "disk image not produced at $DISK_IMG"

OVMF_PATH="/usr/local/share/ovmf/OVMF_CODE.fd"
[[ -f "$OVMF_PATH" ]] || OVMF_PATH="ovmf/OVMF.fd"
[[ -f "$OVMF_PATH" ]] || die "OVMF firmware not found"

run_one() {
  local smp="$1"
  local serial="build/smoke_serial_smp${smp}.log"
  local debugcon="build/smoke_debugcon_smp${smp}.log"
  rm -f "$serial" "$debugcon"

  step "Booting QEMU headless (smp=$smp, timeout=${BOOT_TIMEOUT}s)..."
  # `|| true`: QEMU is killed by the timeout once the boot has settled; the
  # verdict comes from parsing the serial logs, not the exit code.
  timeout "${BOOT_TIMEOUT}" qemu-system-x86_64 \
    -machine q35 \
    -cpu qemu64,+rdrand \
    -smp "$smp" \
    -m 512M \
    -bios "$OVMF_PATH" \
    -drive "format=raw,file=${DISK_IMG},cache=writeback" \
    -display none \
    -no-reboot \
    -serial "file:${serial}" \
    -debugcon "file:${debugcon}" \
    -global isa-debugcon.iobase=0xE9 \
    -netdev user,id=net0 \
    -device e1000,netdev=net0 \
    >/dev/null 2>&1 || true

  # Aggregate everything the guest may have printed.
  local combined
  combined="$(cat "$serial" "$debugcon" 2>/dev/null || true)"

  for bad in "${FAIL_MARKERS[@]}"; do
    if grep -qF "$bad" <<<"$combined"; then
      fail "smp=$smp: found forbidden marker: '$bad'"
      grep -nF "$bad" <<<"$combined" | head -5 | sed 's/^/      /'
      return 1
    fi
  done

  if grep -qF "$PASS_MARKER" <<<"$combined"; then
    ok "smp=$smp: '$PASS_MARKER' observed; no panic / page fault / FAIL"
    return 0
  fi

  fail "smp=$smp: '$PASS_MARKER' NOT found in serial output"
  echo "      (last 20 lines of serial:)"
  tail -20 "$serial" 2>/dev/null | sed 's/^/      /'
  return 1
}

rc=0
for smp in $SMP_SET; do
  run_one "$smp" || rc=1
done

echo ""
if [[ "$rc" -eq 0 ]]; then
  ok "QEMU adversarial smoke PASSED for SMP: $SMP_SET"
else
  die "QEMU adversarial smoke FAILED (see markers above)"
fi
