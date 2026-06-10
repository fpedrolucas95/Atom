#!/usr/bin/env bash
# scripts/ci/build-all.sh
#
# Canonical, single-command build of every critical Atom OS crate plus the full
# bootable image. Each crate is built with the target its own .cargo/config
# selects, so nothing compiles via a hidden manual path: if a critical crate
# stops compiling, this script (and therefore CI) fails.
#
# Usage:
#   ./scripts/ci/build-all.sh            # build every critical crate + image
#   ./scripts/ci/build-all.sh --crates  # build/check critical crates only

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
cd "$REPO_ROOT"

CRATES_ONLY=0
[[ "${1:-}" == "--crates" ]] && CRATES_ONLY=1

step "Building host-target workspace crates (kernel, shared, libs)..."
# The workspace members build for x86_64-unknown-uefi via the root .cargo/config.
cargo build --workspace --release
ok "workspace crates built"

step "Building host tools..."
for crate in "${CRITICAL_HOST_CRATES[@]}"; do
  ( cd "$crate" && cargo build --release )
  ok "built $crate"
done

step "Building bare-metal (x86_64-unknown-none) crates..."
for crate in "${CRITICAL_BAREMETAL_CRATES[@]}"; do
  step "  -> $crate"
  ( cd "$crate" && cargo build --release )
  ok "built $crate"
done

if [[ "$CRATES_ONLY" -eq 1 ]]; then
  ok "All critical crates compiled."
  exit 0
fi

step "Building full bootable image via build.sh..."
./build.sh
ok "Full image built. All critical crates participate in the pipeline."
