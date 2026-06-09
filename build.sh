#!/bin/bash
# build.sh
# Script de build para o kernel Atom no Linux/macOS
# Uso:
#   ./build.sh              # Build completo (kernel + userspace)
#   ./build.sh --clean      # Limpar e rebuildar
#   ./build.sh --run        # Build e executar no QEMU
#   ./build.sh --run --smp=4 # Build + QEMU com 4 CPUs
#   ./build.sh --userspace  # Build apenas drivers userspace
#   ./build.sh --kernel     # Build apenas kernel
#   ./build.sh --rust-only  # Apenas validar código Rust
#   ./build.sh --setup      # Configurar dependências

set -e
set -o pipefail

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
SMP_CPUS=1

for arg in "$@"; do
    case $arg in
        --run)      RUN=true ;;
        --clean)    CLEAN=true ;;
        --rust-only) RUST_ONLY=true ;;
        --setup)    SETUP=true ;;
        --userspace) USERSPACE_ONLY=true ;;
        --kernel)   KERNEL_ONLY=true ;;
        --smp=*)    SMP_CPUS="${arg#*=}" ;;
        --help|-h)
            echo "Uso: ./build.sh [opções]"
            echo ""
            echo "Opções:"
            echo "  --clean       Limpar arquivos de build antes de compilar"
            echo "  --run         Executar no QEMU após build"
            echo "  --smp=N       Executar QEMU com N CPUs (padrão: 1)"
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

SYSTEM_APPS=(
    "keyboard"
    "mouse"
    "display"
    "display_settings"
    "terminal"
    "ui_shell"
    "demo_rects"
    "demo_text"
)

USERSPACE_SERVICES=(
    "init"
    "namesvc"
    "service_manager"
    "fsd"
    "app_launcher"
    "netd"
    "nic_driver"
)

