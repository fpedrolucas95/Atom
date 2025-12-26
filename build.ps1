# build.ps1
# Script de build para o kernel Atom no Windows
# Uso:
#   .\build.ps1              # Build completo (kernel + userspace)
#   .\build.ps1 --clean      # Limpar e rebuildar
#   .\build.ps1 --run        # Build e executar no QEMU
#   .\build.ps1 --userspace  # Build apenas drivers userspace
#   .\build.ps1 --kernel     # Build apenas kernel

param(
    [switch]$Run,
    [switch]$Clean,
    [switch]$Userspace,
    [switch]$Kernel
)

# -------------------------------------------------------------------------
# Configurações
# -------------------------------------------------------------------------

$NASM_PATH  = "C:\Program Files\NASM\nasm.exe"
$REPO_PATH  = $PSScriptRoot
$RUST_LLD   = "$env:USERPROFILE\.rustup\toolchains\nightly-x86_64-pc-windows-msvc\lib\rustlib\x86_64-pc-windows-msvc\bin\rust-lld.exe"
$OVMF_PATH  = "$REPO_PATH\ovmf\OVMF.fd"

# Userspace drivers list
$USERSPACE_DRIVERS = @(
    "keyboard",
    "mouse", 
    "display",
    "ui_shell"
)

# -------------------------------------------------------------------------
# Funções auxiliares
# -------------------------------------------------------------------------

function Write-Step {
    param([string]$Message)
    Write-Host "[*] $Message" -ForegroundColor Cyan
}

function Write-Success {
    param([string]$Message)
    Write-Host "[OK] $Message" -ForegroundColor Green
}

function Write-ErrorMsg {
    param([string]$Message)
    Write-Host "[X] $Message" -ForegroundColor Red
}

function Write-Warning {
    param([string]$Message)
    Write-Host "[!] $Message" -ForegroundColor Yellow
}

# -------------------------------------------------------------------------
# Verificações iniciais
# -------------------------------------------------------------------------

if (-not (Test-Path "kernel\Cargo.toml")) {
    Write-ErrorMsg "Este script deve ser executado na raiz do repositório Atom"
    exit 1
}

# -------------------------------------------------------------------------
# Clean opcional
# -------------------------------------------------------------------------

if ($Clean) {
    Write-Step "Limpando arquivos de build..."
    if (Test-Path "build") {
        Remove-Item -Recurse -Force build\*
    }
    cargo clean
    Write-Success "Build limpo"
}

# -------------------------------------------------------------------------
# Preparar diretórios
# -------------------------------------------------------------------------

if (-not (Test-Path "build")) {
    New-Item -ItemType Directory -Path "build" | Out-Null
}

if (-not (Test-Path "build\userspace")) {
    New-Item -ItemType Directory -Path "build\userspace" | Out-Null
}

if (-not (Test-Path "efi\EFI\BOOT")) {
    New-Item -ItemType Directory -Path "efi\EFI\BOOT" -Force | Out-Null
}

if (-not (Test-Path "efi\drivers")) {
    New-Item -ItemType Directory -Path "efi\drivers" -Force | Out-Null
}

# =========================================================================
# BUILD USERSPACE DRIVERS (Library only - drivers are embedded in kernel)
# =========================================================================

if (-not $Kernel) {
    Write-Host ""
    Write-Host "========== USERSPACE LIBRARIES ==========" -ForegroundColor Magenta
    Write-Host ""

    # Compilar biblioteca syscall (usada pelo shell embutido no kernel)
    Write-Step "Compilando biblioteca atom_syscall..."
    
    # A biblioteca syscall é compilada automaticamente como dependência
    # quando o kernel é compilado. Aqui apenas verificamos se compila.
    cargo check -p atom_syscall 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-ErrorMsg "Falha ao verificar atom_syscall"
        exit 1
    }
    Write-Success "atom_syscall verificada"

    # Nota: Os drivers userspace (keyboard, mouse, display, ui_shell) são
    # compilados como crates separadas que eventualmente serao carregadas
    # como binarios ATXF. Por enquanto, o kernel usa um shell embutido
    # (shell.rs) que roda em Ring 3.
    
    Write-Step "Verificando drivers userspace..."
    foreach ($driver in $USERSPACE_DRIVERS) {
        $driverPath = "userspace\drivers\$driver"
        
        if (-not (Test-Path "$driverPath\Cargo.toml")) {
            Write-Warning "Driver $driver nao encontrado"
            continue
        }

        # Verificar sintaxe diretamente no diretorio do driver
        # (drivers estao excluidos do workspace principal)
        Push-Location $driverPath
        cargo check 2>&1 | Out-Null
        $checkResult = $LASTEXITCODE
        Pop-Location
        
        if ($checkResult -eq 0) {
            Write-Success "$driver driver verificado"
        } else {
            Write-Warning "$driver driver tem erros de sintaxe"
        }
    }

    Write-Success "Verificacao de userspace concluida"
}

