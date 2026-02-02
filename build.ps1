# build.ps1
# Script de build para o kernel Atom no Windows
# Uso:
#   .\build.ps1                    # Build completo (kernel + userspace)
#   .\build.ps1 -Arch aarch64      # Build para ARM64
#   .\build.ps1 --clean            # Limpar e rebuildar
#   .\build.ps1 --run              # Build e executar no QEMU
#   .\build.ps1 --userspace        # Build apenas drivers + services userspace
#   .\build.ps1 --kernel           # Build apenas kernel
#   .\build.ps1 --rust-only        # Apenas compilar Rust (sem assembly/linking)
#   .\build.ps1 --setup            # Configurar dependências Rust

param(
    [string]$Arch = "x86_64",
    [switch]$Run,
    [switch]$Clean,
    [switch]$Userspace,
    [switch]$Kernel,
    [switch]$RustOnly,
    [switch]$Setup
)

# -------------------------------------------------------------------------
# Configurações
# -------------------------------------------------------------------------

$NASM_PATH  = "C:\Program Files\NASM\nasm.exe"
$REPO_PATH  = $PSScriptRoot

# Configurar alvos baseados na arquitetura
if ($Arch -eq "x86_64") {
    $TARGET = "x86_64-unknown-uefi"
    $USER_TARGET = "x86_64-unknown-none"
    $EFI_FILE = "BOOTX64.EFI"
    $QEMU = "qemu-system-x86_64"
    $OVMF_PATH = "$REPO_PATH\ovmf\OVMF.fd"
} elseif ($Arch -eq "aarch64") {
    $TARGET = "aarch64-unknown-uefi"
    $USER_TARGET = "aarch64-unknown-none"
    $EFI_FILE = "BOOTAA64.EFI"
    $QEMU = "qemu-system-aarch64"
    $OVMF_PATH = "$REPO_PATH\ovmf\QEMU_EFI.fd"
} else {
    Write-Error "Arquitetura não suportada: $Arch"
    exit 1
}

# Apenas as pastas dos drivers
$USERSPACE_DRIVERS_DIRS = @("keyboard", "mouse", "display", "terminal", "ui_shell", "demo_rects", "demo_text")

# Services
$USERSPACE_SERVICES = @(
    "init",
    "namesvc",
    "service_manager"
)

# -------------------------------------------------------------------------
# Funções auxiliares
# -------------------------------------------------------------------------

function Write-Step    { param([string]$Message) Write-Host "[*] $Message" -ForegroundColor Cyan }
function Write-Success { param([string]$Message) Write-Host "[OK] $Message" -ForegroundColor Green }
function Write-ErrorMsg{ param([string]$Message) Write-Host "[X] $Message" -ForegroundColor Red; exit 1 }
function Write-Warning { param([string]$Message) Write-Host "[!] $Message" -ForegroundColor Yellow }
function Header        { param([string]$Title)
    Write-Host ""
    Write-Host "========== $Title ==========" -ForegroundColor Magenta
    Write-Host ""
}

# -------------------------------------------------------------------------
# Verificações iniciais
# -------------------------------------------------------------------------

if (-not (Test-Path "kernel\Cargo.toml")) {
    Write-ErrorMsg "Este script deve ser executado na raiz do repositório Atom"
}

# -------------------------------------------------------------------------
# SETUP (instalar componentes Rust)
# -------------------------------------------------------------------------

if ($Setup) {
    Header "SETUP ($Arch)"

    Write-Step "Configurando toolchain Rust..."

    if (-not (Test-Path "rust-toolchain.toml")) {
        '[toolchain]' | Out-File -FilePath "rust-toolchain.toml" -Encoding utf8
        'channel = "nightly"' | Out-File -FilePath "rust-toolchain.toml" -Append -Encoding utf8
        Write-Success "rust-toolchain.toml criado"
    }

    $components = rustup component list --installed
    if ($components -notmatch "rust-src") {
        Write-Step "Adicionando rust-src..."
        rustup component add rust-src
        Write-Success "rust-src adicionado"
    }

    $targets = rustup target list --installed
    if ($targets -notmatch $TARGET) {
        Write-Step "Adicionando target $TARGET..."
        rustup target add $TARGET
        Write-Success "Target $TARGET adicionado"
    }

    if ($targets -notmatch $USER_TARGET) {
        Write-Step "Adicionando target $USER_TARGET..."
        rustup +nightly target add $USER_TARGET
        Write-Success "Target $USER_TARGET adicionado"
    }

    Write-Success "Setup concluído!"
    exit 0
}

# -------------------------------------------------------------------------
# Clean opcional
# -------------------------------------------------------------------------

