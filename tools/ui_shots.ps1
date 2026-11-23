# Renders every scene of the ui_shot dev tool to a PNG, so agents can see the client UI before the Game Director does.
# Usage:  powershell -ExecutionPolicy Bypass -File tools/ui_shots.ps1 -OutDir <folder> [-Offline]
#   -OutDir   folder for the PNG files, 1 per scene, named <scene>.png. Created when missing.
#   -Offline  passes --offline to cargo, for machines or sandboxes without network.
# The scene list comes from `ui_shot --list`, so a new scene in crates/zeta_client/src/bin/ui_shot.rs needs no edit here.
# Exits 0 only when every scene saved its file. Success is read from exit codes, never from stderr.
param([Parameter(Mandatory = $true)][string]$OutDir, [switch]$Offline)

$ErrorActionPreference = 'Continue'
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo

# rustup installs cargo outside PATH on some machines, so fall back to the default install folder.
$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargo) { $cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
if (-not (Test-Path $cargo)) { Write-Host 'cargo not found. Install Rust with rustup.'; exit 1 }

$network = @()
if ($Offline) { $network = @('--offline') }
$run = @('run', '-p', 'zeta_client', '--bin', 'ui_shot', '--locked') + $network + @('--')

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$folder = (Resolve-Path $OutDir).Path

$scenes = @(& $cargo @($run + @('--list')))
if ($LASTEXITCODE -ne 0 -or $scenes.Count -eq 0) { Write-Host 'Could not build ui_shot or list its scenes.'; exit 1 }

$failed = @()
foreach ($scene in $scenes) {
    $file = Join-Path $folder "$scene.png"
    Write-Host "== $scene =="
    & $cargo @($run + @($scene, $file))
    if ($LASTEXITCODE -ne 0) { $failed += $scene }
}

Write-Host ''
if ($failed.Count -gt 0) { Write-Host "FAILED: $($failed -join ', ')"; exit 1 }
Write-Host "Saved $($scenes.Count) scenes to $folder"
exit 0
