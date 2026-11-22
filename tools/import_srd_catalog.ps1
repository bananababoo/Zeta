# Imports the SRD 5.2.1 item catalog from 5e.tools data into crates/zeta_core/data/srd_catalog.json.
# Rules: docs/design/game_design.md Section 5. Rust side: crates/zeta_core/src/catalog.rs.
#
#   tools/import_srd_catalog.ps1 -BaseItems <items-base.json> -Items <items.json> [-Output <path>]
#
# Legal rule: only entries flagged "srd52": true are kept (CC-BY-4.0, see crates/zeta_core/data/ATTRIBUTION.md).
# "basicRules2024" alone is not enough. The raw 5e.tools files hold book content and are never committed.
# Only game data is kept; prose and 5e.tools {@tag ...} markup are dropped or converted.
# Output is deterministic: items sorted by id (ordinal), UTF-8 without BOM, LF line endings. Rerunnable.
#
# Scope (Milestone 2): mundane weapons, armor, ammunition, tools, spellcasting focuses, gear, packs, food,
# tack, and the srd52 item groups (Holy Symbol, Arcane Focus, ...). Skipped for now: magic items (rarity other
# than none/unknown), mounts, vehicles, ships, and services. Adding them later is a change to this script only;
# craft references to skipped items (Spell Scroll, Potion of Healing) are dropped with a warning and resolve on
# their own once those items are imported.
param(
    [Parameter(Mandatory = $true)][string]$BaseItems,
    [Parameter(Mandatory = $true)][string]$Items,
    [string]$Output
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
if (-not $Output) { $Output = Join-Path $repo 'crates/zeta_core/data/srd_catalog.json' }

# 5e.tools type code -> catalog category. Codes missing here and from $skippedTypes stop the import.
$categoryByType = @{
    'M' = 'weapon'; 'R' = 'weapon'
    'LA' = 'armor'; 'MA' = 'armor'; 'HA' = 'armor'; 'S' = 'armor'
    'A' = 'ammunition'
    'AT' = 'tool'; 'T' = 'tool'; 'GS' = 'tool'; 'INS' = 'tool'
    'SCF' = 'spellcasting_focus'
    'G' = 'adventuring_gear'
    'FD' = 'food_and_drink'
    'TAH' = 'tack_and_harness'
}
$skippedTypes = @('MNT', 'VEH', 'SHP', 'AIR')
$skippedNames = @('Stabling (per day)')
$toolKindByType = @{ 'AT' = 'artisans'; 'T' = 'other'; 'GS' = 'gaming_set'; 'INS' = 'musical_instrument' }
$armorCategoryByType = @{ 'LA' = 'light'; 'MA' = 'medium'; 'HA' = 'heavy'; 'S' = 'shield' }
$damageTypeByCode = @{ 'B' = 'bludgeoning'; 'P' = 'piercing'; 'S' = 'slashing' }
$propertyByCode = @{
    'A' = 'ammunition'; 'F' = 'finesse'; 'H' = 'heavy'; 'L' = 'light'; 'LD' = 'loading'
    'R' = 'reach'; 'T' = 'thrown'; '2H' = 'two_handed'; 'V' = 'versatile'
}
$masteries = @('cleave', 'graze', 'nick', 'push', 'sap', 'slow', 'topple', 'vex')
$abilities = @('strength', 'dexterity', 'constitution', 'intelligence', 'wisdom', 'charisma')
$skills = @(
    'acrobatics', 'animal_handling', 'arcana', 'athletics', 'deception', 'history', 'insight', 'intimidation',
    'investigation', 'medicine', 'nature', 'perception', 'performance', 'persuasion', 'religion',
    'sleight_of_hand', 'stealth', 'survival'
)
# Craft list phrases that name a whole kind of item, with the type codes they cover.
$craftKinds = [ordered]@{
    'Any Melee weapon' = @('M'); 'Ranged weapons' = @('R')
    'Light armor' = @('LA'); 'Medium armor' = @('MA'); 'Heavy armor' = @('HA')
}

$warnings = New-Object System.Collections.Generic.List[string]

function Get-TypeCode($entry) {
    if (-not $entry.type) { return $null }
    return ($entry.type -split '\|')[0]
}

function ConvertTo-CatalogId([string]$name) {
    $id = ($name.ToLowerInvariant() -replace "'", '') -replace '[^a-z0-9]+', '_'
    $id = $id.Trim('_')
    if ($id -notmatch '^[a-z0-9_]{1,64}$') { throw "Name '$name' gives the invalid id '$id'." }
    return $id
}

# Converts 5e.tools markup to plain text: {@dc 10} -> "DC 10", {@tag Name|Source|Shown} -> "Shown" or "Name".
function Remove-Markup([string]$text) {
    $plain = [regex]::Replace($text, '\{@dc (\d+)\}', 'DC $1')
    $plain = [regex]::Replace($plain, '\{@\w+ ([^{}]*)\}', [System.Text.RegularExpressions.MatchEvaluator] {
            param($match)
            $parts = $match.Groups[1].Value -split '\|'
            if ($parts.Count -ge 3 -and $parts[2]) { return $parts[2] }
            return $parts[0]
        })
    if ($plain -match '\{@') { throw "Unconverted markup in: $text" }
    return $plain.Trim()
}

function Read-Entries([string]$path, [string]$key) {
    $data = Get-Content $path -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($null -eq $data.$key) { throw "$path has no '$key' list." }
    return @($data.$key)
}

$rawBase = Read-Entries $BaseItems 'baseitem'
$rawItemFile = Get-Content $Items -Raw -Encoding UTF8 | ConvertFrom-Json
$rawItems = @($rawItemFile.item)
$rawGroups = @($rawItemFile.itemGroup)

# 1. Select entries: srd52 only, mundane only, in-scope types only.
$selected = New-Object System.Collections.Generic.List[object]
foreach ($entry in @($rawBase) + @($rawItems)) {
    if ($entry.srd52 -ne $true) { continue }
    if ($entry.rarity -and $entry.rarity -notin @('none', 'unknown')) { continue }
    if ($entry.name -in $skippedNames) { continue }
    $code = Get-TypeCode $entry
    if ($null -eq $code) { throw "SRD entry '$($entry.name)' has no type; decide its category in this script." }
    if ($code -in $skippedTypes) { continue }
    if (-not $categoryByType.ContainsKey($code)) { throw "SRD entry '$($entry.name)' has unknown type code '$code'." }
    $selected.Add($entry)
}

# 2. Assign ids and build the name index used to resolve references.
$idByName = @{}
$typeById = @{}
foreach ($entry in $selected) {
    $id = ConvertTo-CatalogId $entry.name
    $nameKey = $entry.name.ToLowerInvariant()
    if ($idByName.ContainsKey($nameKey) -or $typeById.ContainsKey($id)) { throw "Duplicate catalog entry '$($entry.name)' ($id)." }
    $idByName[$nameKey] = $id
    $typeById[$id] = Get-TypeCode $entry
}
$groups = New-Object System.Collections.Generic.List[object]
foreach ($group in $rawGroups) {
    if ($group.srd52 -ne $true) { continue }
    if ($group.rarity -and $group.rarity -notin @('none', 'unknown')) { continue }
    $members = New-Object 'System.Collections.Generic.SortedSet[string]' ([StringComparer]::Ordinal)
    $complete = $true
    foreach ($member in @($group.items)) {
        $memberKey = ($member -split '\|')[0].ToLowerInvariant()
        if ($idByName.ContainsKey($memberKey)) { [void]$members.Add($idByName[$memberKey]) } else { $complete = $false }
    }
    if (-not $complete -or $members.Count -eq 0) { $warnings.Add("Skipped item group '$($group.name)': not every member is in the catalog."); continue }
    $id = ConvertTo-CatalogId $group.name
    if ($typeById.ContainsKey($id)) { throw "Duplicate catalog entry '$($group.name)' ($id)." }
    $idByName[$group.name.ToLowerInvariant()] = $id
    $typeById[$id] = 'group'
    $groups.Add([pscustomobject]@{ Id = $id; Name = $group.name; Members = $members })
}

# Resolves "Name|Source|Shown" to a catalog id, or $null when the entry is not in the catalog.
function Resolve-Reference([string]$reference, [string]$context) {
    $parts = $reference -split '\|'
    $source = if ($parts.Count -ge 2) { $parts[1].ToUpperInvariant() } else { 'XPHB' }
    $nameKey = $parts[0].ToLowerInvariant()
    if ($source -eq 'XPHB' -and $idByName.ContainsKey($nameKey)) { return $idByName[$nameKey] }
    $warnings.Add("${context}: dropped reference '$reference' (not in the catalog).")
    return $null
}

function New-List { return , (New-Object System.Collections.Generic.List[object]) }

function Get-ListItem($entry, [string]$label) {
    foreach ($block in @($entry.entries)) {
        if ($block.type -ne 'list') { continue }
        foreach ($listItem in @($block.items)) {
            if ($listItem.name -eq $label) { return [string](@($listItem.entries)[0]) }
        }
    }
    return $null
}

# Every prose string of an entry, including nested "entries" blocks.
function Get-ProseStrings($node) {
    $found = New-List
    foreach ($child in @($node)) {
        if ($null -eq $child) { continue }
        if ($child -is [string]) { $found.Add($child); continue }
        if ($child.type -eq 'entries' -or $child.type -eq 'list') {
            foreach ($text in (Get-ProseStrings $child.entries)) { $found.Add($text) }
        }
    }
    return , $found
}

function ConvertTo-CardLine([string]$text) {
    if (-not $text) { return $text }
    return $text.Substring(0, 1).ToUpperInvariant() + $text.Substring(1)
}

# Tool line "Pick a lock ({@dc 15}), or disarm a trap ({@dc 15})" -> 1 check per DC.
function ConvertFrom-ToolUtilize([string]$text, [string]$context) {
    $checks = New-List
    $found = [regex]::Matches($text, '(?:^|,\s*(?:or\s+)?)(.+?)\s*\(\{@dc (\d+)\}\)')
    $covered = 0
    foreach ($match in $found) {
        $covered += $match.Length
        $description = ConvertTo-CardLine (Remove-Markup $match.Groups[1].Value)
        $checks.Add([ordered]@{ description = $description; dc = [long]$match.Groups[2].Value })
    }
    if ($checks.Count -eq 0 -or $covered -ne $text.Length) { throw "${context}: cannot read Utilize line '$text'." }
    return , $checks
}

# Gear prose: a sentence that takes the Utilize action and asks the user for a DC check becomes a check.
# The description is the short action phrase only: "As a Utilize action, you can <action> if you succeed on
# a DC N ... check" keeps <action>, cut before ", and" or " that has" and without " of yourself".
function ConvertFrom-GearUtilize($entry) {
    $checks = New-List
    foreach ($text in (Get-ProseStrings $entry.entries)) {
        foreach ($sentence in ($text -split '(?<=\.)\s+')) {
            if ($sentence -notmatch '\{@action Utilize') { continue }
            $check = [regex]::Match($sentence, '\{@dc (\d+)\}[^.]*\bcheck\b')
            if (-not $check.Success) { continue }
            $action = [regex]::Match((Remove-Markup $sentence), 'you can (.+?) if you succeed')
            if (-not $action.Success) { throw "$($entry.name): cannot read the Utilize action in '$sentence'." }
            $phrase = ($action.Groups[1].Value -split ', and | that has ')[0] -replace ' of yourself', ''
            $utilizeCheck = [ordered]@{ description = (ConvertTo-CardLine $phrase.Trim()); dc = [long]$check.Groups[1].Value }
            $kind = [regex]::Match($sentence, '\{@dc \d+\} (\w+) (?:\(\{@skill ([^|}]+)[^}]*\}\) )?check')
            if ($kind.Success) {
                $ability = $kind.Groups[1].Value.ToLowerInvariant()
                if ($ability -notin $abilities) { throw "$($entry.name): unknown check ability '$ability'." }
                $utilizeCheck['ability'] = $ability
                if ($kind.Groups[2].Success) {
                    $skill = ($kind.Groups[2].Value.ToLowerInvariant() -replace '[^a-z]+', '_')
                    if ($skill -notin $skills) { throw "$($entry.name): unknown check skill '$skill'." }
                    $utilizeCheck['skill'] = $skill
                }
            }
            $checks.Add($utilizeCheck)
        }
    }
    return , $checks
}

function Split-TopLevel([string]$text) {
    $parts = New-List
    $depth = 0
    $current = New-Object System.Text.StringBuilder
    foreach ($character in $text.ToCharArray()) {
        if ($character -eq '(') { $depth++ } elseif ($character -eq ')') { $depth-- }
        if ($character -eq ',' -and $depth -eq 0) {
            $parts.Add($current.ToString().Trim()); [void]$current.Clear()
        } else { [void]$current.Append($character) }
    }
    $parts.Add($current.ToString().Trim())
    return , $parts
}

function ConvertFrom-CraftList([string]$text, [string]$context) {
    $crafts = New-Object 'System.Collections.Generic.SortedSet[string]' ([StringComparer]::Ordinal)
    foreach ($segment in (Split-TopLevel $text)) {
        $segment = $segment -replace '^and\s+', ''
        if ($segment -match '^\{@item ([^{}]*)\}$') {
            $id = Resolve-Reference $Matches[1] $context
            if ($id) { [void]$crafts.Add($id) }
            continue
        }
        $kind = $null
        foreach ($phrase in $craftKinds.Keys) { if ($segment.StartsWith($phrase)) { $kind = $phrase } }
        if ($null -eq $kind) { throw "${context}: cannot read Craft segment '$segment'." }
        $excluded = @{}
        foreach ($match in [regex]::Matches($segment, '\{@item ([^{}]*)\}')) {
            $id = Resolve-Reference $match.Groups[1].Value $context
            if ($id) { $excluded[$id] = $true }
        }
        foreach ($id in $typeById.Keys) {
            if ($typeById[$id] -in $craftKinds[$kind] -and -not $excluded.ContainsKey($id)) { [void]$crafts.Add($id) }
        }
    }
    $list = New-List
    foreach ($id in $crafts) { $list.Add($id) }
    return , $list
}

function ConvertTo-Cost($value, [string]$context) {
    if ($null -eq $value) { return $null }
    $copper = [decimal]$value
    foreach ($quantity in 1..100) {
        $total = $copper * $quantity
        if ($total -eq [decimal]::Truncate($total)) {
            $cost = [ordered]@{ copper_pieces = [long]$total }
            if ($quantity -ne 1) { $cost['per_quantity'] = [long]$quantity }
            return $cost
        }
    }
    throw "${context}: cost $value copper is not a whole number for any quantity up to 100."
}

function ConvertTo-Dice([string]$text, [string]$context) {
    # A flat amount N (the Blowgun deals 1) is written as N dice of 1 side, which always roll N.
    if ($text -match '^(\d+)$') { return [ordered]@{ count = [long]$Matches[1]; sides = [long]1 } }
    if ($text -notmatch '^(\d+)d(\d+)$') { throw "${context}: cannot read damage dice '$text'." }
    return [ordered]@{ count = [long]$Matches[1]; sides = [long]$Matches[2] }
}

function ConvertTo-Weapon($entry, [string]$code, [string]$context) {
    $properties = New-List
    foreach ($property in @($entry.property)) {
        if ($null -eq $property) { continue }
        $uid = if ($property -is [string]) { $property } else { $property.uid }
        $propertyCode = ($uid -split '\|')[0]
        if (-not $propertyByCode.ContainsKey($propertyCode)) { throw "${context}: unknown weapon property '$uid'." }
        if (-not $properties.Contains($propertyByCode[$propertyCode])) { $properties.Add($propertyByCode[$propertyCode]) }
    }
    $mastery = (@($entry.mastery)[0] -split '\|')[0].ToLowerInvariant()
    if ($mastery -notin $masteries) { throw "${context}: unknown weapon mastery '$mastery'." }
    if (-not $damageTypeByCode.ContainsKey([string]$entry.dmgType)) { throw "${context}: unknown damage type '$($entry.dmgType)'." }
    $weapon = [ordered]@{
        category = [string]$entry.weaponCategory
        kind = $(if ($code -eq 'M') { 'melee' } else { 'ranged' })
        damage_dice = (ConvertTo-Dice $entry.dmg1 $context)
        damage_type = $damageTypeByCode[[string]$entry.dmgType]
    }
    if ($weapon.category -notin @('simple', 'martial')) { throw "${context}: unknown weapon category '$($weapon.category)'." }
    if ($entry.dmg2) { $weapon['versatile_dice'] = ConvertTo-Dice $entry.dmg2 $context }
    if ($properties.Count -gt 0) { $weapon['properties'] = $properties }
    if ($entry.range) {
        $range = [regex]::Match([string]$entry.range, '^(\d+)/(\d+)$')
        if (-not $range.Success) { throw "${context}: cannot read range '$($entry.range)'." }
        $weapon['range'] = [ordered]@{ normal_feet = [long]$range.Groups[1].Value; long_feet = [long]$range.Groups[2].Value }
    }
    $weapon['mastery'] = $mastery
    if ($entry.ammoType) {
        $ammunition = Resolve-Reference $entry.ammoType $context
        if (-not $ammunition) { throw "${context}: ammunition '$($entry.ammoType)' is not in the catalog." }
        $weapon['ammunition'] = $ammunition
    }
    return $weapon
}

# 3. Build catalog items.
$catalog = New-Object 'System.Collections.Generic.SortedDictionary[string,object]' ([StringComparer]::Ordinal)
foreach ($entry in $selected) {
    $id = $idByName[$entry.name.ToLowerInvariant()]
    $code = Get-TypeCode $entry
    $context = $entry.name
    $item = [ordered]@{ id = $id; name = [string]$entry.name; category = $categoryByType[$code] }
    $cost = ConvertTo-Cost $entry.value $context
    if ($cost) { $item['cost'] = $cost }
    $weight = if ($null -eq $entry.weight) { 0 } else { [long][math]::Round([decimal]$entry.weight * 1000) }
    $item['weight_thousandths_of_pound'] = $weight
    # 5e.tools tags items used up when used (rations, oil, poisons) with miscTags "CNS".
    if (@($entry.miscTags) -contains 'CNS') { $item['consumable'] = $true }
    if ($code -in @('M', 'R')) { $item['weapon'] = ConvertTo-Weapon $entry $code $context }
    if ($armorCategoryByType.ContainsKey($code)) {
        $armor = [ordered]@{ category = $armorCategoryByType[$code]; armor_class = [long]$entry.ac }
        if ($entry.strength) { $armor['strength_requirement'] = [long]$entry.strength }
        $armor['stealth_disadvantage'] = ($entry.stealth -eq $true)
        $item['armor'] = $armor
    }
    $utilize = New-List
    $crafts = New-List
    if ($toolKindByType.ContainsKey($code)) {
        $ability = [string](Get-ListItem $entry 'Ability:')
        $ability = $ability.ToLowerInvariant()
        if ($ability -notin $abilities) { throw "${context}: tool ability '$ability' is not an ability." }
        $item['tool'] = [ordered]@{ kind = $toolKindByType[$code]; ability = $ability }
        $utilizeLine = Get-ListItem $entry 'Utilize:'
        if (-not $utilizeLine) { throw "${context}: tool has no Utilize line." }
        $utilize = ConvertFrom-ToolUtilize $utilizeLine $context
        $craftLine = Get-ListItem $entry 'Craft:'
        if ($craftLine) { $crafts = ConvertFrom-CraftList $craftLine $context }
    } else {
        $utilize = ConvertFrom-GearUtilize $entry
    }
    if ($utilize.Count -gt 0) { $item['utilize'] = $utilize }
    if ($crafts.Count -gt 0) { $item['crafts'] = $crafts }
    if ($entry.packContents) {
        $contents = New-List
        foreach ($content in @($entry.packContents)) {
            $reference = if ($content -is [string]) { $content } else { [string]$content.item }
            if (-not $reference) { throw "${context}: cannot read pack entry '$($content | ConvertTo-Json -Compress)'." }
            $quantity = if ($content -is [string] -or $null -eq $content.quantity) { 1 } else { [long]$content.quantity }
            $contentId = Resolve-Reference $reference $context
            if (-not $contentId) { throw "${context}: pack entry '$reference' is not in the catalog." }
            $contents.Add([ordered]@{ item = $contentId; quantity = [long]$quantity })
        }
        $item['contents'] = $contents
    }
    $catalog.Add($id, $item)
}
foreach ($group in $groups) {
    $members = New-List
    foreach ($member in $group.Members) { $members.Add($member) }
    $catalog.Add($group.Id, [ordered]@{
            id = $group.Id; name = [string]$group.Name; category = 'item_group'
            weight_thousandths_of_pound = [long]0; group_members = $members
        })
}

# 4. Write JSON by hand so output is byte-stable across PowerShell versions.
function ConvertTo-JsonString([string]$text) {
    if ($text -match '[\x00-\x1f]') { throw "Control character in text: $text" }
    return '"' + $text.Replace('\', '\\').Replace('"', '\"') + '"'
}

function ConvertTo-JsonText($value, [int]$depth) {
    $outer = '  ' * $depth
    $inner = '  ' * ($depth + 1)
    if ($value -is [string]) { return ConvertTo-JsonString $value }
    if ($value -is [bool]) { if ($value) { return 'true' } else { return 'false' } }
    if ($value -is [long] -or $value -is [int]) { return $value.ToString([Globalization.CultureInfo]::InvariantCulture) }
    if ($value -is [System.Collections.IDictionary]) {
        $lines = foreach ($key in $value.Keys) { $inner + (ConvertTo-JsonString $key) + ': ' + (ConvertTo-JsonText $value[$key] ($depth + 1)) }
        return "{`n" + ($lines -join ",`n") + "`n$outer}"
    }
    if ($value -is [System.Collections.IList]) {
        if ($value.Count -eq 0) { return '[]' }
        $lines = foreach ($element in $value) { $inner + (ConvertTo-JsonText $element ($depth + 1)) }
        return "[`n" + ($lines -join ",`n") + "`n$outer]"
    }
    throw "Cannot write value of type $($value.GetType().FullName) as JSON."
}

$itemList = New-List
foreach ($item in $catalog.Values) { $itemList.Add($item) }
$document = [ordered]@{ items = $itemList }
$json = (ConvertTo-JsonText $document 0) + "`n"
$outputDirectory = Split-Path $Output -Parent
if (-not (Test-Path $outputDirectory)) { New-Item -ItemType Directory -Path $outputDirectory | Out-Null }
[IO.File]::WriteAllText($Output, $json, [Text.UTF8Encoding]::new($false))

foreach ($warning in $warnings) { Write-Warning $warning }
Write-Output "Wrote $($catalog.Count) catalog items to $Output"