# Se --userspace only, parar aqui
if ($Userspace) {
    Write-Host ""
    Write-Success "Verificacao userspace concluida!"
    Write-Host ""
    Write-Host "Nota: Os drivers userspace sao verificados apenas." -ForegroundColor Yellow
    Write-Host "O kernel usa um shell embutido (shell.rs) que roda em Ring 3." -ForegroundColor Yellow
    exit 0
}

# =========================================================================
# BUILD UI_SHELL AS ATXF BINARY
# =========================================================================

Write-Host ""
Write-Host "========== UI_SHELL BUILD ==========" -ForegroundColor Magenta
Write-Host ""

# Build elf2atxf tool first
# Use stable toolchain and explicit native target to avoid inheriting kernel's build-std settings
Write-Step "Compilando ferramenta elf2atxf..."
Push-Location tools\elf2atxf
$nativeTarget = "x86_64-pc-windows-msvc"
$env:CARGO_BUILD_TARGET = $nativeTarget
cargo +stable build --release --target $nativeTarget 2>&1 | Out-Null
$toolResult = $LASTEXITCODE
$env:CARGO_BUILD_TARGET = $null
Pop-Location

if ($toolResult -ne 0) {
    Write-ErrorMsg "Falha ao compilar elf2atxf"
    exit 1
}
Write-Success "elf2atxf compilado"

Write-Step "Compilando ui_shell..."
Push-Location userspace\drivers\ui_shell
cargo build --release 2>&1 | Out-File -FilePath ..\..\..\build\ui_shell_cargo.log
$uiResult = $LASTEXITCODE
Pop-Location

if ($uiResult -ne 0) {
    Write-ErrorMsg "Falha ao compilar ui_shell"
    exit 1
}
Write-Success "ui_shell compilado"

Write-Step "Gerando binário ATXF..."
& tools\elf2atxf\target\x86_64-pc-windows-msvc\release\elf2atxf.exe `
    userspace\drivers\ui_shell\target\x86_64-unknown-none\release\ui_shell `
    build\ui_shell.atxf 2>&1 | Out-File -FilePath build\atxf.log

if ($LASTEXITCODE -ne 0) {
    Write-ErrorMsg "Falha ao gerar ATXF"
    Get-Content build\atxf.log
    exit 1
}
Write-Success "ui_shell.atxf gerado"

Write-Step "Copiando ui_shell.atxf para kernel/src/..."
Copy-Item build\ui_shell.atxf kernel\src\ui_shell.atxf -Force
Write-Success "ui_shell.atxf pronto para embedding"

# =========================================================================
# BUILD KERNEL
# =========================================================================

Write-Host ""
Write-Host "========== KERNEL BUILD ==========" -ForegroundColor Magenta
Write-Host ""

# -------------------------------------------------------------------------
# Passo 1: Montar arquivos assembly
# -------------------------------------------------------------------------

Write-Step "Montando arquivos assembly..."

if (-not (Test-Path $NASM_PATH)) {
    Write-ErrorMsg "NASM não encontrado em: $NASM_PATH"
    Write-Warning "Instale NASM de: https://www.nasm.us/"
    exit 1
}

# boot.asm
& $NASM_PATH -f win64 arch\x86_64\boot.asm -o build\boot.obj 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-ErrorMsg "Falha ao montar boot.asm"
    exit 1
}

# handlers.asm
if (Test-Path "build\handlers.obj") { Remove-Item -Force "build\handlers.obj" }
& $NASM_PATH -f win64 kernel\src\interrupts\handlers.asm -o build\handlers.obj 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-ErrorMsg "Falha ao montar handlers.asm"
    exit 1
}

# switch.asm
& $NASM_PATH -f win64 kernel\src\interrupts\switch.asm -o build\switch.obj 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-ErrorMsg "Falha ao montar switch.asm"
    exit 1
}

# syscall handler.asm
& $NASM_PATH -f win64 kernel\src\syscall\handler.asm -o build\syscall_handler.obj 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-ErrorMsg "Falha ao montar syscall/handler.asm"
    exit 1
}

Write-Success "Arquivos assembly montados"

