#!/bin/bash
# build.sh
# Script de build otimizado para o kernel Atom no Linux (ambiente Claude)
# Uso: ./build.sh [--run] [--clean] [--rust-only] [--setup]

set -e

# Cores para output
RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

function step {
    echo -e "${CYAN}[*] $1${NC}"
}

function success {
    echo -e "${GREEN}[✓] $1${NC}"
}

function warning {
    echo -e "${YELLOW}[!] $1${NC}"
}

function error {
    echo -e "${RED}[✗] $1${NC}"
}

function info {
    echo -e "${CYAN}[i] $1${NC}"
}

# Verifica se estamos no diretório correto
if [ ! -f "kernel/Cargo.toml" ]; then
    error "Este script deve ser executado na raiz do repositório Atom"
    exit 1
fi

# Parse argumentos
RUN=false
CLEAN=false
RUST_ONLY=false
SETUP=false

for arg in "$@"; do
    case $arg in
        --run)
            RUN=true
            ;;
        --clean)
            CLEAN=true
            ;;
        --rust-only)
            RUST_ONLY=true
            ;;
        --setup)
            SETUP=true
            ;;
        --help|-h)
            echo "Uso: ./build.sh [opções]"
            echo ""
            echo "Opções:"
            echo "  --clean       Limpar arquivos de build antes de compilar"
            echo "  --rust-only   Compilar apenas o código Rust (sem NASM/linker)"
            echo "  --setup       Configurar dependências do Rust"
            echo "  --run         Executar no QEMU após build"
            echo "  --help, -h    Mostrar esta ajuda"
            echo ""
            echo "Exemplos:"
            echo "  ./build.sh --rust-only    # Apenas validar código Rust"
            echo "  ./build.sh --setup        # Configurar toolchain"
            echo "  ./build.sh --clean --run  # Build completo e executar"
            exit 0
            ;;
    esac
done

# ============================================================================
# SETUP: Configurar dependências Rust
# ============================================================================

if [ "$SETUP" = true ]; then
    step "Configurando toolchain Rust..."

    # Verificar se rust-toolchain.toml existe
    if [ ! -f "rust-toolchain.toml" ]; then
        echo '[toolchain]' > rust-toolchain.toml
        echo 'channel = "nightly"' >> rust-toolchain.toml
        success "rust-toolchain.toml criado"
    fi

    # Adicionar rust-src se necessário
    if ! rustup component list --installed | grep -q "rust-src"; then
        step "Adicionando rust-src..."
        rustup component add rust-src
        success "rust-src adicionado"
    else
        info "rust-src já instalado"
    fi

    # Adicionar target UEFI se necessário
    if ! rustup target list --installed | grep -q "x86_64-unknown-uefi"; then
        step "Adicionando target x86_64-unknown-uefi..."
        rustup target add x86_64-unknown-uefi
        success "Target x86_64-unknown-uefi adicionado"
    else
        info "Target x86_64-unknown-uefi já instalado"
    fi

    success "Setup concluído!"
    exit 0
fi

# ============================================================================
# AUTO-SETUP: Verificar e configurar se necessário
# ============================================================================

# Verificar rust-toolchain.toml
if [ ! -f "rust-toolchain.toml" ]; then
    warning "rust-toolchain.toml não encontrado, criando..."
    echo '[toolchain]' > rust-toolchain.toml
    echo 'channel = "nightly"' >> rust-toolchain.toml
fi

# Verificar rust-src
if ! rustup component list --installed 2>/dev/null | grep -q "rust-src"; then
    warning "rust-src não encontrado, adicionando..."
    rustup component add rust-src 2>/dev/null || true
fi

# Verificar target UEFI
if ! rustup target list --installed 2>/dev/null | grep -q "x86_64-unknown-uefi"; then
    warning "Target x86_64-unknown-uefi não encontrado, adicionando..."
    rustup target add x86_64-unknown-uefi 2>/dev/null || true
fi

# ============================================================================
# CLEAN
# ============================================================================

