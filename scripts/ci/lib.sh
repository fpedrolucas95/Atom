#!/usr/bin/env bash
# scripts/ci/lib.sh
# Shared helpers and the canonical list of critical crates for the Atom OS
# security pipeline. Sourced by the other ci/ scripts.

set -euo pipefail

# Repository root (this file lives in scripts/ci/).
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# ── Output helpers ──────────────────────────────────────────────────────────
c_red='\033[0;31m'; c_grn='\033[0;32m'; c_cyn='\033[0;36m'; c_yel='\033[0;33m'; c_off='\033[0m'
step()    { echo -e "${c_cyn}[*]${c_off} $*"; }
ok()      { echo -e "${c_grn}[OK]${c_off} $*"; }
warn()    { echo -e "${c_yel}[!]${c_off} $*"; }
fail()    { echo -e "${c_red}[X]${c_off} $*"; }
die()     { fail "$*"; exit 1; }

# ── Critical crates ─────────────────────────────────────────────────────────
# These crates participate in boot and/or in the security TCB. Every one of
# them MUST be visible to the pipeline (no manual-only build paths).
#
# Crates buildable on the host UEFI target via the root workspace + build-std.
CRITICAL_WORKSPACE_CRATES=(
  "shared/abi"
  "shared/atxf"
  "kernel"
  "userspace/libs/syscall"
  "userspace/libs/libipc"
)

# Bare-metal (x86_64-unknown-none) crates, each built from its own directory so
# its .cargo/config.toml (PIE linker + build-std) applies.
CRITICAL_BAREMETAL_CRATES=(
  "userspace/services/init"
  "userspace/services/namesvc"
  "userspace/services/service_manager"
  "userspace/services/fsd"
  "userspace/services/app_launcher"
  "userspace/services/netd"
  "userspace/services/nic_driver"
  "userspace/system_apps/ui_shell"
  "userspace/system_apps/display"
  "userspace/system_apps/display_settings"
  "userspace/system_apps/terminal"
  "userspace/apps/security_smoke"
)

# Host-tool crates.
CRITICAL_HOST_CRATES=(
  "tools/elf2atxf"
)

all_critical_crates() {
  printf '%s\n' \
    "${CRITICAL_WORKSPACE_CRATES[@]}" \
    "${CRITICAL_BAREMETAL_CRATES[@]}" \
    "${CRITICAL_HOST_CRATES[@]}"
}
