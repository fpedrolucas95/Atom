#!/bin/bash
# build.sh
# Script de build para o kernel Atom no Linux/macOS
# Uso:
#   ./build.sh              # Build completo (kernel + userspace)
#   ./build.sh --clean      # Limpar e rebuildar
#   ./build.sh --run        # Build e executar no QEMU
#   ./build.sh --userspace  # Build apenas drivers userspace
#   ./build.sh --kernel     # Build apenas kernel
#   ./build.sh --rust-only  # Apenas validar código Rust
#   ./build.sh --setup      # Configurar dependências

set -e

# -------------------------------------------------------------------------
# Cores para output
# -------------------------------------------------------------------------

RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
YELLOW='\033[0;33m'
MAGENTA='\033[0;35m'
NC='\033[0m'

function step {
    echo -e "${CYAN}[*] $1${NC}"
}

function success {
    echo -e "${GREEN}[OK] $1${NC}"
}

function warning {
    echo -e "${YELLOW}[!] $1${NC}"
}

function error {
    echo -e "${RED}[X] $1${NC}"
}

function header {
    echo ""
    echo -e "${MAGENTA}========== $1 ==========${NC}"
    echo ""
}

# -------------------------------------------------------------------------
# Verificações iniciais
# -------------------------------------------------------------------------

if [ ! -f "kernel/Cargo.toml" ]; then
    error "Este script deve ser executado na raiz do repositório Atom"
    exit 1
fi

# -------------------------------------------------------------------------
# Parse argumentos
# -------------------------------------------------------------------------

RUN=false
CLEAN=false
RUST_ONLY=false
SETUP=false
USERSPACE_ONLY=false
KERNEL_ONLY=false

for arg in "$@"; do
    case $arg in
        --run)      RUN=true ;;
        --clean)    CLEAN=true ;;
        --rust-only) RUST_ONLY=true ;;
        --setup)    SETUP=true ;;
        --userspace) USERSPACE_ONLY=true ;;
        --kernel)   KERNEL_ONLY=true ;;
        --help|-h)
            echo "Uso: ./build.sh [opções]"
            echo ""
            echo "Opções:"
            echo "  --clean       Limpar arquivos de build antes de compilar"
            echo "  --run         Executar no QEMU após build"
            echo "  --userspace   Build apenas drivers userspace"
            echo "  --kernel      Build apenas kernel"
            echo "  --rust-only   Apenas validar código Rust (sem NASM/linker)"
            echo "  --setup       Configurar dependências do Rust"
            echo "  --help, -h    Mostrar esta ajuda"
            exit 0
            ;;
    esac
done

# -------------------------------------------------------------------------
# Userspace drivers list
# -------------------------------------------------------------------------

USERSPACE_DRIVERS=(
    "keyboard"
    "mouse"
    "display"
    "ui_shell"
)

# =========================================================================
# SETUP: Configurar dependências Rust
# =========================================================================

if [ "$SETUP" = true ]; then
    header "SETUP"
    
    step "Configurando toolchain Rust..."

    if [ ! -f "rust-toolchain.toml" ]; then
        echo '[toolchain]' > rust-toolchain.toml
        echo 'channel = "nightly"' >> rust-toolchain.toml
        success "rust-toolchain.toml criado"
    fi

    if ! rustup component list --installed | grep -q "rust-src"; then
        step "Adicionando rust-src..."
        rustup component add rust-src
        success "rust-src adicionado"
    else
        echo "rust-src já instalado"
    fi

    if ! rustup target list --installed | grep -q "x86_64-unknown-uefi"; then
        step "Adicionando target x86_64-unknown-uefi..."
        rustup target add x86_64-unknown-uefi
        success "Target x86_64-unknown-uefi adicionado"
    else
        echo "Target x86_64-unknown-uefi já instalado"
    fi

    success "Setup concluído!"
    exit 0
fi

# =========================================================================
# AUTO-SETUP
# =========================================================================

if [ ! -f "rust-toolchain.toml" ]; then
    warning "rust-toolchain.toml não encontrado, criando..."
    echo '[toolchain]' > rust-toolchain.toml
    echo 'channel = "nightly"' >> rust-toolchain.toml
fi

if ! rustup component list --installed 2>/dev/null | grep -q "rust-src"; then
    warning "rust-src não encontrado, adicionando..."
    rustup component add rust-src 2>/dev/null || true
fi

