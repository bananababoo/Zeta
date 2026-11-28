# Renders every scene of the ui_shot dev tool to a PNG, so agents can see the client UI before the Game Director does.
# Usage:  powershell -ExecutionPolicy Bypass -File tools/ui_shots.ps1 -OutDir <folder> [-Offline] [-SceneFilter <pattern...>]
#   -OutDir       folder for the PNG files, 1 per scene, named <scene>.png. Created when missing.
#   -Offline      passes --offline to cargo, for machines or sandboxes without network.
#   -SceneFilter  optional scene names or wildcard patterns (e.g. biome_* or river_scene,planet_coast).
# The scene list comes from `ui_shot --list`, so a new scene in crates/zeta_client/src/bin/ui_shot.rs needs no edit here.
# Exits 0 only when every selected scene saved its file. Success is read from exit codes, never from stderr.
param(
    [Parameter(Mandatory = $true)][string]$OutDir,
    [switch]$Offline,
    [string[]]$SceneFilter = @()
)

$ErrorActionPreference = 'Continue'
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo

if (-not $env:CARGO_BUILD_JOBS) { $env:CARGO_BUILD_JOBS = '4' }

# rustup installs cargo outside PATH on some machines, so fall back to the default install folder.
$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargo) { $cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
if (-not (Test-Path $cargo)) { Write-Host 'cargo not found. Install Rust with rustup.'; exit 1 }

$network = @()
if ($Offline) { $network = @('--offline') }
$run = @('run', '-p', 'zeta_client', '--bin', 'ui_shot', '--locked') + $network + @('--')

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$folder = (Resolve-Path $OutDir).Path

$allScenes = @(& $cargo @($run + @('--list')))
if ($LASTEXITCODE -ne 0 -or $allScenes.Count -eq 0) { Write-Host 'Could not build ui_shot or list its scenes.'; exit 1 }

$scenes = @()
if ($SceneFilter.Count -eq 0) {
    $scenes = $allScenes
} else {
    $patterns = @()
    foreach ($entry in $SceneFilter) {
        foreach ($part in ($entry -split ',')) {
            $trimmed = $part.Trim()
            if ($trimmed.Length -gt 0) { $patterns += $trimmed }
        }
    }
    foreach ($scene in $allScenes) {
        foreach ($pattern in $patterns) {
            if ($scene -like $pattern) {
                $scenes += $scene
                break
            }
        }
    }
    if ($scenes.Count -eq 0) {
        Write-Host "No scenes matched -SceneFilter ($($patterns -join ', '))."
        exit 1
    }
}

foreach ($scene in $scenes) {
    $existing = Join-Path $folder "$scene.png"
    if (Test-Path $existing) { Remove-Item -Force $existing }
}

Write-Host "Rendering $($scenes.Count) scene(s) to $folder ..."
& $cargo @($run + @('--batch', $folder) + $scenes)
$batchExit = $LASTEXITCODE

$failed = @()
foreach ($scene in $scenes) {
    $file = Join-Path $folder "$scene.png"
    if (-not (Test-Path $file)) { $failed += $scene }
}

Write-Host ''
if ($batchExit -ne 0 -or $failed.Count -gt 0) {
    if ($failed.Count -gt 0) {
        Write-Host "FAILED: $($failed -join ', ')"
    } else {
        Write-Host "FAILED: ui_shot --batch exited with code $batchExit"
    }
    exit 1
}
Write-Host "Saved $($scenes.Count) scenes to $folder"
exit 0
