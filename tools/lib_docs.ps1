# Library lookup: prints the real source and docs of a third-party crate at the exact version Cargo.lock pins.
# Process and rules: docs/architecture/library_documentation.md. Works offline and in the sandbox.
#
#   tools/lib_docs.ps1                                        direct dependencies, locked versions, source folders
#   tools/lib_docs.ps1 -Crate bevy_replicon                   README, crate docs, and public modules of 1 crate
#   tools/lib_docs.ps1 -Crate 'bevy*' -Item Message           every definition named Message, with its doc comment
#   tools/lib_docs.ps1 -Crate bevy_ecs -Find 'fn add_message' regex search of the crate source
#   tools/lib_docs.ps1 -Crate bevy_replicon -Examples         example files shipped with the crate
#   tools/lib_docs.ps1 -Crate bevy_replicon -Changelog        newest changelog entries (read before an upgrade)
#
# -Crate accepts wildcards ('bevy*' covers the bevy facade and every bevy_* crate it re-exports).
param([string]$Crate, [string]$Item, [string]$Find, [switch]$Examples, [switch]$Changelog, [int]$Max = 40)

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent

# Locked registry packages from Cargo.lock.
$packages = @()
foreach ($block in ((Get-Content (Join-Path $repo 'Cargo.lock') -Raw) -split '\[\[package\]\]')) {
    if ($block -notmatch '(?m)^name = "([^"]+)"') { continue }
    $name = $Matches[1]
    if ($block -notmatch '(?m)^version = "([^"]+)"') { continue }
    $version = $Matches[1]
    if ($block -notmatch '(?m)^source = "registry\+') { continue }
    $packages += [pscustomobject]@{ Name = $name; Version = $version }
}

# Version requirements of direct dependencies from [workspace.dependencies].
$direct = [ordered]@{}
$inDeps = $false
foreach ($line in (Get-Content (Join-Path $repo 'Cargo.toml'))) {
    if ($line -match '^\[') { $inDeps = ($line -eq '[workspace.dependencies]'); continue }
    if (-not $inDeps) { continue }
    if ($line -match '^([A-Za-z0-9_\-]+)\s*=\s*"([^"]+)"') { $direct[$Matches[1]] = $Matches[2] }
    elseif ($line -match '^([A-Za-z0-9_\-]+)\s*=\s*\{.*version\s*=\s*"([^"]+)"') { $direct[$Matches[1]] = $Matches[2] }
}

$cargoHome = $env:CARGO_HOME
if (-not $cargoHome) { $cargoHome = Join-Path $HOME '.cargo' }
$sourceRoots = @(Get-ChildItem (Join-Path $cargoHome 'registry\src') -Directory -ErrorAction SilentlyContinue)

function Get-SourceDir($package) {
    foreach ($root in $sourceRoots) {
        $dir = Join-Path $root.FullName "$($package.Name)-$($package.Version)"
        if (Test-Path $dir) { return $dir }
    }
    return $null
}

function Test-Requirement($version, $requirement) {
    return ($version -eq $requirement) -or $version.StartsWith("$requirement.")
}

# No crate: list direct dependencies.
if (-not $Crate) {
    'Direct dependencies (workspace Cargo.toml) at their locked versions:'
    foreach ($name in $direct.Keys) {
        $match = $packages | Where-Object { $_.Name -eq $name -and (Test-Requirement $_.Version $direct[$name]) } | Select-Object -First 1
        if (-not $match) { continue }
        $dir = Get-SourceDir $match
        if (-not $dir) { $dir = 'source missing: run a bypassed cargo fetch' }
        '{0,-16} {1,-10} {2}' -f $match.Name, $match.Version, $dir
    }
    ''
    'Next: tools/lib_docs.ps1 -Crate <name> [-Item <Name> | -Find <regex> | -Examples | -Changelog]'
    exit 0
}

# Matching packages; a direct dependency keeps only the version its requirement selects.
$selected = @($packages | Where-Object { $_.Name -like $Crate } | Where-Object {
    (-not $direct.Contains($_.Name)) -or (Test-Requirement $_.Version $direct[$_.Name])
} | Sort-Object Name, Version)
if ($selected.Count -eq 0) { "No locked registry crate matches '$Crate'. Run without -Crate to list direct dependencies."; exit 1 }

$dirs = @()
foreach ($package in $selected) {
    $dir = Get-SourceDir $package
    if ($dir) { $dirs += [pscustomobject]@{ Package = $package; Dir = $dir } }
    else { "warning: $($package.Name) $($package.Version) is locked but its source is not downloaded (bypassed cargo fetch)." }
}
$duplicates = $selected | Group-Object Name | Where-Object { $_.Count -gt 1 }
foreach ($group in $duplicates) {
    "warning: $($group.Name) is locked at several versions ($(($group.Group.Version) -join ', ')); check which one the calling crate uses."
}

