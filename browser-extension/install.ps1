# Laadt de Capture Browser Bridge (Chromium-variant) automatisch in een
# Chromium-browser:
#
#   .\install.ps1                 # start Chrome met de extensie
#   .\install.ps1 -Browser Edge   # of brave / vivaldi / opera
#   .\install.ps1 -ProfileDir ...  # blijvend profiel, anders tijdelijk
#
# Herladen na een bestandswijziging: opnieuw draaien start een nieuwe
# sessie met de actuele map. Voor live-herladen tijdens het ontwikkelen
# gebruik de "Herlaad"-knop in chrome://extensions (Ontwikkelaarsmodus).

param(
    [ValidateSet("chrome", "edge", "brave", "vivaldi", "opera")]
    [string]$Browser = "chrome",

    # Tijdelijk profiel houdt je normale browservenster buiten schot;
    # geef een pad op voor een blijvende sessie.
    [string]$ProfileDir = ""
)

$ErrorActionPreference = "Stop"
$extDir = Join-Path $PSScriptRoot "browser-extension"

if (-not (Test-Path (Join-Path $extDir "manifest.json"))) {
    Write-Error "Geen manifest.json gevonden in $extDir. Draai dit script vanuit de repo-map."
}

$paths = @{
    chrome  = @("${env:ProgramFiles}\Google\Chrome\Application\chrome.exe",
                "${env:ProgramFiles(x86)}\Google\Chrome\Application\chrome.exe",
                "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe")
    edge    = @("${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe",
                "${env:ProgramFiles}\Microsoft\Edge\Application\msedge.exe")
    brave   = @("$env:LOCALAPPDATA\BraveSoftware\Brave-Browser\Application\brave.exe",
                "${env:ProgramFiles}\BraveSoftware\Brave-Browser\Application\brave.exe")
    vivaldi = @("$env:LOCALAPPDATA\Vivaldi\Application\vivaldi.exe",
                "${env:ProgramFiles}\Vivaldi\Application\vivaldi.exe")
    opera   = @("$env:LOCALAPPDATA\Programs\Opera\opera.exe",
                "${env:ProgramFiles}\Opera\opera.exe")
}

$exe = $paths[$Browser] | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $exe) {
    Write-Error "Kon $Browser niet vinden. Geef het pad op de opdrachtregel of installeer de browser."
}

if (-not $ProfileDir) {
    $ProfileDir = Join-Path $env:TEMP "capture-ext-profile"
    New-Item -ItemType Directory -Force -Path $ProfileDir | Out-Null
}

Write-Host "Extensie : $extDir"
Write-Host "Browser  : $exe"
Write-Host "Profiel  : $ProfileDir"

& $exe `
    "--user-data-dir=$ProfileDir" `
    "--load-extension=$extDir" `
    "--no-first-run" `
    "about:blank"

Write-Host "Klaar. De extensie is geladen; zie chrome://extensions."
