# Glossary

| Field | Value |
|---|---|
| **Status** | `Locked` |
| **Scope** | Approved abbreviations and Defined Terms used across `docs/` |
| **Last Updated** | `2026-11-16` |
| **Decision Authority** | Game Director (Project Owner) |

---

## Overview
This file is the only list of approved abbreviations and Defined Terms in the repository. Any abbreviation not listed here is prohibited in prose, headings, and filenames and must be written out in full. Every Defined Term listed here must be written exactly as shown, and its rules live only in its owning document.

---

## 1. Abbreviation Policy

### What
Only abbreviations registered in the Approved Abbreviations table may be used. Prose and headings use the Display Form, and filenames use the Filename Form. Exemptions are defined in [`documentation_specification.md`](documentation_specification.md) Section 5, and adding a new abbreviation requires Game Director approval.

### Why
A reader never has to guess what a shortened word means, and agents never mix 2 forms of the same word, such as `spec` and `specification`.

### Boundaries
Does not govern third-party library names or standard Rust crate identifiers such as `serde` or `tokio`.

---

## 2. Defined Term Policy

### What
A Defined Term is a game or project concept with 1 precise meaning. Each Defined Term is written in Title Case exactly as listed, and each has exactly 1 owning document where its rules are defined. This glossary gives only a 1-sentence definition and a link, and the owning document is authoritative. New Defined Terms are added in the same edit that introduces them.

### Why
Registered names stop agents from inventing a second name for a concept that already has one.

### Boundaries
This glossary is not a rules source. Official 5e rule terms that are not listed keep their 5e meaning and capitalization.

---

## Schemas

### Approved Abbreviations

| Display Form | Filename Form | Full Meaning | Usage Context |
|---|---|---|---|
| **2D** | `2d` | Two-Dimensional | Top-down map, grid, and sprite art |
| **5e** | `5e` | Dungeons & Dragons 5th Edition | Target full-game ruleset (skipped in initial prototype) |
| **AI** | `ai` | Artificial Intelligence | Orchestrator, Dungeon Master reasoning, asset generation |
| **D&D** | `dnd` | Dungeons & Dragons | The tabletop game the design is inspired by |
| **DM** | `dm` | Dungeon Master | The scene-level AI referee and conflict storyteller |
| **ECS** | `ecs` | Entity-Component-System | Bevy world-state and database component architecture |
| **JSON** | `json` | JavaScript Object Notation | AI output format and data payloads |
| **NPC** | `npc` | Non-Player Character | AI-controlled characters or Infinite Downtime shopkeepers |
| **PvP** | `pvp` | Player versus Player | Combat between players during Campaign Mode |
| **UTC** | `utc` | Coordinated Universal Time | World Clock and Calendar Day schedules |
| **VPS** | `vps` | Virtual Private Server | Single-server hosting environment after the laptop prototype |
| **VTT** | `vtt` | Virtual Tabletop | Top-down 2D map, grid, and token perspective |

### Defined Terms

| Term | Definition | Owning Document |
|---|---|---|
| **Area Clock** | The local time of day inside a Bubble Area, which advances 6 in-game seconds per completed 5e round while a Turn Bubble is active. | [`design/time_model.md`](design/time_model.md) |
| **Bubble Area** | The spatial extent of an active Turn Bubble plus the perception range around it, in which the Area Clock replaces the World Clock. | [`design/time_model.md`](design/time_model.md) |
| **Calendar Day** | The in-game date, which advances by 1 day per real-world week and dates all history. | [`design/time_model.md`](design/time_model.md) |
| **Campaign Mode** | The playstyle with full AI DM interaction, Dynamic Bubble Turns, PvP, and map alterations. | [`design/game_design.md`](design/game_design.md) |
| **Canonical Order** | The rule that real-world time decides the order of events in the world. | [`design/time_model.md`](design/time_model.md) |
| **Catch-Up** | The rapid advance that returns an Area Clock to the World Clock after a Turn Bubble ends. | [`design/time_model.md`](design/time_model.md) |
| **Dawn** | The start of a Day Cycle, at in-game 06:00. | [`design/time_model.md`](design/time_model.md) |
| **Day Cycle** | One 4-real-world-hour span in which the World Clock runs through 1 full in-game day. | [`design/time_model.md`](design/time_model.md) |
| **Downtime** | The playstyle for non-story activities such as shopping, crafting, and running shops, without AI DM prompts. | [`design/game_design.md`](design/game_design.md) |
| **Dynamic Bubble Turns** | The mechanic that switches a local area from real-time exploration into turn-based play by creating a Turn Bubble. | [`design/game_design.md`](design/game_design.md) |
| **Event Log** | The append-only record of every applied world change and its cause, which is the source of truth for world state. | [`architecture/persistence.md`](architecture/persistence.md) |
| **Freeform Action Prompt** | The Campaign Mode text input in which a player describes any action for the AI DM to adjudicate. | [`architecture/technical_architecture.md`](architecture/technical_architecture.md) |
| **Game Director** | The project owner and final decision authority for all documentation. | [`documentation_specification.md`](documentation_specification.md) |
| **Hybrid Action Bar** | The Campaign Mode bar of predefined actions used alongside the Freeform Action Prompt. | [`architecture/technical_architecture.md`](architecture/technical_architecture.md) |
| **Infinite Downtime** | Remaining in Downtime indefinitely as a Level 1 shopkeeper or townsperson. | [`design/game_design.md`](design/game_design.md) |
| **Lore** | The fixed foundational content of the world, such as gods, eras, continents, and settlements, stored as Lore Entries in `lore/`. | [`design/world_building.md`](design/world_building.md) |
| **Lore Entry** | 1 Markdown file in `lore/` describing 1 piece of Lore. | [`lore_specification.md`](lore_specification.md) |
| **Lore Format** | The rules for how Lore Entries are written, including folders, identifiers, links, and front matter. | [`lore_specification.md`](lore_specification.md) |
| **Lore Principles** | The rules for creating Lore, including the worldbuilding process and where Lore draws its ideas from. | [`design/world_building.md`](design/world_building.md) |
| **Map Partition** | A bounded block of the world map whose identifier is stored on every event and entity for location queries and future server partitioning. | [`architecture/persistence.md`](architecture/persistence.md) |
| **Object** | Any entity in the world that records its own history. | [`design/game_design.md`](design/game_design.md) |
| **One-Shot** | A solo or random Session triggered by an in-world NPC. | [`design/game_design.md`](design/game_design.md) |
| **Overland Scale** | The zoomed-out travel map that covers long distances while in-game time passes at the World Clock rate. | [`design/time_model.md`](design/time_model.md) |
| **Place** | A location that records its own history. | [`design/game_design.md`](design/game_design.md) |
| **Session** | One AI DM story session, either a Campaign Mode party Session or a One-Shot, lasting at most 1 Day Cycle. | [`design/time_model.md`](design/time_model.md) |
| **Settling** | The resolution of an unstable state into its final state through a `settle` timer. | [`design/game_design.md`](design/game_design.md) |
| **Snapshot** | The set of entities the acting character could plausibly perceive, sent to the AI DM with each Freeform Action Prompt. | [`architecture/ai_orchestration.md`](architecture/ai_orchestration.md) |
| **Turn Bubble** | The local turn-based spatial area created by Dynamic Bubble Turns. | [`design/game_design.md`](design/game_design.md) |
| **World Clock** | The single shared time of day for the entire world, which repeats every Day Cycle. | [`design/time_model.md`](design/time_model.md) |