if ! rustup target list --installed 2>/dev/null | grep -q "x86_64-unknown-uefi"; then
    warning "Target x86_64-unknown-uefi não encontrado, adicionando..."
    rustup target add x86_64-unknown-uefi 2>/dev/null || true
fi

# =========================================================================
# CLEAN
# =========================================================================

if [ "$CLEAN" = true ]; then
    step "Limpando arquivos de build..."
    rm -rf build/* 2>/dev/null || true
    cargo clean 2>/dev/null || true
    success "Arquivos limpos"
fi

# =========================================================================
# Preparar diretórios
# =========================================================================

mkdir -p build
mkdir -p build/userspace
mkdir -p efi/EFI/BOOT
mkdir -p efi/drivers

# =========================================================================
# BUILD USERSPACE DRIVERS (Verification only - drivers are embedded in kernel)
# =========================================================================

if [ "$KERNEL_ONLY" != true ]; then
    header "USERSPACE LIBRARIES"

    # Compilar biblioteca syscall primeiro
    step "Compilando biblioteca atom_syscall..."
    
    pushd userspace/libs/syscall > /dev/null
    if cargo check 2>build.log; then
        success "atom_syscall verificada"
    else
        error "Falha ao verificar atom_syscall"
        cat build.log
        exit 1
    fi
    popd > /dev/null

    # Verificar cada driver (drivers estao excluidos do workspace principal)
    # Nota: Os drivers userspace serao compilados como binarios ATXF
    # quando o loader estiver implementado. Por enquanto, apenas verificamos.
    step "Verificando drivers userspace..."
    for driver in "${USERSPACE_DRIVERS[@]}"; do
        driver_path="userspace/drivers/$driver"
        
        if [ ! -f "$driver_path/Cargo.toml" ]; then
            warning "Driver $driver não encontrado, pulando..."
            continue
        fi

        pushd "$driver_path" > /dev/null
        if cargo check 2>/dev/null; then
            popd > /dev/null
            success "$driver driver verificado"
        else
            warning "$driver driver tem erros de sintaxe"
            popd > /dev/null
        fi
    done

    success "Verificacao de userspace concluida"
fi

# Se --userspace only, parar aqui
if [ "$USERSPACE_ONLY" = true ]; then
    echo ""
    success "Build userspace concluído!"
    exit 0
fi

# =========================================================================
# BUILD UI_SHELL AS ATXF BINARY
# =========================================================================

header "UI_SHELL BUILD"

step "Compilando ui_shell..."
pushd userspace/drivers/ui_shell > /dev/null
if cargo build --release 2>&1 | tee ../../../build/ui_shell_cargo.log; then
    success "ui_shell compilado"
else
    error "Falha ao compilar ui_shell"
    exit 1
fi
popd > /dev/null

step "Gerando binário ATXF..."
if python3 tools/build_atxf.py \
    userspace/drivers/ui_shell/target/x86_64-unknown-none/release/ui_shell \
    build/ui_shell.atxf 2>&1 | tee build/atxf.log; then
    success "ui_shell.atxf gerado"
else
    error "Falha ao gerar ATXF"
    cat build/atxf.log
    exit 1
fi

step "Copiando ui_shell.atxf para kernel/src/..."
cp build/ui_shell.atxf kernel/src/ui_shell.atxf
success "ui_shell.atxf pronto para embedding"

# =========================================================================
# BUILD KERNEL RUST
# =========================================================================

header "KERNEL BUILD"

step "Compilando kernel Rust..."
if cargo build -p atom-kernel --release 2>&1 | tee build/cargo.log; then
    success "Kernel Rust compilado"

    if grep -q "warning:" build/cargo.log; then
        warning "Build teve warnings (veja build/cargo.log)"
    fi
else
    error "Falha ao compilar kernel Rust"
    exit 1
fi

# Se --rust-only, parar aqui
if [ "$RUST_ONLY" = true ]; then
    echo ""
    success "Build Rust-only concluído!"
    echo "Arquivo gerado: target/x86_64-unknown-uefi/release/libatom.a"
    exit 0
fi

# =========================================================================
# VERIFICAR NASM
# =========================================================================

if ! command -v nasm &> /dev/null; then
    warning "NASM não encontrado - pulando assembly e linking"
    warning "Para build completo, instale NASM: sudo apt install nasm"
    echo ""
    success "Build Rust concluído (sem assembly/linking)"
    exit 0
fi

# =========================================================================
# MONTAR ARQUIVOS ASSEMBLY
# =========================================================================

step "Montando arquivos assembly..."

if nasm -f win64 arch/x86_64/boot.asm -o build/boot.obj 2>build/nasm.log; then
    success "boot.obj criado"
else
    error "Falha ao montar boot.asm"
    cat build/nasm.log
    exit 1
fi

rm -f build/handlers.obj
if nasm -f win64 kernel/src/interrupts/handlers.asm -o build/handlers.obj 2>build/nasm_handlers.log; then
    success "handlers.obj criado"
else
    error "Falha ao montar handlers.asm"
    cat build/nasm_handlers.log
    exit 1
fi

if nasm -f win64 kernel/src/interrupts/switch.asm -o build/switch.obj 2>build/nasm_switch.log; then
    success "switch.obj criado"
else
    error "Falha ao montar switch.asm"
    cat build/nasm_switch.log
    exit 1
fi

if nasm -f win64 kernel/src/syscall/handler.asm -o build/syscall_handler.obj 2>build/nasm_syscall.log; then
    success "syscall_handler.obj criado"
else
    error "Falha ao montar handler.asm"
    cat build/nasm_syscall.log
    exit 1
fi

# =========================================================================
# LINKAR ATOM.EFI
# =========================================================================

step "Linkando Atom.efi..."

# Find rust-lld
RUST_LLD=$(find ~/.rustup/toolchains/nightly-*/lib/rustlib/*/bin/rust-lld 2>/dev/null | head -1)
if [ -z "$RUST_LLD" ]; then
    warning "rust-lld não encontrado, tentando lld-link..."
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

# =========================================================================
# COPIAR PARA EFI BOOT
# =========================================================================

step "Copiando para efi/EFI/BOOT/BOOTX64.EFI..."
cp build/Atom.efi efi/EFI/BOOT/BOOTX64.EFI
success "BOOTX64.EFI atualizado"

# =========================================================================
# SUMÁRIO DO BUILD
# =========================================================================

header "BUILD COMPLETO"

echo "Kernel:     build/Atom.efi"
echo "EFI Image:  efi/EFI/BOOT/BOOTX64.EFI"
echo "Drivers:    efi/drivers/"
echo ""

# Lista de drivers compilados
if [ -d "efi/drivers" ]; then
    drivers=$(ls efi/drivers/*.bin 2>/dev/null || true)
    if [ -n "$drivers" ]; then
        echo -e "${CYAN}Drivers userspace:${NC}"
        for d in efi/drivers/*.bin; do
            echo "  - $(basename $d)"
        done
        echo ""
    fi
fi

# =========================================================================
# EXECUTAR QEMU (OPCIONAL)
# =========================================================================

if [ "$RUN" = true ]; then
    header "QEMU"

    # Encontrar OVMF
    OVMF_PATH="/usr/share/OVMF/OVMF_CODE.fd"
    if [ ! -f "$OVMF_PATH" ]; then
        OVMF_PATH="/usr/share/edk2-ovmf/x64/OVMF_CODE.fd"
    fi
    if [ ! -f "$OVMF_PATH" ]; then
        OVMF_PATH="ovmf/OVMF.fd"
    fi

    if [ ! -f "$OVMF_PATH" ]; then
        error "OVMF.fd não encontrado"
        warning "Instale: sudo apt install ovmf"
        exit 1
    fi

    if ! command -v qemu-system-x86_64 &> /dev/null; then
        error "qemu-system-x86_64 não encontrado"
        warning "Instale: sudo apt install qemu-system-x86"
        exit 1
    fi

    step "Iniciando QEMU..."
    echo -e "${YELLOW}Pressione Ctrl+A X para sair do QEMU${NC}"
    echo ""
    echo "=========================================="

    qemu-system-x86_64 \
        -machine q35 \
        -cpu qemu64 \
        -m 512M \
        -bios "$OVMF_PATH" \
        -drive format=raw,file=fat:rw:efi \
        -device VGA \
        -usb \
        -device usb-mouse \
        -serial stdio \
        -debugcon file:serial_log.txt \
        -global isa-debugcon.iobase=0xE9
else
    echo -e "${YELLOW}Para testar no QEMU: ./build.sh --run${NC}"
    echo -e "${YELLOW}Para build rápido:   ./build.sh --rust-only${NC}"
fi