# -------------------------------------------------------------------------
# Passo 2: Compilar kernel Rust
# -------------------------------------------------------------------------

Write-Step "Compilando kernel Rust..."

cargo build -p atom-kernel --release 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-ErrorMsg "Falha ao compilar o kernel"
    exit 1
}

Write-Success "Kernel compilado"

# -------------------------------------------------------------------------
# Passo 3: Linkar Atom.efi
# -------------------------------------------------------------------------

Write-Step "Linkando Atom.efi..."

if (-not (Test-Path $RUST_LLD)) {
    Write-ErrorMsg "rust-lld não encontrado em: $RUST_LLD"
    exit 1
}

& $RUST_LLD `
    -flavor link `
    build\boot.obj `
    build\handlers.obj `
    build\switch.obj `
    build\syscall_handler.obj `
    target\x86_64-unknown-uefi\release\libatom.a `
    /OUT:build\Atom.efi `
    /SUBSYSTEM:EFI_APPLICATION `
    /ENTRY:efi_entry `
    /NODEFAULTLIB

if ($LASTEXITCODE -ne 0) {
    Write-ErrorMsg "Falha ao linkar Atom.efi"
    exit 1
}

Write-Success "Atom.efi criado"

# -------------------------------------------------------------------------
# Passo 4: Preparar imagem EFI para QEMU
# -------------------------------------------------------------------------

Write-Step "Preparando imagem EFI..."

Copy-Item build\Atom.efi efi\EFI\BOOT\BOOTX64.EFI -Force

Write-Success "BOOTX64.EFI atualizado"

# =========================================================================
# SUMÁRIO DO BUILD
# =========================================================================

Write-Host ""
Write-Host "========== BUILD COMPLETO ==========" -ForegroundColor Green
Write-Host ""
Write-Host "Kernel:     build\Atom.efi" -ForegroundColor White
Write-Host "EFI Image:  efi\EFI\BOOT\BOOTX64.EFI" -ForegroundColor White
Write-Host "Drivers:    efi\drivers\" -ForegroundColor White
Write-Host ""

# Lista de drivers compilados
if (Test-Path "efi\drivers") {
    $drivers = Get-ChildItem "efi\drivers\*.bin" -ErrorAction SilentlyContinue
    if ($drivers) {
        Write-Host "Drivers userspace:" -ForegroundColor Cyan
        foreach ($d in $drivers) {
            Write-Host "  - $($d.Name)" -ForegroundColor White
        }
        Write-Host ""
    }
}

# -------------------------------------------------------------------------
# Executar QEMU (opcional)
# -------------------------------------------------------------------------

if ($Run) {
    Write-Host "========== QEMU ==========" -ForegroundColor Magenta
    Write-Host ""
    
    if (-not (Test-Path $OVMF_PATH)) {
        Write-ErrorMsg "OVMF.fd não encontrado em: $OVMF_PATH"
        Write-Warning "Baixe OVMF de: https://github.com/tianocore/edk2"
        exit 1
    }

    # Verificar se QEMU está instalado
    $qemu = Get-Command "qemu-system-x86_64" -ErrorAction SilentlyContinue
    if (-not $qemu) {
        Write-ErrorMsg "QEMU não encontrado no PATH"
        Write-Warning "Instale QEMU de: https://www.qemu.org/download/"
        exit 1
    }

    Write-Step "Iniciando QEMU..."
    Write-Host "Pressione Ctrl+C para encerrar" -ForegroundColor Yellow
    Write-Host ""

    # Executar QEMU com suporte a mouse PS/2
    qemu-system-x86_64 `
        -machine q35 `
        -cpu qemu64 `
        -m 512M `
        -bios "$OVMF_PATH" `
        -drive format=raw,file=fat:rw:"$REPO_PATH\efi" `
        -device VGA `
        -usb `
        -device usb-mouse `
        -serial stdio `
        -debugcon file:serial_log.txt `
        -global isa-debugcon.iobase=0xE9
}
else {
    Write-Host "Para testar no QEMU:" -ForegroundColor Yellow
    Write-Host "  .\build.ps1 --run" -ForegroundColor Yellow
    Write-Host ""
    Write-Host "Outras opções:" -ForegroundColor Yellow  
    Write-Host "  .\build.ps1 --clean      # Limpar e rebuildar" -ForegroundColor Yellow
    Write-Host "  .\build.ps1 --userspace  # Apenas drivers userspace" -ForegroundColor Yellow
    Write-Host "  .\build.ps1 --kernel     # Apenas kernel" -ForegroundColor Yellow
}
