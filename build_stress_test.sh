#!/bin/bash
# build_stress_test.sh
# Script de build para o kernel Atom com stress test habilitado.
# Baseado em build.sh; alterações marcadas com [STRESS].
#
# Uso:
#   ./build_stress_test.sh              # Build kernel + executa no QEMU
#   ./build_stress_test.sh --clean      # Limpar e rebuildar
#   ./build_stress_test.sh --run        # Build e executar no QEMU (mesmo que padrão)
#   ./build_stress_test.sh --kernel     # Build apenas kernel (sem QEMU)
#   ./build_stress_test.sh --rust-only  # Apenas validar código Rust
#   ./build_stress_test.sh --setup      # Configurar dependências

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

# [STRESS] RUN=true por padrão: o objetivo principal é rodar o teste no QEMU
RUN=true
CLEAN=false
RUST_ONLY=false
SETUP=false
KERNEL_ONLY=false

for arg in "$@"; do
    case $arg in
        --run)       RUN=true ;;
        --clean)     CLEAN=true ;;
        --rust-only) RUST_ONLY=true ;;
        --setup)     SETUP=true ;;
        --kernel)    KERNEL_ONLY=true; RUN=false ;;
        --help|-h)
            echo "Uso: ./build_stress_test.sh [opções]"
            echo ""
            echo "Opções:"
            echo "  --clean       Limpar arquivos de build antes de compilar"
            echo "  --run         Executar no QEMU após build (padrão)"
            echo "  --kernel      Build apenas kernel, sem executar QEMU"
            echo "  --rust-only   Apenas validar código Rust (sem NASM/linker)"
            echo "  --setup       Configurar dependências do Rust"
            echo "  --help, -h    Mostrar esta ajuda"
            echo ""
            echo "Variáveis de ambiente:"
            echo "  QEMU_MEM      Memória para o QEMU (padrão: 1G)"
            echo "  STRESS_LOG    Arquivo de log serial (padrão: stress_serial.log)"
            exit 0
            ;;
    esac
done

# [STRESS] Parâmetros configuráveis via variável de ambiente
QEMU_MEM="${QEMU_MEM:-1G}"
STRESS_LOG="${STRESS_LOG:-stress_serial.log}"

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
# BUILD KERNEL RUST  [STRESS: adiciona --features stress_test]
# =========================================================================

header "KERNEL BUILD (stress_test)"

step "Compilando kernel Rust com feature stress_test..."
# [STRESS] Única linha alterada em relação ao build.sh original:
#   adicionado --features stress_test
if cargo build -p atom-kernel --release --features stress_test 2>&1 | tee build/cargo.log; then
    success "Kernel Rust compilado com stress_test"

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
    # [STRESS] Instruções específicas por plataforma
    if [[ "$(uname)" == "Darwin" ]]; then
        warning "No macOS instale via: brew install nasm"
    else
        warning "Para build completo, instale NASM: sudo apt install nasm"
    fi
    echo ""
    success "Build Rust concluído (sem assembly/linking)"
    exit 0
fi

# =========================================================================
# MONTAR ARQUIVOS ASSEMBLY
# =========================================================================

step "Montando arquivos assembly..."

# [STRESS] nasm -f win64 funciona em qualquer host (NASM é cross-assembler);
# no macOS com brew install nasm o comportamento é idêntico ao Linux.

if nasm -f win64 arch/x86_64/boot.asm -o build/boot.obj 2>build/nasm.log; then
    success "boot.obj criado"
else
    error "Falha ao montar boot.asm"
    cat build/nasm.log
    exit 1
fi

rm -f build/handlers.obj
if nasm -f win64 -i kernel/src/interrupts/ kernel/src/interrupts/handlers.asm -o build/handlers.obj 2>build/nasm_handlers.log; then
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
echo ""

# =========================================================================
# EXECUTAR QEMU COM STRESS TEST  [STRESS: bloco reescrito]
# =========================================================================

