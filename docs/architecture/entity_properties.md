# Entity Properties

| Field | Value |
|---|---|
| **Status** | `Draft` |
| **Scope** | Composable 2024 5e (System Reference Document 5.2.1) ECS property components, closed rule enums, compiled ruleset reference, AI inspect lookup, and typed mutation verbs |
| **Last Updated** | `2026-11-26` |
| **Decision Authority** | Game Director (Project Owner) |

---

## Overview
This document defines how 2024 5e (System Reference Document 5.2.1) mechanical attributes, stats, conditions, resources, and class progression are modeled as composable ECS components on any world entity. Every closed rule concept is a fixed Rust enum backed by 1 compiled ruleset reference so neither code nor the AI DM can invent invalid classes, species, conditions, or damage types.

---

## 1. Composable ECS Property Components

### What
Every mechanical property is an independent replicated Bevy component in `zeta_core::protocol::properties`, appended to `register_protocol`. Any entity—player character, monster, or breakable object—attaches only the components it needs. Every replicated type uses plain structs and unit-only enums so the binary network format (`postcard`) decodes it without tagged-enum failures.

The complete 2024 5e component set consists of 23 components across 5 groups:

- **Vitals and structure:** `Health` (`current`, `max`, `temporary`, derived `is_bloodied` at half maximum or less), `ArmorClass` (`base`, `damage_threshold`), `Size` (6 categories from `Tiny` to `Gargantuan`), `DamageAffinities` (resistances, immunities, vulnerabilities across 13 `DamageType` values; objects carry Poison and Psychic immunity), and `ConditionImmunities`.
- **Creature anatomy:** `CreatureType` (14 System Reference Document types), `AbilityScores` (6 abilities `1..=30` with derived modifiers), `Speed` (`walk`, `burrow`, `climb`, `fly`, `swim`, `hover`), `Senses` (`blindsight`, `darkvision`, `tremorsense`, `truesight`), and `Proficiencies` (saving throws, 18 skills with `Half`, `Proficient`, or `Expertise`, armor, weapons, weapon masteries, tools, and languages).
- **Character identity:** `Species` (9 System Reference Document 5.2.1 species), `Background` (4 backgrounds), `ClassLevels` (multiclass list of `ClassId` [12], level `1..=20`, optional `SubclassId` [12], hit dice pools, derived total level and proficiency bonus), `CharacterTraits` (features, feats, attuned item stacks up to 3, heroic inspiration), `Spellbook` (entries with spell identifier, source, casting ability, optional fixed Difficulty Class, preparation state, and optional use pool), `CoinPurse` (`cp`, `sp`, `ep`, `gp`, `pp`), and `CharacterBio` (traits, ideals, bonds, flaws).
- **Dynamic status:** `Conditions` (15 conditions, exhaustion `0..=6` at `-2` per level on d20 tests and `-5` feet speed per level with death at 6, and 4 hazards: `Burning`, `Dehydration`, `Malnutrition`, `Suffocation`), `DeathSaves` (`successes`, `failures`, `stabilized`), `Concentration` (1 active slot, broken automatically on `Incapacitated` or 0 hit points), `ActiveEffects` (timed or untimed buffs, debuffs, stances, and cover using flat `StatModifier` structs supporting flat bonuses, dice formulas, ability minimums, advantage or disadvantage scopes, and temporary affinities), and `ResourcePools` (level 1–9 spell slots, pact slots, closed class `ResourceId` pools, validated feature uses, and item-stack charge pools).
- **Monsters:** `MonsterProfile` (`challenge_rating_eighths` for exact fractional ratings and `initiative_bonus`).

Derived values—passive skill scores, spell save Difficulty Class, spell attack bonus, effective ability scores, and carrying weight—are computed from live components and never stored separately.

### Why
Splitting character and object mechanics into independent components lets a wooden bridge, an oil barrel, a goblin, and a fighter share 1 `Health`, `ArmorClass`, and `DamageAffinities` implementation. Fixed Rust enums stop invalid classes or conditions at deserialization time, and `ActiveEffects` models every spell buff, debuff, stance, and magic-item override without adding a new component per spell.

### Boundaries
Does not define character-sheet interface layout, automated combat action bars, or full spell-effect scripting. Equipment storage remains in `Inventory` and narrative states remain in `Tags`.

---

## 2. Single-Source Ruleset Reference & AI Inspect Lookup

### What
All closed rule definitions live in 1 embedded JSON file, `crates/zeta_core/data/srd_ruleset.json`, compiled into `zeta_core::ruleset` with zero runtime input or output (following the same pattern and Creative Commons Attribution 4.0 license as `crates/zeta_core/data/srd_catalog.json`).

The ruleset stores authoritative definitions for every property component schema, class, subclass, species, background, condition, hazard, ability, skill, damage type, size, creature type, language, feat, class resource, and core class feature. `zeta_core::inspect::ruleset_query` answers AI DM lookups by domain and identifier or search text, returning the full definition consistently on every request.

A generic exhaustiveness unit test verifies that every variant of every closed Rust enum in `zeta_core::protocol::properties` has a matching definition in `srd_ruleset.json`.

### Why
Keeping 1 compiled ruleset reference keeps the AI DM, server validation, and client display aligned on what every class, condition, or stat means. The exhaustiveness test makes it impossible to add an enum variant without documenting its rules for the AI DM.

### Boundaries
Contains only Creative Commons Attribution 4.0 System Reference Document 5.2.1 rules and Zeta's component schema documentation; no non-open book content is bundled.

---

## 3. Server-Authoritative AI Mutation Verbs

### What
The AI DM mutates typed entity properties only through strict `WorldEffect` variants (`Damage`, `Heal`, `TempHp`, `Condition`, `ApplyEffect`, `RemoveEffect`, `SpendResource`, `RestoreResource`, and `AdvanceLevel`), defined in [`ai_orchestration.md`](ai_orchestration.md). The server performs all 5e arithmetic and invariant checks:

- `Damage` checks `damage_threshold`, applies immunity (`0`), resistance (halved rounded down), or vulnerability (doubled), absorbs into `temporary` hit points first, clamps `current` at `0`, and clears `Concentration` at `0` hit points.
- `Heal` clamps `current` to `max` and clears `DeathSaves` and `Unconscious` when restoring a creature above `0`.
- `TempHp` applies the non-stacking rule (`max(existing, amount)`).
- `Condition` ignores conditions present in `ConditionImmunities`, clamps exhaustion to `0..=6`, and clears `Concentration` when `Incapacitated` is applied.
- `ApplyEffect` replaces any prior `Concentration` slot when the new effect requires concentration.
- `SpendResource` and `RestoreResource` reject underflow and clamp restoration to pool maximums.
- `AdvanceLevel` enforces total level at most `20` and verifies that any chosen `SubclassId` belongs to that `ClassId`.

### Why
Running arithmetic and rule invariants on the server prevents AI math mistakes, illegal multiclass combinations, or skipped immunities while keeping the AI DM free to adjudicate narrative causes and Difficulty Classes.

### Boundaries
Does not automate full turn-based spell targeting or reaction prompts, which belong to later combat milestones.