if ($Clean) {
    Write-Step "Limpando arquivos de build..."
    if (Test-Path "build") { Remove-Item -Recurse -Force "build\*" }
    cargo clean
    Write-Success "Build limpo"
}

# -------------------------------------------------------------------------
# Preparar diretórios
# -------------------------------------------------------------------------

New-Item -ItemType Directory -Path "build","build\userspace","efi\EFI\BOOT","efi\drivers" -Force | Out-Null

# =========================================================================
# BUILD ELF2ATXF TOOL
# =========================================================================

Header "ELF2ATXF TOOL"

$ELF2ATXF_PATH = "tools\elf2atxf"
# Find the host target directory
$HOST_TARGET = rustc -vV | Select-String "host: " | ForEach-Object { $_.ToString().Split(" ")[1] }
$ELF2ATXF_EXE  = "$ELF2ATXF_PATH\target\$HOST_TARGET\release\elf2atxf.exe"

if (-not (Test-Path $ELF2ATXF_EXE) -or $Clean) {
    Write-Step "Compilando elf2atxf tool..."

    Push-Location $ELF2ATXF_PATH
    cargo build --release *> "$REPO_PATH\build.log"
    if ($LASTEXITCODE -ne 0) { Write-ErrorMsg "Falha ao compilar elf2atxf"; Pop-Location; exit 1 }
    Pop-Location

    Write-Success "elf2atxf compilado"
} else {
    Write-Success "elf2atxf já existe (use --clean para forçar recompilação)"
}

# =========================================================================
# BUILD USERSPACE (drivers + services)
# =========================================================================

if (-not $Kernel) {
    Header "USERSPACE BUILD ($Arch)"

    # Função auxiliar para build + conversão ATXF
    function Build-And-Convert {
        param([string]$Path, [string]$Type)

        if (-not (Test-Path "$Path\Cargo.toml")) {
            Write-Warning "$Type em $Path não encontrado, pulando..."
            return
        }

        $dirName = Split-Path $Path -Leaf
        Write-Step "Compilando $Type $dirName..."

        Push-Location $Path
        cargo build --target $USER_TARGET --release *> "$REPO_PATH\build.log"
        if ($LASTEXITCODE -ne 0) {
            Pop-Location
            Write-ErrorMsg "Falha ao compilar $Type $dirName"
        }
        Pop-Location

        $cargoContent = Get-Content "$Path\Cargo.toml" -Raw
        if ($cargoContent -match '(?smi)\[\[bin\]\].*?name\s*=\s*"(.*?)"') {
            $binName = $Matches[1].Trim()
        } elseif ($cargoContent -match '(?smi)name\s*=\s*"(.*?)"') {
            $binName = $Matches[1].Trim()
        } else {
            $binName = $dirName
        }

        $elfPath = "$Path\target\$USER_TARGET\release\$binName"
        if (-not (Test-Path $elfPath)) {
            Write-Warning "Binário ELF não encontrado: $elfPath"
            return
        }

        $atxfPath = "efi\drivers\$dirName.atxf"
        Write-Step "Convertendo $binName para ATXF..."

        $elf2atxfFull = Resolve-Path $ELF2ATXF_EXE -ErrorAction SilentlyContinue
        & $elf2atxfFull "$elfPath" "$atxfPath" *> "$REPO_PATH\build.log"
        if ($LASTEXITCODE -ne 0) { Write-ErrorMsg "Falha na conversão para ATXF ($LASTEXITCODE)" }

        Write-Success "$dirName.atxf criado"
    }

    foreach ($driverDir in $USERSPACE_DRIVERS_DIRS) {
        Build-And-Convert "userspace\drivers\$driverDir" "driver"
    }

    foreach ($service in $USERSPACE_SERVICES) {
        Build-And-Convert "userspace\services\$service" "service"
    }

    if (Test-Path "efi\drivers\init.atxf") {
        Copy-Item "efi\drivers\init.atxf" "efi\EFI\BOOT\init.atxf" -Force
        Write-Success "init.atxf instalado como payload de boot (PID 1)"
    } else {
        Write-Warning "init.atxf não encontrado - o sistema não irá bootar corretamente!"
    }

    Write-Success "Userspace concluído"
}

if ($Userspace) {
    Write-Host ""
    Write-Success "Build userspace concluído!"
    exit 0
}

# =========================================================================
# BUILD KERNEL RUST
# =========================================================================

Header "KERNEL BUILD ($Arch)"

Write-Step "Compilando kernel Rust..."
cargo build -p atom-kernel --target $TARGET --release *> "$REPO_PATH\build.log"
if ($LASTEXITCODE -ne 0) { Write-ErrorMsg "Falha ao compilar kernel Rust" }

if (Select-String "warning:" "$REPO_PATH\build.log") {
    Write-Warning "Build teve warnings (veja build.log)"
}

