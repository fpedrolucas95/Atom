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

# Userspace drivers: directory name -> binary name mapping
# The binary name comes from the "name" field in each driver's Cargo.toml
$USERSPACE_DRIVERS = @{
    "keyboard"  = "keyboard_driver"
    "mouse"     = "mouse_driver"
    "display"   = "display_driver"
    "ui_shell"  = "atom_desktop"
}

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
# BUILD ELF2ATXF TOOL
# =========================================================================

Write-Host ""
Write-Host "========== ELF2ATXF TOOL ==========" -ForegroundColor Magenta
Write-Host ""

$ELF2ATXF_PATH = "tools\elf2atxf"
$ELF2ATXF_EXE = "$ELF2ATXF_PATH\target\release\elf2atxf.exe"

if (-not (Test-Path $ELF2ATXF_EXE) -or $Clean) {
    Write-Step "Compilando elf2atxf tool..."

    Push-Location $ELF2ATXF_PATH
    cargo build --release 2>&1 | Tee-Object -FilePath "build.log"
    $buildResult = $LASTEXITCODE
    Pop-Location

    if ($buildResult -ne 0) {
        Write-ErrorMsg "Falha ao compilar elf2atxf"
        exit 1
    }
    Write-Success "elf2atxf compilado"
} else {
    Write-Success "elf2atxf já existe (use --clean para recompilar)"
}

# =========================================================================
# BUILD USERSPACE DRIVERS (ATXF format)
# =========================================================================

if (-not $Kernel) {
    Write-Host ""
    Write-Host "========== USERSPACE DRIVERS ==========" -ForegroundColor Magenta
    Write-Host ""

    # Compilar biblioteca syscall primeiro
    Write-Step "Compilando biblioteca atom_syscall..."
    cargo check -p atom_syscall 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-ErrorMsg "Falha ao verificar atom_syscall"
        exit 1
    }
    Write-Success "atom_syscall verificada"

    # Compilar cada driver userspace
    foreach ($driverDir in $USERSPACE_DRIVERS.Keys) {
        $binaryName = $USERSPACE_DRIVERS[$driverDir]
        $driverPath = "userspace\drivers\$driverDir"

        if (-not (Test-Path "$driverPath\Cargo.toml")) {
            Write-Warning "Driver $driverDir nao encontrado"
            continue
        }

        Write-Step "Compilando driver $driverDir ($binaryName)..."

        Push-Location $driverPath
        cargo build --release 2>&1 | Tee-Object -FilePath "build.log"
        $buildResult = $LASTEXITCODE
        Pop-Location

        if ($buildResult -ne 0) {
            Write-ErrorMsg "Falha ao compilar driver $driverDir"
            exit 1
        }

        # Encontrar o binário ELF gerado (use binary name from Cargo.toml)
        $elfPath = "$driverPath\target\x86_64-unknown-none\release\$binaryName"
        if (-not (Test-Path $elfPath)) {
            Write-Warning "Binario ELF nao encontrado: $elfPath"
            continue
        }

        # Converter ELF para ATXF
        $atxfPath = "efi\drivers\$driverDir.atxf"
        Write-Step "Convertendo $binaryName para ATXF..."

        # Use Start-Process to avoid PowerShell path interpretation issues
        $elf2atxfFullPath = Join-Path $REPO_PATH $ELF2ATXF_EXE
        $elfFullPath = Join-Path $REPO_PATH $elfPath
        $atxfFullPath = Join-Path $REPO_PATH $atxfPath

        $process = Start-Process -FilePath $elf2atxfFullPath -ArgumentList "`"$elfFullPath`"", "`"$atxfFullPath`"" -Wait -PassThru -NoNewWindow
        if ($process.ExitCode -ne 0) {
            Write-ErrorMsg "Falha ao converter $driverDir para ATXF (exit code: $($process.ExitCode))"
            exit 1
        }

        Write-Success "$driverDir.atxf criado"
    }

    # Copiar ui_shell.atxf para o diretório de boot como init.atxf
    if (Test-Path "efi\drivers\ui_shell.atxf") {
        Copy-Item "efi\drivers\ui_shell.atxf" "efi\EFI\BOOT\init.atxf" -Force
        Write-Success "init.atxf criado a partir de ui_shell"
    } else {
        Write-ErrorMsg "ui_shell.atxf nao encontrado - o kernel nao podera iniciar!"
    }

    Write-Success "Compilação de userspace concluída"
}

# Se --userspace only, parar aqui
if ($Userspace) {
    Write-Host ""
    Write-Success "Build userspace concluído!"
    Write-Host ""
    Write-Host "Arquivos gerados:" -ForegroundColor Cyan
    Get-ChildItem "efi\drivers\*.atxf" | ForEach-Object { Write-Host "  - $($_.Name)" }
    Write-Host "  - efi\EFI\BOOT\init.atxf" -ForegroundColor White
    exit 0
}

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
Write-Host "Init:       efi\EFI\BOOT\init.atxf" -ForegroundColor White
Write-Host "Drivers:    efi\drivers\" -ForegroundColor White
Write-Host ""

# Lista de drivers compilados
if (Test-Path "efi\drivers") {
    $drivers = Get-ChildItem "efi\drivers\*.atxf" -ErrorAction SilentlyContinue
    if ($drivers) {
        Write-Host "Drivers userspace (ATXF):" -ForegroundColor Cyan
        foreach ($d in $drivers) {
            $size = [math]::Round($d.Length / 1024, 1)
            Write-Host "  - $($d.Name) ($size KB)" -ForegroundColor White
        }
        Write-Host ""
    }
}

# Verificar se init.atxf existe
if (Test-Path "efi\EFI\BOOT\init.atxf") {
    $initSize = [math]::Round((Get-Item "efi\EFI\BOOT\init.atxf").Length / 1024, 1)
    Write-Host "Init payload: init.atxf ($initSize KB)" -ForegroundColor Green
} else {
    Write-Host "AVISO: init.atxf não encontrado! O kernel não poderá iniciar o UI shell." -ForegroundColor Red
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