if [ "$RUN" = true ] && [ "$KERNEL_ONLY" = false ]; then
    header "QEMU — STRESS TEST"

    # -------------------------------------------------------------------------
    # [STRESS] Detectar OVMF: Linux, macOS (Homebrew Intel e Apple Silicon)
    # -------------------------------------------------------------------------
    OVMF_PATH=""

    # Linux paths
    for candidate in \
        "/usr/share/OVMF/OVMF_CODE.fd" \
        "/usr/share/edk2-ovmf/x64/OVMF_CODE.fd" \
        "/usr/share/edk2/ovmf/OVMF_CODE.fd"; do
        if [ -f "$candidate" ]; then
            OVMF_PATH="$candidate"
            break
        fi
    done

    # macOS — Homebrew (Apple Silicon: /opt/homebrew; Intel: /usr/local)
    if [ -z "$OVMF_PATH" ] && [[ "$(uname)" == "Darwin" ]]; then
        BREW_PREFIX="$(brew --prefix 2>/dev/null || echo /opt/homebrew)"
        for candidate in \
            "$BREW_PREFIX/share/qemu/edk2-x86_64-code.fd" \
            "$BREW_PREFIX/Cellar/qemu/$(ls "$BREW_PREFIX/Cellar/qemu" 2>/dev/null | tail -1)/share/qemu/edk2-x86_64-code.fd" \
            "$BREW_PREFIX/share/OVMF/OVMF_CODE.fd"; do
            if [ -f "$candidate" ]; then
                OVMF_PATH="$candidate"
                break
            fi
        done
    fi

    # Fallback: repositório local
    if [ -z "$OVMF_PATH" ] && [ -f "ovmf/OVMF.fd" ]; then
        OVMF_PATH="ovmf/OVMF.fd"
    fi

    if [ -z "$OVMF_PATH" ]; then
        error "OVMF.fd não encontrado"
        if [[ "$(uname)" == "Darwin" ]]; then
            warning "No macOS o OVMF vem com o QEMU do Homebrew: brew install qemu"
            warning "Ou coloque manualmente em ovmf/OVMF.fd"
        else
            warning "Instale: sudo apt install ovmf"
        fi
        exit 1
    fi

    success "Usando OVMF: $OVMF_PATH"

    if ! command -v qemu-system-x86_64 &> /dev/null; then
        error "qemu-system-x86_64 não encontrado"
        if [[ "$(uname)" == "Darwin" ]]; then
            warning "Instale: brew install qemu"
        else
            warning "Instale: sudo apt install qemu-system-x86"
        fi
        exit 1
    fi

    # -------------------------------------------------------------------------
    # [STRESS] Remover log anterior para não misturar resultados
    # -------------------------------------------------------------------------
    rm -f "$STRESS_LOG"

    step "Iniciando QEMU com stress test (memória: $QEMU_MEM)..."
    echo -e "${YELLOW}Log serial: $STRESS_LOG${NC}"
    echo -e "${YELLOW}O QEMU não reiniciará automaticamente ao fim do teste.${NC}"
    echo -e "${YELLOW}Pressione Ctrl+A X para encerrar manualmente se necessário.${NC}"
    echo ""
    echo "=========================================="

    # [STRESS] Diferenças em relação ao build.sh:
    #   -m $QEMU_MEM         : mais memória para o stress test (padrão 1G)
    #   -serial file:...     : redireciona saída serial para arquivo + tee no terminal
    #   -no-reboot           : não reinicia em caso de panic/triple-fault
    #   -no-shutdown         : mantém QEMU vivo para inspecionar estado final
    #   -debugcon file:...   : debug console separado (porta 0xE9)
    qemu-system-x86_64 \
        -machine q35 \
        -cpu qemu64 \
        -m "$QEMU_MEM" \
        -bios "$OVMF_PATH" \
        -drive format=raw,file=fat:rw:efi \
        -device VGA \
        -usb \
        -device usb-mouse \
        -serial file:"$STRESS_LOG" \
        -debugcon file:stress_debug.log \
        -global isa-debugcon.iobase=0xE9 \
        -no-reboot \
        -no-shutdown \
        || true   # não abortar o script se o QEMU sair com código != 0

    echo "=========================================="
    echo ""

    # -------------------------------------------------------------------------
    # [STRESS] Analisar resultado do log serial
    # -------------------------------------------------------------------------
    header "RESULTADO DO STRESS TEST"

    if [ ! -f "$STRESS_LOG" ]; then
        error "Arquivo de log não encontrado: $STRESS_LOG"
        error "O kernel pode ter falhado antes de escrever na serial."
        exit 1
    fi

    echo -e "${CYAN}Últimas 30 linhas do log:${NC}"
    tail -30 "$STRESS_LOG"
    echo ""

    # Verificar veredicto final
    if grep -q "RESULT: PASS" "$STRESS_LOG"; then
        success "STRESS TEST: PASS"
    elif grep -q "RESULT: FAIL" "$STRESS_LOG"; then
        error "STRESS TEST: FAIL"
        echo ""
        echo -e "${YELLOW}Linhas de falha detectadas:${NC}"
        grep -E "FAIL|STALL|CANARY|PANIC|ERROR" "$STRESS_LOG" || true
        exit 1
    else
        warning "Veredicto final não encontrado no log (teste incompleto ou kernel travado)"
        echo ""
        echo -e "${YELLOW}Eventos relevantes encontrados:${NC}"
        grep -E "stress|FAIL|PASS|PANIC|ERROR|stall|canary" "$STRESS_LOG" | tail -20 || true
    fi

    echo ""
    echo -e "${CYAN}Log completo: $STRESS_LOG${NC}"
    echo -e "${CYAN}Debug log:    stress_debug.log${NC}"
else
    echo -e "${YELLOW}Build concluído. Para executar: ./build_stress_test.sh --run${NC}"
    echo -e "${YELLOW}Para build Rust rápido:        ./build_stress_test.sh --rust-only${NC}"
fi
