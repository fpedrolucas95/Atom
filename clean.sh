#!/bin/bash
# clean.sh
# Atom OS - Clean Script (macOS / Linux)

set -e

# ===============================
# Cores
# ===============================

RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
YELLOW='\033[0;33m'
MAGENTA='\033[0;35m'
NC='\033[0m'

function header {
    echo ""
    echo -e "${MAGENTA}===============================${NC}"
    echo -e "${MAGENTA} Atom OS - Clean Script${NC}"
    echo -e "${MAGENTA}===============================${NC}"
    echo ""
}

function safe_remove {
    if [ -e "$1" ]; then
        echo -e "${CYAN}Removendo:${NC} $1"
        rm -rf "$1"
    fi
}

function remove_file {
    if [ -f "$1" ]; then
        echo -e "${CYAN}Removendo arquivo:${NC} $1"
        rm -f "$1"
    fi
}

header

ROOT=$(pwd)

# =========================
# Diretórios principais
# =========================

MAIN_DIRS=(
    "$ROOT/target"
    "$ROOT/build"
    "$ROOT/images"
    "$ROOT/EFI"
    "$ROOT/efi/EFI"
    "$ROOT/efi/drivers"
    "$ROOT/ovmf/build"
)

for dir in "${MAIN_DIRS[@]}"; do
    safe_remove "$dir"
done

# =========================
# Remove todos target/
# =========================

echo -e "${YELLOW}Procurando pastas target/...${NC}"
find "$ROOT" -type d -name "target" -exec rm -rf {} + 2>/dev/null || true

# =========================
# Remove artefatos Cargo
# =========================

BUILD_ARTIFACTS=(
    ".fingerprint"
    "incremental"
    "build"
    "deps"
    "examples"
)

for artifact in "${BUILD_ARTIFACTS[@]}"; do
    find "$ROOT" -type d -name "$artifact" -exec rm -rf {} + 2>/dev/null || true
done

# =========================
# Tools
# =========================

safe_remove "$ROOT/tools/elf2atxf/target"
safe_remove "$ROOT/build/userspace"

# =========================
# Logs
# =========================

echo -e "${YELLOW}Removendo logs (*.log)...${NC}"
find "$ROOT" -type f -name "*.log" -exec rm -f {} + 2>/dev/null || true

# =========================
# Locks (exceto Cargo.lock)
# =========================

echo -e "${YELLOW}Removendo .lock (exceto Cargo.lock)...${NC}"
find "$ROOT" -type f -name "*.lock" ! -name "Cargo.lock" -exec rm -f {} + 2>/dev/null || true

# =========================
# Limpeza explícita UEFI
# =========================

safe_remove "$ROOT/efi/build"
safe_remove "$ROOT/efi/EFI/BOOT"
safe_remove "$ROOT/efi/EFI/Atom"
safe_remove "$ROOT/efi/drivers/build"

# =========================
# Final
# =========================

echo ""
echo -e "${GREEN}===============================${NC}"
echo -e "${GREEN} Clean completo finalizado!${NC}"
echo -e "${GREEN} Sistema pronto para rebuild${NC}"
echo -e "${GREEN}===============================${NC}"
echo ""