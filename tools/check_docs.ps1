# Documentation compliance checker for docs/ (see docs/documentation_specification.md).
# Manual use:  powershell -ExecutionPolicy Bypass -File tools/check_docs.ps1
# Hook use:    powershell -ExecutionPolicy Bypass -File tools/check_docs.ps1 -Hook
#   Reads the Antigravity Stop hook payload on stdin and writes the hook decision JSON on stdout.
param([switch]$Hook)

$root = Join-Path (Split-Path $PSScriptRoot -Parent) "docs"
$repo = Split-Path $root -Parent
$banned = 'e\.g\.', 'i\.e\.', 'etc\.', '\bPC\b', '\bID\b', '\bUI\b', '\bspec\b', 'combat bubble', 'turn bubble', 'snapshots',
    '(?i)\bseamless', '(?i)\borganic', '(?i)\bgenuine', '(?i)\brobust', '(?i)\bleverag', '(?i)\bensur', '(?i)\bguarantee',
    '(?i)\bcrucial', '(?i)\bdelve', '(?i)rather than', '(?i)single source of truth'
# Folders the specification reserves before they exist (Section 4, Paths).
$reserved = 'lore', 'lore/', 'crates', 'crates/', 'n/a'

$report = @()
$failed = $false
Get-ChildItem $root -Recurse -File -Filter *.md | ForEach-Object {
    $f = $_; $lines = @(Get-Content $f.FullName -Encoding UTF8); $issues = @()
    $words = ($f.BaseName -split '_') | ForEach-Object { if ($_ -eq 'ai') { 'AI' } else { $_.Substring(0,1).ToUpper() + $_.Substring(1) } }
    $expected = "# " + ($words -join ' ')
    if ($lines[0] -ne $expected) { $issues += "title '$($lines[0])' vs '$expected'" }
    if ($lines[-1] -eq '---') { $issues += "ends with ---" }
    $inCode = $false
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $l = $lines[$i]
        if ($l -match '^```') { $inCode = -not $inCode; continue }
        if ($inCode) { continue }
        if ($l -match '^## ' -and ($i -lt 2 -or $lines[$i-2] -ne '---')) { $issues += "line $($i+1): ## without --- above" }
        if ($l -eq '---' -and -not ($i + 2 -lt $lines.Count -and $lines[$i+2] -match '^## ')) { $issues += "line $($i+1): stray ---" }
        $prose = $l -replace '`[^`]*`', ''
        foreach ($bad in $banned) {
            if ($prose -cmatch $bad) { $issues += "line $($i+1): '$bad'" }
        }
        foreach ($m in [regex]::Matches($l, '\]\(([^)#]+)')) {
            $p = Join-Path $f.DirectoryName $m.Groups[1].Value
            if (-not (Test-Path $p)) { $issues += "line $($i+1): broken link $($m.Groups[1].Value)" }
        }
        foreach ($m in [regex]::Matches($l, '`([\w.\-]+(/[\w.\-]*)+)`')) {
            $t = $m.Groups[1].Value
            if ($reserved -contains $t) { continue }
            $found = (Test-Path (Join-Path $f.DirectoryName $t)) -or (Test-Path (Join-Path $repo $t)) -or (Test-Path (Join-Path $root $t))
            if (-not $found) { $issues += "line $($i+1): path does not exist $t" }
        }
    }
    $rel = $f.FullName.Substring($root.Length + 1)
    if ($issues) { $report += "${rel}: " + ($issues -join '; '); $failed = $true }
    elseif (-not $Hook) { $report += "${rel}: OK" }
}

# Manual use exits 1 on any issue, so automated checks fail.
if (-not $Hook) { $report; if ($failed) { exit 1 } else { exit 0 } }

$raw = ''
try { $raw = [Console]::In.ReadToEnd() } catch {}
try { Set-Content -Path (Join-Path $env:TEMP 'zeta_stop_hook_payload.json') -Value $raw -Encoding UTF8 } catch {}
# Matched as text: Windows PowerShell's ConvertFrom-Json rejects some payloads, which left the loop guard always off.
$alreadyContinued = ($raw -match '"continuationActive"\s*:\s*true') -or ($raw -match '"executionNum"\s*:\s*[1-9]')
if ($report.Count -gt 0 -and -not $alreadyContinued) {
    @{ decision = "continue"; reason = "Documentation compliance check failed. Fix these issues in docs/ before stopping:`n" + ($report -join "`n") } | ConvertTo-Json -Compress
} else {
    '{}'
}
