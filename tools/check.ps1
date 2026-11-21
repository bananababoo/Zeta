# Runs every pre-commit check from docs/architecture/code_style.md Section 1, in the same order as CI.
# Usage:  powershell -ExecutionPolicy Bypass -File tools/check.ps1 [-Fix] [-Offline] [-NoDocs]
#   -Fix      formats the code first, so the format check only fails on files rustfmt cannot fix.
#   -Offline  passes --offline to cargo, for machines or sandboxes without network.
#   -NoDocs   skips the documentation rules, for the CI job that only builds Rust.
# Exits 0 only when every check passes. Success is read from exit codes, never from stderr,
# because cargo writes progress to stderr even when it succeeds.
param([switch]$Fix, [switch]$Offline, [switch]$NoDocs)

$ErrorActionPreference = 'Continue'
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo

# rustup installs cargo outside PATH on some machines, so fall back to the default install folder.
$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargo) { $cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
if (-not (Test-Path $cargo)) { Write-Host 'cargo not found. Install Rust with rustup.'; exit 1 }

$network = @()
if ($Offline) { $network = @('--offline') }

$steps = @()
if ($Fix) { $steps += , @('Format (fix)', @('fmt', '--all')) }
$steps += , @('Format', @('fmt', '--all', '--check'))
$steps += , @('Check (core)', (@('check', '-p', 'zeta_core', '--no-default-features', '--locked') + $network))
$steps += , @('Lint', (@('clippy', '--workspace', '--all-targets', '--all-features', '--locked') + $network + @('--', '-D', 'warnings')))
$steps += , @('Test', (@('test', '--workspace', '--all-features', '--locked', '--quiet') + $network))

$results = @()
foreach ($step in $steps) {
    Write-Host "== $($step[0]) =="
    & $cargo @($step[1])
    $results += [pscustomobject]@{ Check = $step[0]; Passed = ($LASTEXITCODE -eq 0) }
}

if (-not $NoDocs) {
    Write-Host '== Docs =='
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'check_docs.ps1') | Where-Object { $_ -notmatch ': OK$' }
    $results += [pscustomobject]@{ Check = 'Docs'; Passed = ($LASTEXITCODE -eq 0) }
}

Write-Host ''
$results | Format-Table -AutoSize | Out-String | Write-Host
if ($results.Passed -contains $false) { Write-Host 'FAILED'; exit 1 }
Write-Host 'ALL CHECKS PASSED'
exit 0