if [ "$CLEAN" = true ]; then
    step "Limpando arquivos de build..."
    rm -rf build/* 2>/dev/null || true
    cargo clean 2>/dev/null || true
    success "Arquivos limpos"
fi

# Cria diretório build se não existir
mkdir -p build

# ============================================================================
# BUILD RUST KERNEL
# ============================================================================

step "Compilando kernel Rust..."
if cargo build -p atom-kernel --release 2>&1 | tee build/cargo.log; then
    success "Kernel Rust compilado com sucesso"

    # Mostrar warnings se houver
    if grep -q "warning:" build/cargo.log; then
        warning "Build teve warnings (veja build/cargo.log para detalhes)"
    fi
else
    error "Falha ao compilar kernel Rust"
    cat build/cargo.log
    exit 1
fi

# Se --rust-only, parar aqui
if [ "$RUST_ONLY" = true ]; then
    echo ""
    success "Build Rust-only concluído com sucesso!"
    info "Arquivo gerado: target/x86_64-unknown-uefi/release/libatom.a"
    exit 0
fi

# ============================================================================
# VERIFICAR NASM (OPCIONAL)
# ============================================================================

HAS_NASM=false
if command -v nasm &> /dev/null; then
    HAS_NASM=true
else
    warning "NASM não encontrado - pulando assembly e linking"
    warning "Para build completo, instale NASM: sudo apt install nasm"
    echo ""
    success "Build Rust concluído (sem assembly/linking)"
    info "Use './build.sh --rust-only' para compilação rápida"
    exit 0
fi

# ============================================================================
# MONTAR ARQUIVOS ASSEMBLY
# ============================================================================

step "Montando boot.asm com NASM..."
if nasm -f win64 arch/x86_64/boot.asm -o build/boot.obj 2>build/nasm.log; then
    success "boot.obj criado"
else
    error "Falha ao montar boot.asm"
    cat build/nasm.log
    exit 1
fi

step "Montando handlers.asm com NASM..."
rm -f build/handlers.obj
if nasm -f win64 kernel/src/interrupts/handlers.asm -o build/handlers.obj 2>build/nasm_handlers.log; then
    success "handlers.obj criado"
else
    error "Falha ao montar handlers.asm"
    cat build/nasm_handlers.log
    exit 1
fi

step "Montando switch.asm com NASM..."
if nasm -f win64 kernel/src/interrupts/switch.asm -o build/switch.obj 2>build/nasm_switch.log; then
    success "switch.obj criado"
else
    error "Falha ao montar switch.asm"
    cat build/nasm_switch.log
    exit 1
fi

step "Montando syscall handler.asm com NASM..."
if nasm -f win64 kernel/src/syscall/handler.asm -o build/syscall_handler.obj 2>build/nasm_syscall.log; then
    success "syscall_handler.obj criado"
else
    error "Falha ao montar handler.asm"
    cat build/nasm_syscall.log
    exit 1
fi

# ============================================================================
# LINKAR ATOM.EFI
# ============================================================================

step "Linkando Atom.efi..."

# Find rust-lld
RUST_LLD=$(find ~/.rustup/toolchains/nightly-*/lib/rustlib/*/bin/rust-lld 2>/dev/null | head -1)
if [ -z "$RUST_LLD" ]; then
    warning "rust-lld não encontrado"
    warning "Tentando usar lld-link do sistema..."
    RUST_LLD="lld-link"
fi

if "$RUST_LLD" -flavor link \
    build/boot.obj \
    build/handlers.obj \
    build/switch.obj \
    build/syscall_handler.obj \
    target/x86_64-unknown-uefi/release/libatom.a \
    /OUT:build/Atom.efi \
    /SUBSYSTEM:EFI_APPLICATION \
    /ENTRY:efi_entry \
    /NODEFAULTLIB 2>build/link.log; then
    success "Atom.efi criado"
else
    error "Falha ao linkar Atom.efi"
    cat build/link.log
    exit 1
fi

# ============================================================================
# COPIAR PARA EFI BOOT
# ============================================================================

step "Copiando para efi/EFI/BOOT/BOOTX64.EFI..."
mkdir -p efi/EFI/BOOT
cp build/Atom.efi efi/EFI/BOOT/BOOTX64.EFI
success "BOOTX64.EFI atualizado"

echo ""
success "Build completo concluído com sucesso!"
info "Imagem EFI: efi/EFI/BOOT/BOOTX64.EFI"
echo ""

# ============================================================================
# EXECUTAR QEMU (OPCIONAL)
# ============================================================================

if [ "$RUN" = true ]; then
    step "Iniciando QEMU..."

    OVMF_PATH="/usr/share/OVMF/OVMF_CODE.fd"
    if [ ! -f "$OVMF_PATH" ]; then
        OVMF_PATH="/usr/share/edk2-ovmf/x64/OVMF_CODE.fd"
    fi

    if [ ! -f "$OVMF_PATH" ]; then
        warning "OVMF.fd não encontrado"
        warning "Para executar no QEMU, instale: sudo apt install ovmf qemu-system-x86"
        exit 0
    fi

    if ! command -v qemu-system-x86_64 &> /dev/null; then
        warning "qemu-system-x86_64 não encontrado"
        warning "Instale com: sudo apt install qemu-system-x86"
        exit 0
    fi

    echo -e "${YELLOW}Pressione Ctrl+A X para sair do QEMU${NC}"
    echo -e "${YELLOW}Serial output será exibido abaixo:${NC}"
    echo ""
    echo "=========================================="

    qemu-system-x86_64 \
        -machine q35 \
        -cpu qemu64 \
        -m 512M \
        -bios "$OVMF_PATH" \
        -drive format=raw,file=fat:rw:efi \
        -serial stdio \
        -nographic \
        -no-reboot
else
    echo -e "${YELLOW}Para testar no QEMU: ./build.sh --run${NC}"
    echo -e "${YELLOW}Para build rápido:   ./build.sh --rust-only${NC}"
fi
