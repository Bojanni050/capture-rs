# Bouwt capture.exe en daarna de per-user installer.
#
#   .\installer\build.ps1
#
# Vereist: Rust (stable, msvc) en Inno Setup 6.3+ (`choco install innosetup`).
# Resultaat: dist\Capture-<versie>-setup.exe

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)

# Eén versie, twee bestanden: Cargo.toml is de bron, capture.iss volgt. Liever
# hier stuklopen dan een setup.exe afleveren die een andere versie meldt dan
# `capture --version`.
$cargoVersion = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value
$issVersion = (Select-String -Path installer\capture.iss -Pattern '^#define AppVersion "(.+)"').Matches[0].Groups[1].Value
if ($cargoVersion -ne $issVersion) {
    throw "installer\capture.iss noemt versie $issVersion, Cargo.toml $cargoVersion. Werk AppVersion in capture.iss bij."
}

$iscc = @(
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) {
    throw "ISCC.exe niet gevonden. Installeer Inno Setup 6: choco install innosetup"
}

cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build --release is mislukt" }

& $iscc "installer\capture.iss"
if ($LASTEXITCODE -ne 0) { throw "ISCC is mislukt" }

Write-Host "Klaar: dist\Capture-$cargoVersion-setup.exe"
