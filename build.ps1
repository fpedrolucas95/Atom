# build.ps1
# Script de build para o kernel Atom no Windows
# Uso:
#   .\build.ps1                    # Build completo (kernel + userspace)
#   .\build.ps1 --clean            # Limpar e rebuildar
#   .\build.ps1 --run              # Build e executar no QEMU
#   .\build.ps1 --userspace        # Build apenas drivers + services userspace
#   .\build.ps1 --kernel           # Build apenas kernel
#   .\build.ps1 --rust-only        # Apenas compilar Rust (sem assembly/linking)
#   .\build.ps1 --setup            # Configurar dependências Rust

param(
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
$OVMF_PATH  = "$REPO_PATH\ovmf\OVMF.fd"

# Apenas as pastas dos drivers (não precisamos mais do nome do binário aqui)
$USERSPACE_DRIVERS_DIRS = @("keyboard", "mouse", "display", "terminal", "ui_shell")

# Services (incluindo init = PID 1)
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
    Header "SETUP"

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
    if ($targets -notmatch "x86_64-unknown-uefi") {
        Write-Step "Adicionando target x86_64-unknown-uefi..."
        rustup target add x86_64-unknown-uefi
        Write-Success "Target adicionado"
    }

    if ($targets -notmatch "x86_64-unknown-none") {
        Write-Step "Adicionando target x86_64-unknown-none (para elf2atxf)..."
        rustup +nightly target add x86_64-unknown-none
        Write-Success "Target x86_64-unknown-none adicionado"
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
$ELF2ATXF_EXE  = "$ELF2ATXF_PATH\target\x86_64-pc-windows-msvc\release\elf2atxf.exe"

if (-not (Test-Path $ELF2ATXF_EXE) -or $Clean) {
    Write-Step "Compilando elf2atxf tool..."

    Push-Location $ELF2ATXF_PATH
    cargo build --release --target x86_64-pc-windows-msvc *> "$REPO_PATH\build.log"
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
    Header "USERSPACE BUILD"

    # Função auxiliar para build + conversão ATXF
    function Build-And-Convert {
        param([string]$Path, [string]$Type)  # $Type = "driver" ou "service"

        if (-not (Test-Path "$Path\Cargo.toml")) {
            Write-Warning "$Type em $Path não encontrado, pulando..."
            return
        }

        $dirName = Split-Path $Path -Leaf
        Write-Step "Compilando $Type $dirName..."

        Push-Location $Path
        cargo build --release *> "$REPO_PATH\build.log"
        if ($LASTEXITCODE -ne 0) {
            Pop-Location
            Write-ErrorMsg "Falha ao compilar $Type $dirName"
        }
        Pop-Location

        # Descobrir nome do binário a partir de Cargo.toml - regex mais robusto
        $cargoContent = Get-Content "$Path\Cargo.toml" -Raw

        # Tentativa 1: procura name dentro de [[bin]]
        if ($cargoContent -match '(?smi)\[\[bin\]\].*?name\s*=\s*"(.*?)"') {
            $binName = $Matches[1].Trim()
            Write-Host "   [DEBUG] Binário extraído do Cargo.toml: $binName" -ForegroundColor DarkGray
        }
        # Tentativa 2: fallback para qualquer name = "..." no arquivo
        elseif ($cargoContent -match '(?smi)name\s*=\s*"(.*?)"') {
            $binName = $Matches[1].Trim()
            Write-Host "   [DEBUG] Binário encontrado via fallback name: $binName" -ForegroundColor DarkGray
        }
        # Último fallback: nome da pasta
        else {
            $binName = $dirName
            Write-Warning "Nenhum nome de binário encontrado em $Path\Cargo.toml - usando fallback: $binName"
        }

        $elfPath = "$Path\target\x86_64-unknown-none\release\$binName"
        if (-not (Test-Path $elfPath)) {
            Write-Warning "Binário ELF não encontrado: $elfPath (verifique se o nome em [[bin]] name é correto)"
            return
        }

        $atxfPath = "efi\drivers\$dirName.atxf"
        Write-Step "Convertendo $binName para ATXF..."

        $elf2atxfFull = Resolve-Path $ELF2ATXF_EXE -ErrorAction SilentlyContinue
        if (-not $elf2atxfFull) { Write-ErrorMsg "elf2atxf.exe não encontrado" }

        & $elf2atxfFull "$elfPath" "$atxfPath" *> "$REPO_PATH\build.log"
        if ($LASTEXITCODE -ne 0) { Write-ErrorMsg "Falha na conversão para ATXF ($LASTEXITCODE)" }

        Write-Success "$dirName.atxf criado"
    }

    # Drivers - agora usando a mesma função que services
    foreach ($driverDir in $USERSPACE_DRIVERS_DIRS) {
        Build-And-Convert "userspace\drivers\$driverDir" "driver"
    }

    # Services
    foreach ($service in $USERSPACE_SERVICES) {
        Build-And-Convert "userspace\services\$service" "service"
    }

    # Instalar init.atxf como payload de boot (PID 1)
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

Header "KERNEL BUILD"

Write-Step "Compilando kernel Rust..."
cargo build -p atom-kernel --release *> "$REPO_PATH\build.log"
if ($LASTEXITCODE -ne 0) { Write-ErrorMsg "Falha ao compilar kernel Rust" }

# Checar warnings (similar ao Linux)
if (Select-String "warning:" "$REPO_PATH\build.log") {
    Write-Warning "Build teve warnings (veja build.log)"
}

Write-Success "Kernel Rust compilado"

if ($RustOnly) {
    Write-Success "Build Rust-only concluído!"
    Write-Host "Arquivo gerado: target\x86_64-unknown-uefi\release\libatom.a"
    exit 0
}

if (-not $Kernel -and -not $Userspace) {  # Evita rodar assembly se --kernel only sem necessidade
    # =========================================================================
    # MONTAR ASSEMBLY
    # =========================================================================

    Write-Step "Montando arquivos assembly..."

    if (-not (Test-Path $NASM_PATH)) {
        Write-ErrorMsg "NASM não encontrado em: $NASM_PATH"
        Write-Warning "Instale de: https://www.nasm.us/"
    }

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
}

# =========================================================================
# LINKAR ATOM.EFI
# =========================================================================

Write-Step "Linkando Atom.efi..."

$RUST_LLD = "$env:USERPROFILE\.rustup\toolchains\nightly-x86_64-pc-windows-msvc\lib\rustlib\x86_64-pc-windows-msvc\bin\rust-lld.exe"
if (-not (Test-Path $RUST_LLD)) {
    Write-Warning "rust-lld não encontrado no caminho padrão. Tentando localizar..."
    $RUST_LLD = Get-ChildItem "$env:USERPROFILE\.rustup\toolchains\nightly*" -Recurse -File -Name "rust-lld.exe" -ErrorAction SilentlyContinue |
                ForEach-Object { Join-Path "$env:USERPROFILE\.rustup\toolchains" $_ } | Select-Object -First 1
    if (-not $RUST_LLD) { Write-ErrorMsg "rust-lld não encontrado" }
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
    /NODEFAULTLIB *> "$REPO_PATH\build.log"

if ($LASTEXITCODE -ne 0) { Write-ErrorMsg "Falha ao linkar Atom.efi" }

Write-Success "Atom.efi criado"

# Copiar para EFI
Copy-Item build\Atom.efi efi\EFI\BOOT\BOOTX64.EFI -Force
Write-Success "BOOTX64.EFI atualizado"

# =========================================================================
# SUMÁRIO
# =========================================================================

Header "BUILD COMPLETO"

Write-Host "Kernel:     build\Atom.efi" -ForegroundColor White
Write-Host "EFI Image:  efi\EFI\BOOT\BOOTX64.EFI" -ForegroundColor White
Write-Host "Init:       efi\EFI\BOOT\init.atxf" -ForegroundColor White
Write-Host "Drivers:    efi\drivers\" -ForegroundColor White
Write-Host ""

if (Test-Path "efi\drivers") {
    $files = Get-ChildItem "efi\drivers\*.atxf" -ErrorAction SilentlyContinue
    if ($files) {
        Write-Host "Arquivos ATXF gerados:" -ForegroundColor Cyan
        foreach ($f in $files) {
            $size = [math]::Round($f.Length / 1KB, 1)
            Write-Host "  - $($f.Name) ($size KB)" -ForegroundColor White
        }
    }
}

# =========================================================================
# QEMU
# =========================================================================

if ($Run) {
    Header "QEMU"

    if (-not (Test-Path $OVMF_PATH)) {
        Write-ErrorMsg "OVMF.fd não encontrado em: $OVMF_PATH"
        Write-Warning "Baixe de: https://github.com/tianocore/edk2"
    }

    $qemu = Get-Command "qemu-system-x86_64" -ErrorAction SilentlyContinue
    if (-not $qemu) {
        Write-ErrorMsg "qemu-system-x86_64 não encontrado"
        Write-Warning "Instale QEMU: https://www.qemu.org/download/"
    }

    Write-Step "Iniciando QEMU..."
    Write-Host "Pressione Ctrl+C para sair" -ForegroundColor Yellow

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
} else {
    Write-Host "Para rodar no QEMU: .\build.ps1 --run" -ForegroundColor Yellow
}