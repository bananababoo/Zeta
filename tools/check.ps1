# Runs every pre-commit check from docs/architecture/code_style.md Section 1, in the same order as CI.
# Usage:  powershell -ExecutionPolicy Bypass -File tools/check.ps1 [-Fix] [-Offline] [-NoDocs] [-Jobs <N>]
#   -Fix       formats the code first (and skips the redundant format check when formatting succeeds).
#   -Offline   passes --offline to cargo, for machines or sandboxes without network.
#   -NoDocs    skips the documentation rules, for the CI job that only builds Rust.
#   -Jobs <N>  passes -j <N> to cargo to cap parallel compiler jobs when desired.
# Exits 0 only when every check passes. Success is read from exit codes, never from stderr,
# because cargo writes progress to stderr even when it succeeds.
param([switch]$Fix, [switch]$Offline, [switch]$NoDocs, [int]$Jobs = 0)

$ErrorActionPreference = 'Continue'
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo
$totalTimer = [System.Diagnostics.Stopwatch]::StartNew()

# rustup installs cargo outside PATH on some machines, so fall back to the default install folder.
$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargo) { $cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
if (-not (Test-Path $cargo)) { Write-Host 'cargo not found. Install Rust with rustup.'; exit 1 }

$cargoFlags = @()
if ($Offline) { $cargoFlags += '--offline' }
if ($Jobs -gt 0) { $cargoFlags += @('-j', "$Jobs") }

# Starts a background process that does not touch Cargo's target lock (rustfmt --check or docs)
# so it overlaps completely with Cargo compilation.
function Start-ParallelCheck {
    param([string]$Name, [string]$FilePath, [string]$Arguments, [bool]$FilterOkLines = $false)
    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $FilePath
    $psi.Arguments = $Arguments
    $psi.WorkingDirectory = $repo
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true
    $proc = [System.Diagnostics.Process]::Start($psi)
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    return [pscustomobject]@{
        Name          = $Name
        Process       = $proc
        StdoutTask    = $proc.StandardOutput.ReadToEndAsync()
        StderrTask    = $proc.StandardError.ReadToEndAsync()
        Timer         = $sw
        FilterOkLines = $FilterOkLines
    }
}

function Complete-ParallelCheck {
    param($Handle)
    $Handle.Process.WaitForExit()
    $Handle.Timer.Stop()
    $stdout = $Handle.StdoutTask.GetAwaiter().GetResult()
    $stderr = $Handle.StderrTask.GetAwaiter().GetResult()
    $passed = ($Handle.Process.ExitCode -eq 0)
    $elapsed = ($Handle.Process.ExitTime - $Handle.Process.StartTime).TotalSeconds
    $Handle.Process.Dispose()

    Write-Host "== $($Handle.Name) =="
    if ($stdout) {
        $lines = $stdout -split "`r?`n"
        if ($Handle.FilterOkLines) {
            $lines = $lines | Where-Object { $_ -and ($_ -notmatch ': OK$') }
        }
        foreach ($line in $lines) {
            if ($line -ne '') { Write-Host $line }
        }
    }
    if ($stderr) {
        foreach ($line in ($stderr -split "`r?`n")) {
            if ($line -ne '') { Write-Host $line }
        }
    }
    return [pscustomobject]@{
        Check   = $Handle.Name
        Passed  = $passed
        Seconds = [math]::Round($elapsed, 1)
    }
}

$results = @()
$bgChecks = @()

# -Fix mutates source files in place, so it runs before Cargo reads them.
if ($Fix) {
    Write-Host '== Format (fix) =='
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    & $cargo fmt --all
    $sw.Stop()
    $fixPassed = ($LASTEXITCODE -eq 0)
    $results += [pscustomobject]@{
        Check   = 'Format (fix)'
        Passed  = $fixPassed
        Seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1)
    }
    if (-not $fixPassed) {
        $bgChecks += Start-ParallelCheck -Name 'Format' -FilePath $cargo -Arguments 'fmt --all --check'
    }
} else {
    $bgChecks += Start-ParallelCheck -Name 'Format' -FilePath $cargo -Arguments 'fmt --all --check'
}

if (-not $NoDocs) {
    $docsScript = Join-Path $PSScriptRoot 'check_docs.ps1'
    $bgChecks += Start-ParallelCheck -Name 'Docs' -FilePath 'powershell' -Arguments "-NoProfile -ExecutionPolicy Bypass -File `"$docsScript`"" -FilterOkLines $true
}

$cargoSteps = @(
    ,@('Check (core)', (@('check', '-p', 'zeta_core', '--no-default-features', '--locked') + $cargoFlags))
    ,@('Lint', (@('clippy', '--workspace', '--all-targets', '--all-features', '--locked') + $cargoFlags + @('--', '-D', 'warnings')))
    ,@('Test', (@('test', '--workspace', '--all-features', '--locked', '--quiet') + $cargoFlags))
)

foreach ($step in $cargoSteps) {
    Write-Host "== $($step[0]) =="
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    & $cargo @($step[1])
    $sw.Stop()
    $results += [pscustomobject]@{
        Check   = $step[0]
        Passed  = ($LASTEXITCODE -eq 0)
        Seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1)
    }
}

foreach ($bg in $bgChecks) {
    $results += Complete-ParallelCheck -Handle $bg
}

$totalTimer.Stop()
Write-Host ''
$results | Format-Table -AutoSize | Out-String | Write-Host
Write-Host ("Total wall time: {0:N1}s" -f $totalTimer.Elapsed.TotalSeconds)
if ($results.Passed -contains $false) { Write-Host 'FAILED'; exit 1 }
Write-Host 'ALL CHECKS PASSED'
exit 0
