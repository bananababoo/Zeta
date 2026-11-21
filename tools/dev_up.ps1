# Brings up the local development services from compose.yaml and confirms PostGIS answers.
# Usage:  powershell -ExecutionPolicy Bypass -File tools/dev_up.ps1
# Starts Docker Desktop when its engine is down, waits for it, starts the database, and waits
# for the healthcheck. Needs network and Docker access, so agents run it outside the sandbox.
$ErrorActionPreference = 'Continue'
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo

if (-not (Test-Path (Join-Path $repo '.env'))) { Write-Host 'Missing .env. Copy .env.example to .env and fill it in.'; exit 1 }

$docker = (Get-Command docker -ErrorAction SilentlyContinue).Source
if (-not $docker) { Write-Host 'docker not found. Install Docker Desktop.'; exit 1 }

& $docker info *> $null
if ($LASTEXITCODE -ne 0) {
    # docker.exe sits in <install>\resources\bin, and Docker Desktop.exe sits in <install>.
    $desktop = Join-Path (Split-Path (Split-Path (Split-Path $docker -Parent) -Parent) -Parent) 'Docker Desktop.exe'
    if (-not (Test-Path $desktop)) { Write-Host "Docker engine is down and Docker Desktop was not found at $desktop."; exit 1 }
    Write-Host 'Starting Docker Desktop...'
    Start-Process $desktop
    $deadline = (Get-Date).AddMinutes(3)
    do { Start-Sleep 3; & $docker info *> $null } while ($LASTEXITCODE -ne 0 -and (Get-Date) -lt $deadline)
    if ($LASTEXITCODE -ne 0) {
        Write-Host 'Docker engine did not start in 3 minutes. Open Docker Desktop and check for a prompt or error.'
        exit 1
    }
}

# --wait returns only after every service reports healthy, or fails if one does not.
& $docker compose up -d --wait
if ($LASTEXITCODE -ne 0) { Write-Host 'docker compose up failed.'; exit 1 }

# SQL goes through stdin, because PowerShell mangles nested quotes in sh -c arguments.
$version = 'SELECT postgis_version();' | & $docker compose exec -T postgres sh -c 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -tA'
if ($LASTEXITCODE -ne 0 -or -not $version) { Write-Host 'Database is up but PostGIS did not answer.'; exit 1 }
Write-Host "Database ready. PostGIS $version"
exit 0