USERSPACE_APPS=(
    "fileman"
    "fs_test"
    "hello_atxf"
    "timesync"
    "browser"
    "security_smoke"
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
mkdir -p efi/system/services
mkdir -p efi/apps/system
mkdir -p efi/apps/user
mkdir -p efi/user/home
mkdir -p efi/user/config
mkdir -p efi/user/data

# =========================================================================
# RESTAURAR DEPENDÊNCIAS (submodules)
# =========================================================================

restore_submodule() {
    local name="$1"
    local path="$2"
    local url="$3"
    local sentinel="$4"
    local pinned_commit="$5"

    if [ -e "$sentinel" ]; then
        return 0
    fi

    step "Restaurando $name..."

    # Ensure .gitmodules registers this submodule so git knows the URL
    if [ ! -f ".gitmodules" ] || ! grep -q "$path" .gitmodules 2>/dev/null; then
        cat >> .gitmodules <<GITMOD

[submodule "$path"]
	path = $path
	url = $url
GITMOD
        git submodule sync "$path" 2>/dev/null || true
    fi

    # Try git submodule update first (fast if .git/modules already exists)
    if git submodule update --init "$path" 2>/dev/null && [ -e "$sentinel" ]; then
        # Ensure we are at the pinned commit
        git -C "$path" checkout "$pinned_commit" 2>/dev/null || true
        success "$name restaurado via submodule update (commit $pinned_commit)"
        return 0
    fi

    # Fall back: clone directly
    rm -rf "$path"
    if git clone "$url" "$path" 2>/dev/null; then
        git -C "$path" checkout "$pinned_commit" 2>/dev/null || true
        success "$name clonado de $url (commit $pinned_commit)"
    else
        warning "Falha ao restaurar $name — verifique sua conexão de rede"
        warning "  Clone manual: git clone $url $path"
        warning "  Depois: git -C $path checkout $pinned_commit"
    fi
}

restore_submodule \
    "tinygl_src" \
    "userspace/libs/tinygl_src" \
    "https://github.com/C-Chads/tinygl" \
    "userspace/libs/tinygl_src/src/api.c" \
    "e94a97bd"

# =========================================================================
# BUILD USERSPACE TOOLS AND DRIVERS
# =========================================================================

if [ "$KERNEL_ONLY" != true ]; then
    header "USERSPACE BUILD"

    # -------------------------------------------------------------------------
    # Build elf2atxf tool first
    # -------------------------------------------------------------------------
    step "Building elf2atxf tool..."

    HOST_TRIPLE=$(rustc -vV | sed -n 's/host: //p' | tr -d '\r')

    if [ -z "$HOST_TRIPLE" ]; then
        error "Could not determine Rust host triple (rustc -vV)"
        exit 1
    fi

    if ! rustup +nightly target list --installed | grep -q "x86_64-unknown-none"; then
        step "Installing x86_64-unknown-none target..."
        rustup +nightly target add x86_64-unknown-none
    fi

    pushd tools/elf2atxf > /dev/null
    if CARGO_TARGET_DIR=target cargo build --release --target "$HOST_TRIPLE" 2>build.log; then
        success "elf2atxf built"
    else
        error "Failed to build elf2atxf"
        cat build.log
        exit 1
    fi
    popd > /dev/null

    ELF2ATXF="tools/elf2atxf/target/$HOST_TRIPLE/release/elf2atxf"

    if [ ! -x "$ELF2ATXF" ]; then
        error "elf2atxf binary not found at expected path: $ELF2ATXF"
        exit 1
    fi


    # -------------------------------------------------------------------------
    # Build system apps (ui_shell, display, keyboard, mouse, terminal, …)
    # Output → efi/apps/system/
    # -------------------------------------------------------------------------
    step "Building system apps..."

    for app in "${SYSTEM_APPS[@]}"; do
        app_path="userspace/system_apps/$app"

        if [ ! -f "$app_path/Cargo.toml" ]; then
            warning "System app $app not found, skipping..."
            continue
        fi

        step "  Building $app..."
        pushd "$app_path" > /dev/null

        if cargo build --release 2>build.log; then
            popd > /dev/null

            bin_name=$(grep -A5 '\[\[bin\]\]' "$app_path/Cargo.toml" | grep 'name' | head -1 | sed 's/.*= *"\(.*\)"/\1/' | tr -d '\r' || echo "$app")
            if [ -z "$bin_name" ]; then
                bin_name="$app"
            fi

            elf_path="$app_path/target/x86_64-unknown-none/release/$bin_name"
            atxf_path="efi/apps/system/${app}.atxf"

            if [ -f "$elf_path" ]; then
                step "  Converting $app to ATXF..."
                if "$ELF2ATXF" "$elf_path" "$atxf_path" 2>build/elf2atxf_$app.log; then
                    success "$app.atxf → apps/system/"
                else
                    warning "Failed to convert $app to ATXF"
                    cat build/elf2atxf_$app.log
                fi
            else
                warning "ELF not found at $elf_path"
            fi
        else
            popd > /dev/null
            warning "$app failed to build"
            cat "$app_path/build.log" 2>/dev/null || true
        fi
    done

    # -------------------------------------------------------------------------
    # Build system services and convert to ATXF
    # Output → efi/system/services/
    # -------------------------------------------------------------------------
    step "Building system services..."

    for service in "${USERSPACE_SERVICES[@]}"; do
        service_path="userspace/services/$service"

        if [ ! -f "$service_path/Cargo.toml" ]; then
            warning "Service $service not found, skipping..."
            continue
        fi

        step "  Building $service service..."
        pushd "$service_path" > /dev/null

        if cargo build --release 2>build.log; then
            popd > /dev/null

            bin_name=$(grep -A5 '\[\[bin\]\]' "$service_path/Cargo.toml" | grep 'name' | head -1 | sed 's/.*= *"\(.*\)"/\1/' | tr -d '\r' || echo "$service")
            if [ -z "$bin_name" ]; then
                bin_name="$service"
            fi

            elf_path="$service_path/target/x86_64-unknown-none/release/$bin_name"
            atxf_path="efi/system/services/${service}.atxf"

            if [ -f "$elf_path" ]; then
                step "  Converting $service to ATXF..."
                if "$ELF2ATXF" "$elf_path" "$atxf_path" 2>build/elf2atxf_$service.log; then
                    success "$service.atxf → system/services/"
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

    # -------------------------------------------------------------------------
    # Build libc (C standard library for userspace C programs)
    # Output → userspace/libs/libc/build/
    # -------------------------------------------------------------------------
    step "Building libc (C standard library)..."

    if [ -f "userspace/libs/libc/Makefile" ]; then
        if make -C userspace/libs/libc 2>build/libc.log; then
            success "libc.a and crt0.o built"
        else
            warning "libc build failed (see build/libc.log)"
            cat build/libc.log
        fi
    else
        warning "userspace/libs/libc/Makefile not found, skipping libc build"
    fi

    # -------------------------------------------------------------------------
    # Build hello_c (C app to validate libc)
    # Depends on libc being built first; uses its own Makefile
    # Output → efi/apps/user/
    # -------------------------------------------------------------------------
    step "Building hello_c (libc test app)..."

    if [ -f "userspace/apps/hello_c/Makefile" ]; then
        if make -C userspace/apps/hello_c ELF2ATXF="../../../$ELF2ATXF" 2>build/hello_c.log; then
            if [ -f "userspace/apps/hello_c/hello_c.atxf" ]; then
                mkdir -p efi/apps/user
                cp userspace/apps/hello_c/hello_c.atxf efi/apps/user/hello_c.atxf
                success "hello_c.atxf → apps/user/"
            else
                warning "hello_c.atxf não gerado (elf2atxf pode estar ausente)"
            fi
        else
            warning "Falha ao compilar hello_c (veja build/hello_c.log)"
            cat build/hello_c.log
        fi
    else
        warning "userspace/apps/hello_c/Makefile não encontrado, pulando"
    fi

    # -------------------------------------------------------------------------
    # Build libtinygl (TinyGL 0.4.1 — software OpenGL library)
    # Depends on libc being built first
    # Output → userspace/libs/tinygl/build/libtinygl.a
    # -------------------------------------------------------------------------
    step "Building libtinygl (TinyGL 0.4.1)..."

    if [ -f "userspace/libs/tinygl/Makefile" ]; then
        if make -C userspace/libs/tinygl 2>build/libtinygl.log; then
            success "libtinygl.a built"
        else
            warning "libtinygl build failed (see build/libtinygl.log)"
            cat build/libtinygl.log
        fi
    else
        warning "userspace/libs/tinygl/Makefile not found, skipping libtinygl build"
    fi

    # -------------------------------------------------------------------------
    # Build tinygl_demo (TinyGL 0.4.1 gears demo)
    # Depends on libc + libtinygl being built first; uses its own Makefile
    # Output → efi/apps/user/
    # -------------------------------------------------------------------------
    step "Building tinygl_demo (OpenGL gears)..."

    if [ -f "userspace/apps/tinygl_demo/Makefile" ]; then
        if make -C userspace/apps/tinygl_demo ELF2ATXF="../../../$ELF2ATXF" 2>build/tinygl_demo.log; then
            if [ -f "userspace/apps/tinygl_demo/tinygl_demo.atxf" ]; then
                mkdir -p efi/apps/user
                cp userspace/apps/tinygl_demo/tinygl_demo.atxf efi/apps/user/tinygl_demo.atxf
                success "tinygl_demo.atxf → apps/user/"
            else
                warning "tinygl_demo.atxf não gerado (elf2atxf pode estar ausente)"
            fi
        else
            warning "Falha ao compilar tinygl_demo (veja build/tinygl_demo.log)"
            cat build/tinygl_demo.log
        fi
    else
        warning "userspace/apps/tinygl_demo/Makefile não encontrado, pulando"
    fi

    # -------------------------------------------------------------------------
    # Build user applications and convert to ATXF
    # Output → efi/apps/user/
    # -------------------------------------------------------------------------
    step "Building user applications..."

    for app in "${USERSPACE_APPS[@]}"; do
        app_path="userspace/apps/$app"

        if [ ! -f "$app_path/Cargo.toml" ]; then
            warning "App $app not found, skipping..."
            continue
        fi

        step "  Building $app application..."
        pushd "$app_path" > /dev/null

        if cargo build --release 2>build.log; then
            popd > /dev/null

            bin_name=$(grep -A5 '\[\[bin\]\]' "$app_path/Cargo.toml" | grep 'name' | head -1 | sed 's/.*= *"\(.*\)"/\1/' | tr -d '\r' || echo "$app")
            if [ -z "$bin_name" ]; then
                bin_name="$app"
            fi

            elf_path="$app_path/target/x86_64-unknown-none/release/$bin_name"
            atxf_path="efi/apps/user/${app}.atxf"

            if [ -f "$elf_path" ]; then
                step "  Converting $app to ATXF..."
                if "$ELF2ATXF" "$elf_path" "$atxf_path" 2>build/elf2atxf_$app.log; then
                    success "$app.atxf → apps/user/"
                else
                    warning "Failed to convert $app to ATXF"
                    cat build/elf2atxf_$app.log
                fi
            else
                warning "ELF not found at $elf_path - app may not compile to UEFI target"
                warning "If this is a userspace app, it might need separate handling"
            fi
        else
            popd > /dev/null
            warning "$app application failed to build"
            cat "$app_path/build.log" 2>/dev/null || true
        fi
    done

    # init.atxf is the PID-1 boot payload — it must live on the EFI partition
    # so the UEFI bootloader can load it before ExitBootServices().
    if [ -f "efi/system/services/init.atxf" ]; then
        cp efi/system/services/init.atxf efi/EFI/BOOT/init.atxf
        success "init.atxf installed as EFI boot payload (PID 1)"
    else
        warning "init.atxf not found - system will not boot!"
    fi

    success "Userspace build completed"
fi

if [ "$USERSPACE_ONLY" = true ]; then
    echo ""
    success "Build userspace concluído!"
    exit 0
fi

# =========================================================================
# BUILD KERNEL RUST
# =========================================================================

header "KERNEL BUILD"

step "Compilando kernel Rust..."
mkdir -p target/x86_64-unknown-uefi/release/deps
if cargo build -p atom-kernel --release 2>&1 | tee build/cargo.log; then
    if [ -f "target/x86_64-unknown-uefi/release/libatom.a" ]; then
        success "Kernel Rust compilado"
    else
        error "Build Rust terminou sem gerar libatom.a"
        exit 1
    fi

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

if [ ! -f "target/x86_64-unknown-uefi/release/libatom.a" ]; then
    error "libatom.a não encontrado em target/x86_64-unknown-uefi/release/"
    error "Execute o build do kernel Rust novamente para gerar o artefato"
    exit 1
fi

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
# GERAR IMAGEM DE DISCO (Substitui a cópia simples)
# =========================================================================

DISK_IMG="build/atom_disk.img"

# Only create a fresh image if one doesn't exist yet.
# This preserves runtime user data (files created inside the OS) across
# reboots.  Run with --clean to force a full rebuild of the image.
if [ ! -f "$DISK_IMG" ]; then
    step "Criando nova imagem de disco..."

    dd if=/dev/zero of=$DISK_IMG bs=1M count=64 2>/dev/null

    mformat -i $DISK_IMG -F ::

    # ---- EFI partition (boot-only, not visible to users) ----
    mmd -i $DISK_IMG ::/EFI
    mmd -i $DISK_IMG ::/EFI/BOOT

    # ---- OS partition directories ----
    mmd -i $DISK_IMG ::/system
    mmd -i $DISK_IMG ::/system/services
    mmd -i $DISK_IMG ::/system/wallpapers
    mmd -i $DISK_IMG ::/apps
    mmd -i $DISK_IMG ::/apps/system
    mmd -i $DISK_IMG ::/apps/user
    mmd -i $DISK_IMG ::/user
    mmd -i $DISK_IMG ::/user/home
    mmd -i $DISK_IMG ::/user/config
    mmd -i $DISK_IMG ::/user/data

    success "Imagem de disco criada: $DISK_IMG"
else
    step "Imagem existente preservada: $DISK_IMG (use --clean para recriar)"
fi

# Always update OS binaries on the image so the latest build is used,
# but leave /user and runtime-created files untouched.
step "Atualizando binários na imagem..."

# ---- Boot files (EFI partition) ----
mcopy -i $DISK_IMG -o build/Atom.efi ::/EFI/BOOT/BOOTX64.EFI

if [ -f "efi/EFI/BOOT/init.atxf" ]; then
    mcopy -i $DISK_IMG -o efi/EFI/BOOT/init.atxf ::/EFI/BOOT/init.atxf
fi

# ---- System services → /system/services/ ----
if ls efi/system/services/*.atxf 1>/dev/null 2>&1; then
    mcopy -i $DISK_IMG -o efi/system/services/*.atxf ::/system/services/
fi

# ---- System apps → /apps/system/ ----
if ls efi/apps/system/*.atxf 1>/dev/null 2>&1; then
    mcopy -i $DISK_IMG -o efi/apps/system/*.atxf ::/apps/system/
fi

# ---- User apps → /apps/user/ ----
if ls efi/apps/user/*.atxf 1>/dev/null 2>&1; then
    mcopy -i $DISK_IMG -o efi/apps/user/*.atxf ::/apps/user/
fi

# ---- Wallpaper images → /system/wallpapers/ ----
if ls userspace/system_apps/ui_shell/img/*.jpg 1>/dev/null 2>&1; then
    for img in userspace/system_apps/ui_shell/img/*.jpg; do
        mcopy -i $DISK_IMG -o "$img" ::/system/wallpapers/
        success "$(basename $img) → system/wallpapers/"
        if command -v sips >/dev/null 2>&1; then
            png_tmp="build/$(basename "${img%.*}").png"
            if sips -s format png "$img" --out "$png_tmp" >/dev/null 2>&1; then
                mcopy -i $DISK_IMG -o "$png_tmp" ::/system/wallpapers/
                success "$(basename "$png_tmp") → system/wallpapers/"
            fi
        fi
    done
fi

if ls userspace/system_apps/ui_shell/img/*.jpeg 1>/dev/null 2>&1; then
    for img in userspace/system_apps/ui_shell/img/*.jpeg; do
        mcopy -i $DISK_IMG -o "$img" ::/system/wallpapers/
        success "$(basename $img) → system/wallpapers/"
        if command -v sips >/dev/null 2>&1; then
            png_tmp="build/$(basename "${img%.*}").png"
            if sips -s format png "$img" --out "$png_tmp" >/dev/null 2>&1; then
                mcopy -i $DISK_IMG -o "$png_tmp" ::/system/wallpapers/
                success "$(basename "$png_tmp") → system/wallpapers/"
            fi
        fi
    done
fi

# ---- User data files (WADs, etc.) → /apps/user/ ----
if ls efi/apps/user/*.wad 1>/dev/null 2>&1; then
    for wad in efi/apps/user/*.wad; do
        mcopy -i $DISK_IMG -o "$wad" ::/apps/user/
        success "$(basename $wad) → apps/user/"
    done
fi

success "Binários atualizados na imagem: $DISK_IMG"

# =========================================================================
# EXECUTAR QEMU
# =========================================================================

if [ "$RUN" = true ]; then
    header "QEMU"

    OVMF_PATH="/usr/local/share/ovmf/OVMF_CODE.fd" 
    if [ ! -f "$OVMF_PATH" ]; then
        OVMF_PATH="ovmf/OVMF.fd" 
    fi

    if ! [[ "$SMP_CPUS" =~ ^[0-9]+$ ]] || [ "$SMP_CPUS" -lt 1 ]; then
        error "Valor inválido para --smp: $SMP_CPUS"
        exit 1
    fi

    step "Iniciando QEMU com imagem real (smp=$SMP_CPUS)..."
    
    qemu-system-x86_64 \
        -machine q35 \
        -cpu qemu64 \
        -smp "$SMP_CPUS" \
        -m 512M \
        -bios "$OVMF_PATH" \
        -drive format=raw,file=$DISK_IMG,cache=writeback \
        -device VGA \
        -serial file:serial.log \
        -debugcon file:serial_log.txt \
        -global isa-debugcon.iobase=0xE9 \
        -d cpu_reset,int,pcall,guest_errors,unimp \
        -D qemu_execution.log \
        -netdev user,id=net0,net=10.0.2.0/24,host=10.0.2.2,dns=10.0.2.3,hostfwd=tcp::12222-:22 \
        -device e1000,netdev=net0,mac=52:54:00:12:34:56 \
        -object filter-dump,id=f1,netdev=net0,file=net_capture.pcap
fi
