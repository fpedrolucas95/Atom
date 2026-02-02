#!/bin/bash
# build.sh
# Script de build para o kernel Atom no Linux/macOS
# Uso:
#   ./build.sh              # Build completo (kernel + userspace)
#   ./build.sh --arch=aarch64 # Build para ARM64
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
ARCH="x86_64"

for arg in "$@"; do
    case $arg in
        --run)      RUN=true ;;
        --clean)    CLEAN=true ;;
        --rust-only) RUST_ONLY=true ;;
        --setup)    SETUP=true ;;
        --userspace) USERSPACE_ONLY=true ;;
        --kernel)   KERNEL_ONLY=true ;;
        --arch=*)   ARCH="${arg#*=}" ;;
        --help|-h)
            echo "Uso: ./build.sh [opções]"
            echo ""
            echo "Opções:"
            echo "  --arch=ARCH   Arquitetura de destino (x86_64, aarch64). Padrão: x86_64"
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

# Configurar alvos baseados na arquitetura
if [ "$ARCH" = "x86_64" ]; then
    TARGET="x86_64-unknown-uefi"
    USER_TARGET="x86_64-unknown-none"
    EFI_FILE="BOOTX64.EFI"
    QEMU="qemu-system-x86_64"
elif [ "$ARCH" = "aarch64" ]; then
    TARGET="aarch64-unknown-uefi"
    USER_TARGET="aarch64-unknown-none"
    EFI_FILE="BOOTAA64.EFI"
    QEMU="qemu-system-aarch64"
else
    error "Arquitetura não suportada: $ARCH"
    exit 1
fi

# -------------------------------------------------------------------------
# Userspace drivers list
# -------------------------------------------------------------------------

USERSPACE_DRIVERS=(
    "keyboard"
    "mouse"
    "display"
    "terminal"
    "ui_shell"
    "demo_rects"
    "demo_text"
)

