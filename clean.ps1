Write-Host "==============================="
Write-Host " Atom OS - Clean Build Script"
Write-Host "==============================="

$ErrorActionPreference = "SilentlyContinue"

# =========================
# Função segura de remoção
# =========================
function SafeRemove($path) {
    if (Test-Path $path) {
        Write-Host "Removendo: $path"
        Remove-Item -Recurse -Force -Path $path
    }
}

# =========================
# Raiz do projeto
# =========================
$ROOT = Get-Location

# =========================
# Diretórios principais
# =========================
$mainDirs = @(
    "$ROOT\target",
    "$ROOT\build",
    "$ROOT\images",
    "$ROOT\EFI",          # <<< EFI ROOT
    "$ROOT\efi\EFI",      # <<< EFI/EFI
    "$ROOT\efi\drivers",  # <<< EFI drivers
    "$ROOT\ovmf\build"
)

foreach ($dir in $mainDirs) {
    SafeRemove $dir
}

# =========================
# Remove todos os target/ recursivamente
# =========================
Get-ChildItem -Path $ROOT -Recurse -Directory -Filter "target" | ForEach-Object {
    SafeRemove $_.FullName
}

# =========================
# Remove pastas padrão do Cargo
# =========================
$buildArtifacts = @(
    ".fingerprint",
    "incremental",
    "build",
    "deps",
    "examples"
)

foreach ($artifact in $buildArtifacts) {
    Get-ChildItem -Path $ROOT -Recurse -Directory -Filter $artifact | ForEach-Object {
        SafeRemove $_.FullName
    }
}

# =========================
# Tools
# =========================
SafeRemove "$ROOT\tools\elf2atxf\target"

# =========================
# Userspace
# =========================
SafeRemove "$ROOT\build\userspace"

# =========================
# Logs
# =========================
Get-ChildItem -Path $ROOT -Recurse -File -Filter "*.log" | ForEach-Object {
    Write-Host "Removendo log: $($_.FullName)"
    Remove-Item -Force $_.FullName
}

# =========================
# Locks
# =========================
Get-ChildItem -Path $ROOT -Recurse -File -Filter "*.lock" | ForEach-Object {

    # Protege Cargo.lock (comportamento seguro)
    if ($_.Name -ne "Cargo.lock") {
        Write-Host "Removendo lock: $($_.FullName)"
        Remove-Item -Force $_.FullName
    }
}

# =========================
# Limpeza explícita UEFI
# =========================
SafeRemove "$ROOT\efi\build"
SafeRemove "$ROOT\efi\EFI\BOOT"
SafeRemove "$ROOT\efi\EFI\Atom"
SafeRemove "$ROOT\efi\drivers\build"

# =========================
# Fim
# =========================
Write-Host "==============================="
Write-Host " Clean completo finalizado!"
Write-Host " Sistema pronto para rebuild"
Write-Host "==============================="