function Get-Label($entry, $path) {
    return "$($entry.Package.Name)-$($entry.Package.Version)" + $path.Substring($entry.Dir.Length).Replace('\', '/')
}

if ($Item) {
    $pattern = '^\s*(pub(\([^)]*\))?\s+)?((unsafe|async|const|extern "C")\s+)*(struct|enum|trait|fn|type|union|mod|static|const)\s+' + [regex]::Escape($Item) + '\b|macro_rules!\s+' + [regex]::Escape($Item) + '\b'
    # Definitions are case-sensitive. Library source (src/) comes first, then examples and tests.
    $found = @()
    foreach ($entry in $dirs) {
        foreach ($hit in (Get-ChildItem $entry.Dir -Recurse -File -Filter *.rs | Select-String -Pattern $pattern -CaseSensitive)) {
            $rank = 1
            if ($hit.Path.Substring($entry.Dir.Length) -like '\src\*') { $rank = 0 }
            $found += [pscustomobject]@{ Entry = $entry; Hit = $hit; Rank = $rank; Order = $found.Count }
        }
    }
    $count = 0
    foreach ($result in ($found | Sort-Object Rank, Order)) {
        if ($count -ge $Max) { break }
        $count++
        $entry = $result.Entry
        $hit = $result.Hit
        $lines = @(Get-Content $hit.Path -Encoding UTF8)
        $start = $hit.LineNumber - 1
        while ($start -gt 0 -and $lines[$start - 1] -match '^\s*(///|#\[|#!\[)') { $start-- }
        $end = $hit.LineNumber - 1
        while ($end -lt $lines.Count - 1 -and $end -lt $hit.LineNumber + 8 -and $lines[$end] -notmatch '[{;}]\s*$') { $end++ }
        "== $(Get-Label $entry $hit.Path):$($hit.LineNumber)"
        $lines[$start..$end]
        ''
    }
    if ($count -eq 0) { "No definition named '$Item' in $(($dirs | ForEach-Object { $_.Package.Name }) -join ', '). Try -Find, or a wider -Crate pattern; the item may be re-exported from another crate." }
    elseif ($found.Count -gt $Max) { "(showing $Max of $($found.Count) results; narrow -Crate or raise -Max)" }
    exit 0
}

if ($Find) {
    $count = 0
    foreach ($entry in $dirs) {
        foreach ($hit in (Get-ChildItem $entry.Dir -Recurse -File -Include *.rs, *.md | Select-String -Pattern $Find)) {
            if ($count -ge $Max) { break }
            $count++
            "$(Get-Label $entry $hit.Path):$($hit.LineNumber): $($hit.Line.Trim())"
        }
    }
    if ($count -eq 0) { "No match for '$Find'." } elseif ($count -ge $Max) { "(stopped at $Max results; narrow the pattern or raise -Max)" }
    exit 0
}

foreach ($entry in $dirs) {
    "== $($entry.Package.Name) $($entry.Package.Version)"
    "source: $($entry.Dir)"
    if ($Examples) {
        $exampleDir = Join-Path $entry.Dir 'examples'
        if (Test-Path $exampleDir) { Get-ChildItem $exampleDir -Recurse -File | ForEach-Object { Get-Label $entry $_.FullName } }
        else { 'no examples folder in the published crate' }
        ''
        continue
    }
    if ($Changelog) {
        $log = Get-ChildItem $entry.Dir -File | Where-Object { $_.Name -match '^CHANGELOG' } | Select-Object -First 1
        if ($log) { Get-Content $log.FullName -Encoding UTF8 -TotalCount 120 } else { 'no changelog in the published crate' }
        ''
        continue
    }
    if ($dirs.Count -gt 1) { continue }
    $readme = Get-ChildItem $entry.Dir -File | Where-Object { $_.Name -match '^README' } | Select-Object -First 1
    if ($readme) { '-- README (first 60 lines)'; Get-Content $readme.FullName -Encoding UTF8 -TotalCount 60 }
    $lib = Join-Path $entry.Dir 'src\lib.rs'
    if (Test-Path $lib) {
        $libLines = @(Get-Content $lib -Encoding UTF8)
        '-- crate docs (src/lib.rs //!)'
        $libLines | Where-Object { $_ -match '^//!' } | Select-Object -First 80
        '-- public modules and re-exports (src/lib.rs)'
        $libLines | Where-Object { $_ -match '^\s*pub (mod|use) ' } | Select-Object -First 60
    }
    ''
}
if ($dirs.Count -gt 1 -and -not $Examples -and -not $Changelog) { "$($dirs.Count) crates match; add -Item or -Find to search them, or name 1 crate to read its docs." }
