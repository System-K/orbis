# =============================================================================
# Orbis — Release-Packaging (Windows PowerShell)
# =============================================================================
# Baut ein Release-Binary und packt alles in einen verteilbaren Ordner.
#
# Nutzung:
#   .\scripts\package.ps1               # Release-Build + Packaging
#   .\scripts\package.ps1 -SkipBuild    # Nur Packaging
# =============================================================================

param(
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

$ReleaseName = "orbis-windows-x86_64"
$ReleaseDir  = "release\$ReleaseName"
$BinaryName  = "orbis.exe"
$BinarySrc   = "target\release\$BinaryName"

Write-Host "=== Orbis Release: $ReleaseName ===" -ForegroundColor Cyan

# Build
if (-not $SkipBuild) {
    Write-Host ">>> cargo build --release"
    cargo build --release
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FEHLER: Build fehlgeschlagen!" -ForegroundColor Red
        exit 1
    }
}

# Binary prüfen
if (-not (Test-Path $BinarySrc)) {
    Write-Host "FEHLER: Binary nicht gefunden: $BinarySrc" -ForegroundColor Red
    Write-Host "Führe zuerst 'cargo build --release' aus."
    exit 1
}

# Release-Ordner erstellen
Write-Host ">>> Erstelle $ReleaseDir\"
if (Test-Path $ReleaseDir) { Remove-Item $ReleaseDir -Recurse -Force }
New-Item -ItemType Directory -Path $ReleaseDir -Force | Out-Null

# Binary kopieren
Copy-Item $BinarySrc "$ReleaseDir\"

# Assets kopieren
Copy-Item -Recurse "assets" "$ReleaseDir\assets"

# Dokumentation kopieren (falls vorhanden)
if (Test-Path "README.md")  { Copy-Item "README.md" "$ReleaseDir\" }
if (Test-Path "LICENSE")    { Copy-Item "LICENSE" "$ReleaseDir\" }

# Zip erstellen
$ZipPath = "release\$ReleaseName.zip"
Write-Host ">>> Erstelle $ZipPath"
if (Test-Path $ZipPath) { Remove-Item $ZipPath -Force }
Compress-Archive -Path $ReleaseDir -DestinationPath $ZipPath -CompressionLevel Optimal

# Statistik
$BinarySize = (Get-Item "$ReleaseDir\$BinaryName").Length / 1MB
Write-Host ""
Write-Host "=== Fertig ===" -ForegroundColor Green
Write-Host ("Binary:  {0:N1} MB" -f $BinarySize)
Write-Host "Ordner:  $ReleaseDir\"
Write-Host "Archiv:  $ZipPath"
Write-Host ""
Get-ChildItem $ReleaseDir | Format-Table Name, Length -AutoSize
