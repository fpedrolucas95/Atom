# build.ps1
# Script de build para o kernel Atom no Windows
# Uso:
#   .\build.ps1
#   .\build.ps1 --clean
#   .\build.ps1 --run
#   .\build.ps1 --clean --run

param(
    [switch]$Run,
    [switch]$Clean
)

# -------------------------------------------------------------------------
# Configurações específicas da máquina
# -------------------------------------------------------------------------

$NASM_PATH  = "C:\Program Files\NASM\nasm.exe"
$REPO_PATH  = "C:\Users\amand\source\repos\Atom"
$RUST_LLD   = "$env:USERPROFILE\.rustup\toolchains\nightly-x86_64-pc-windows-msvc\lib\rustlib\x86_64-pc-windows-msvc\bin\rust-lld.exe"
$OVMF_PATH  = "$REPO_PATH\ovmf\OVMF.fd"

# -------------------------------------------------------------------------
# Funções auxiliares
# -------------------------------------------------------------------------

function Write-Step {
    param([string]$Message)
    Write-Host '[✗] $Message' -ForegroundColor Cyan
}

function Write-Success {
    param([string]$Message)
    Write-Host '[✓] $Message' -ForegroundColor Green
}

function Write-ErrorMsg {
    param([string]$Message)
    Write-Host '[✗] $Message' -ForegroundColor Red
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
# Preparar diretório build
# -------------------------------------------------------------------------

if (-not (Test-Path "build")) {
    New-Item -ItemType Directory -Path "build" | Out-Null
}

# -------------------------------------------------------------------------
# Passo 1: Montar arquivos assembly
# -------------------------------------------------------------------------

Write-Step "Montando boot.asm com NASM..."

if (-not (Test-Path $NASM_PATH)) {
    Write-ErrorMsg "NASM não encontrado em: $NASM_PATH"
    exit 1
}

& $NASM_PATH -f win64 arch\x86_64\boot.asm -o build\boot.obj
if ($LASTEXITCODE -ne 0) {
    Write-ErrorMsg "Falha ao montar boot.asm"
    exit 1
}

Write-Success "boot.obj criado"

Write-Step "Montando handlers.asm com NASM..."

if (Test-Path "build\handlers.obj") {
    Remove-Item -Force "build\handlers.obj"
}

& $NASM_PATH -f win64 kernel\src\interrupts\handlers.asm -o build\handlers.obj
if ($LASTEXITCODE -ne 0) {
    Write-ErrorMsg "Falha ao montar handlers.asm"
    exit 1
}

Write-Success "handlers.obj criado"

Write-Step "Montando switch.asm com NASM..."

& $NASM_PATH -f win64 kernel\src\interrupts\switch.asm -o build\switch.obj
if ($LASTEXITCODE -ne 0) {
    Write-ErrorMsg "Falha ao montar switch.asm"
    exit 1
}

Write-Success "switch.obj criado"

Write-Step "Montando syscall handler.asm com NASM..."

& $NASM_PATH -f win64 kernel\src\syscall\handler.asm -o build\syscall_handler.obj
if ($LASTEXITCODE -ne 0) {
    Write-ErrorMsg "Falha ao montar handler.asm"
    exit 1
}

Write-Success "syscall_handler.obj criado"

# -------------------------------------------------------------------------
# Passo 2: Compilar kernel Rust
# -------------------------------------------------------------------------

Write-Step "Compilando kernel Rust..."

cargo build -p atom-kernel --release
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
# Passo 4: Copiar para EFI/BOOT
# -------------------------------------------------------------------------

Write-Step "Atualizando BOOTX64.EFI..."

if (-not (Test-Path "efi\EFI\BOOT")) {
    New-Item -ItemType Directory -Path "efi\EFI\BOOT" -Force | Out-Null
}

Copy-Item build\Atom.efi efi\EFI\BOOT\BOOTX64.EFI -Force
if ($LASTEXITCODE -ne 0) {
    Write-ErrorMsg "Falha ao copiar BOOTX64.EFI"
    exit 1
}

Write-Success "BOOTX64.EFI atualizado"

Write-Host ""
Write-Success "Build concluído com sucesso!"
Write-Host ""

# -------------------------------------------------------------------------
# Executar QEMU (opcional)
# -------------------------------------------------------------------------

if ($Run) {
    Write-Step "Iniciando QEMU..."

    if (-not (Test-Path $OVMF_PATH)) {
        Write-ErrorMsg "OVMF.fd não encontrado em: $OVMF_PATH"
        exit 1
    }

    Write-Host "Pressione Ctrl+C para encerrar o QEMU" -ForegroundColor Yellow
    Write-Host ""

    qemu-system-x86_64 `
        -machine q35 `
        -cpu qemu64 `
        -m 512M `
        -bios "$OVMF_PATH" `
        -drive format=raw,file=fat:rw:"$REPO_PATH\efi" `
        -serial stdio `
        -debugcon stdio `
        -global isa-debugcon.iobase=0xE9
}
else {
    Write-Host "Para testar no QEMU, execute:" -ForegroundColor Yellow
    Write-Host '  .\build.ps1 --run' -ForegroundColor Yellow
}