# System services (init is PID 1 - spawns everything else)
USERSPACE_SERVICES=(
    "init"
    "namesvc"
    "service_manager"
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

    if ! rustup target list --installed | grep -q "$TARGET"; then
        step "Adicionando target $TARGET..."
        rustup target add $TARGET
        success "Target $TARGET adicionado"
    else
        echo "Target $TARGET já instalado"
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

if ! rustup target list --installed 2>/dev/null | grep -q "$TARGET"; then
    warning "Target $TARGET não encontrado, adicionando..."
    rustup target add $TARGET 2>/dev/null || true
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
# BUILD USERSPACE TOOLS AND DRIVERS
# =========================================================================

if [ "$KERNEL_ONLY" != true ]; then
    header "USERSPACE BUILD ($ARCH)"

    # -------------------------------------------------------------------------
    # Build elf2atxf tool first
    # -------------------------------------------------------------------------
    step "Building elf2atxf tool..."

    # elf2atxf is a host tool, always build for the host
    pushd tools/elf2atxf > /dev/null
    if cargo build --release 2>build.log; then
        success "elf2atxf built"
    else
        error "Failed to build elf2atxf"
        cat build.log
        exit 1
    fi
    popd > /dev/null

    # Find the host target directory
    HOST_TARGET=$(rustc -vV | grep host | cut -d' ' -f2)
    ELF2ATXF="tools/elf2atxf/target/$HOST_TARGET/release/elf2atxf"
    if [ ! -f "$ELF2ATXF" ]; then
        # Fallback if host detection fails
        ELF2ATXF=$(find tools/elf2atxf/target -name elf2atxf | grep release | head -1)
    fi

    # -------------------------------------------------------------------------
    # Build userspace drivers and convert to ATXF
    # -------------------------------------------------------------------------
    step "Building userspace drivers..."

    if ! rustup target list --installed | grep -q "$USER_TARGET"; then
        step "Installing $USER_TARGET target..."
        rustup target add $USER_TARGET
    fi

    for driver in "${USERSPACE_DRIVERS[@]}"; do
        driver_path="userspace/drivers/$driver"

        if [ ! -f "$driver_path/Cargo.toml" ]; then
            warning "Driver $driver not found, skipping..."
            continue
        fi

        step "  Building $driver driver..."
        pushd "$driver_path" > /dev/null

        if cargo build --target $USER_TARGET --release 2>build.log; then
            popd > /dev/null

            # Find the ELF binary name from Cargo.toml
            bin_name=$(grep -A5 '\[\[bin\]\]' "$driver_path/Cargo.toml" | grep 'name' | head -1 | sed 's/.*= *"\(.*\)"/\1/' | tr -d '\r' || echo "$driver")
            if [ -z "$bin_name" ]; then
                bin_name="$driver"
            fi

            elf_path="$driver_path/target/$USER_TARGET/release/$bin_name"
            atxf_path="efi/drivers/${driver}.atxf"

            if [ -f "$elf_path" ]; then
                step "  Converting $driver to ATXF..."
                if "$ELF2ATXF" "$elf_path" "$atxf_path" 2>build/elf2atxf_$driver.log; then
                    success "$driver.atxf created"
                else
                    warning "Failed to convert $driver to ATXF"
                    cat build/elf2atxf_$driver.log
                fi
            else
                warning "ELF not found at $elf_path"
            fi
        else
            popd > /dev/null
            warning "$driver driver failed to build"
            cat "$driver_path/build.log" 2>/dev/null || true
        fi
    done

    # -------------------------------------------------------------------------
    # Build userspace services and convert to ATXF
    # -------------------------------------------------------------------------
    step "Building userspace services..."

    for service in "${USERSPACE_SERVICES[@]}"; do
        service_path="userspace/services/$service"

        if [ ! -f "$service_path/Cargo.toml" ]; then
            warning "Service $service not found, skipping..."
            continue
        fi

        step "  Building $service service..."
        pushd "$service_path" > /dev/null

        if cargo build --target $USER_TARGET --release 2>build.log; then
            popd > /dev/null

            # Find the ELF binary name from Cargo.toml
            bin_name=$(grep -A5 '\[\[bin\]\]' "$service_path/Cargo.toml" | grep 'name' | head -1 | sed 's/.*= *"\(.*\)"/\1/' | tr -d '\r' || echo "$service")
            if [ -z "$bin_name" ]; then
                bin_name="$service"
            fi

            elf_path="$service_path/target/$USER_TARGET/release/$bin_name"
            atxf_path="efi/drivers/${service}.atxf"

            if [ -f "$elf_path" ]; then
                step "  Converting $service to ATXF..."
                if "$ELF2ATXF" "$elf_path" "$atxf_path" 2>build/elf2atxf_$service.log; then
                    success "$service.atxf created"
                else
                    warning "Failed to convert $service to ATXF"
                    cat build/elf2atxf_$service.log
                fi
            else
                warning "ELF not found at $elf_path"
            fi
        else
            popd > /dev/null
            warning "$service service failed to build"
            cat "$service_path/build.log" 2>/dev/null || true
        fi
    done

    # Copy init.atxf to EFI boot directory as the boot payload (PID 1)
    if [ -f "efi/drivers/init.atxf" ]; then
        cp efi/drivers/init.atxf efi/EFI/BOOT/init.atxf
        success "init.atxf installed as boot payload (PID 1)"
    else
        warning "init.atxf not found - system will not boot!"
    fi

    success "Userspace build completed"
fi

# Se --userspace only, parar aqui
if [ "$USERSPACE_ONLY" = true ]; then
    echo ""
    success "Build userspace concluído!"
    exit 0
fi

# =========================================================================
# BUILD KERNEL RUST
# =========================================================================

header "KERNEL BUILD ($ARCH)"

step "Compilando kernel Rust..."
if cargo build -p atom-kernel --target $TARGET --release 2>&1 | tee build/cargo.log; then
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
    echo "Arquivo gerado: target/$TARGET/release/libatom.a"
    exit 0
fi

# =========================================================================
# ASSEMBLY AND LINKING (Architecture Specific)
# =========================================================================

if [ "$ARCH" = "x86_64" ]; then
    # -------------------------------------------------------------------------
    # VERIFICAR NASM
    # -------------------------------------------------------------------------
    if ! command -v nasm &> /dev/null; then
        warning "NASM não encontrado - pulando assembly e linking para x86_64"
        warning "Para build completo, instale NASM: sudo apt install nasm"
        echo ""
        success "Build Rust concluído (sem assembly/linking)"
    else
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

        step "Linkando Atom.efi..."
        RUST_LLD=$(find ~/.rustup/toolchains/nightly-*/lib/rustlib/*/bin/rust-lld 2>/dev/null | head -1)
        if [ -z "$RUST_LLD" ]; then RUST_LLD="lld-link"; fi

        if "$RUST_LLD" -flavor link \
            build/boot.obj \
            build/handlers.obj \
            build/switch.obj \
            build/syscall_handler.obj \
            target/$TARGET/release/libatom.a \
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
    fi
elif [ "$ARCH" = "aarch64" ]; then
    # For AArch64, we rely on the rust-generated EFI binary for now
    # or use a specialized linker script.
    # Since we are using target aarch64-unknown-uefi, cargo already produces a PE file.
    step "Extracting EFI binary for AArch64..."
    # Cargo name for the staticlib is libatom.a, but for uefi target it might produce .efi if configured as cdylib
    # In our case it's staticlib. We might need to change crate-type or use a custom build step.
    # For now, let's assume we just need the lib for further linking if we had assembly.
    # Since we don't have AArch64 assembly yet, we'll just note that.
    warning "Full linking for AArch64 is not yet implemented in this script."
    warning "Only the Rust static library was built."
    # TODO: Implement AArch64 EFI linking
fi

# =========================================================================
# COPIAR PARA EFI BOOT
# =========================================================================

if [ -f "build/Atom.efi" ]; then
    step "Copiando para efi/EFI/BOOT/$EFI_FILE..."
    cp build/Atom.efi efi/EFI/BOOT/$EFI_FILE
    success "$EFI_FILE atualizado"
fi

# =========================================================================
# SUMÁRIO DO BUILD
# =========================================================================

header "BUILD COMPLETO ($ARCH)"

echo "Kernel Lib:  target/$TARGET/release/libatom.a"
if [ -f "build/Atom.efi" ]; then
    echo "EFI Image:   efi/EFI/BOOT/$EFI_FILE"
fi
echo "Drivers:     efi/drivers/"
echo ""

# =========================================================================
# EXECUTAR QEMU (OPCIONAL)
# =========================================================================

if [ "$RUN" = true ]; then
    header "QEMU ($ARCH)"

    if [ "$ARCH" = "x86_64" ]; then
        OVMF_PATH="/usr/share/OVMF/OVMF_CODE.fd"
        if [ ! -f "$OVMF_PATH" ]; then OVMF_PATH="/usr/share/edk2-ovmf/x64/OVMF_CODE.fd"; fi
        if [ ! -f "$OVMF_PATH" ]; then OVMF_PATH="ovmf/OVMF.fd"; fi

        if [ ! -f "$OVMF_PATH" ]; then
            error "OVMF.fd não encontrado"
            exit 1
        fi

        qemu-system-x86_64 \
            -machine q35 -cpu qemu64 -m 512M \
            -bios "$OVMF_PATH" \
            -drive format=raw,file=fat:rw:efi \
            -device VGA -usb -device usb-mouse -serial stdio
    elif [ "$ARCH" = "aarch64" ]; then
        # For AArch64, we need QEMU_EFI.fd
        OVMF_PATH="/usr/share/AAVMF/AAVMF_CODE.fd"
        if [ ! -f "$OVMF_PATH" ]; then OVMF_PATH="ovmf/QEMU_EFI.fd"; fi

        if [ ! -f "$OVMF_PATH" ]; then
            warning "AAVMF_CODE.fd (ARM64 UEFI) não encontrado, QEMU pode falhar."
        fi

        qemu-system-aarch64 \
            -machine virt -cpu cortex-a57 -m 512M \
            -bios "$OVMF_PATH" \
            -drive format=raw,file=fat:rw:efi \
            -device virtio-gpu-pci -usb -device usb-mouse -serial stdio
    fi
else
    echo -e "${YELLOW}Para testar no QEMU: ./build.sh --arch=$ARCH --run${NC}"
fi