Write-Success "Kernel Rust compilado"

if ($RustOnly) {
    Write-Success "Build Rust-only concluído!"
    Write-Host "Arquivo gerado: target\$TARGET\release\libatom.a"
    exit 0
}

# =========================================================================
# ASSEMBLY AND LINKING
# =========================================================================

if ($Arch -eq "x86_64") {
    if (-not $Kernel -and -not $Userspace) {
        Write-Step "Montando arquivos assembly..."

        if (-not (Test-Path $NASM_PATH)) {
            Write-Warning "NASM não encontrado em: $NASM_PATH"
        } else {
            $asmFiles = @(
                @{src="arch\x86_64\boot.asm"; obj="build\boot.obj"},
                @{src="kernel\src\interrupts\handlers.asm"; obj="build\handlers.obj"},
                @{src="kernel\src\interrupts\switch.asm"; obj="build\switch.obj"},
                @{src="kernel\src\syscall\handler.asm"; obj="build\syscall_handler.obj"}
            )

            foreach ($asm in $asmFiles) {
                if ($asm.obj -like "*handlers.obj" -and (Test-Path $asm.obj)) { Remove-Item -Force $asm.obj }
                & $NASM_PATH -f win64 $asm.src -o $asm.obj *> "$REPO_PATH\build.log"
                if ($LASTEXITCODE -ne 0) { Write-ErrorMsg "Falha ao montar $($asm.src)" }
                Write-Success "$(Split-Path $asm.src -Leaf).obj criado"
            }

            Write-Step "Linkando Atom.efi..."
            # Try to find rust-lld
            $RUST_LLD = Get-ChildItem "$env:USERPROFILE\.rustup\toolchains\nightly*" -Recurse -Filter "rust-lld.exe" | Select-Object -First 1
            if (-not $RUST_LLD) { Write-ErrorMsg "rust-lld não encontrado" }

            & $RUST_LLD.FullName `
                -flavor link `
                build\boot.obj `
                build\handlers.obj `
                build\switch.obj `
                build\syscall_handler.obj `
                target\$TARGET\release\libatom.a `
                /OUT:build\Atom.efi `
                /SUBSYSTEM:EFI_APPLICATION `
                /ENTRY:efi_entry `
                /NODEFAULTLIB *> "$REPO_PATH\build.log"

            if ($LASTEXITCODE -ne 0) { Write-ErrorMsg "Falha ao linkar Atom.efi" }
            Write-Success "Atom.efi criado"
        }
    }
} elseif ($Arch -eq "aarch64") {
    Write-Warning "Full linking for AArch64 is not yet implemented in this script."
}

# =========================================================================
# COPIAR PARA EFI BOOT
# =========================================================================

if (Test-Path "build\Atom.efi") {
    Copy-Item build\Atom.efi efi\EFI\BOOT\$EFI_FILE -Force
    Write-Success "$EFI_FILE atualizado"
}

# =========================================================================
# SUMÁRIO
# =========================================================================

Header "BUILD COMPLETO ($Arch)"

Write-Host "Kernel Lib:  target\$TARGET\release\libatom.a" -ForegroundColor White
if (Test-Path "build\Atom.efi") {
    Write-Host "EFI Image:   efi\EFI\BOOT\$EFI_FILE" -ForegroundColor White
}
Write-Host "Drivers:     efi\drivers\" -ForegroundColor White
Write-Host ""

# =========================================================================
# QEMU
# =========================================================================

if ($Run) {
    Header "QEMU ($Arch)"

    if ($Arch -eq "x86_64") {
        if (-not (Test-Path $OVMF_PATH)) { Write-ErrorMsg "OVMF.fd não encontrado em: $OVMF_PATH" }

        qemu-system-x86_64 `
            -machine q35 -cpu qemu64 -m 512M `
            -bios "$OVMF_PATH" `
            -drive format=raw,file=fat:rw:"$REPO_PATH\efi" `
            -device VGA -usb -device usb-mouse -serial stdio `
            -debugcon file:serial_log.txt -global isa-debugcon.iobase=0xE9
    } elseif ($Arch -eq "aarch64") {
        # For AArch64, we need QEMU_EFI.fd
        if (-not (Test-Path $OVMF_PATH)) { Write-Warning "AAVMF_CODE.fd (ARM64 UEFI) não encontrado." }

        qemu-system-aarch64 `
            -machine virt -cpu cortex-a57 -m 512M `
            -bios "$OVMF_PATH" `
            -drive format=raw,file=fat:rw:"$REPO_PATH\efi" `
            -device virtio-gpu-pci -usb -device usb-mouse -serial stdio
    }
} else {
    Write-Host "Para rodar no QEMU: .\build.ps1 -Arch $Arch --run" -ForegroundColor Yellow
}
