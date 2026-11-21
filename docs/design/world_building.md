# World Building

| Field | Value |
|---|---|
| **Status** | `Draft` |
| **Scope** | Lore Principles: the separation of design and Lore, the worldbuilding process, inspiration, and time in Lore |
| **Last Updated** | `2026-11-19` |
| **Decision Authority** | Game Director (Project Owner) |

---

## Overview
This document defines the Lore Principles, the rules for creating the world's Lore. It keeps design and Lore apart, sets the order in which the world is built, and sets where Lore draws its ideas from. The Lore itself lives in `lore/`, written in the Lore Format defined in [`../lore_specification.md`](../lore_specification.md).

---

## 1. Design & Lore Separation

### What
**Lore** is the fixed foundational content of the world, such as gods, eras, continents, and settlements, stored as Lore Entries in `lore/` at the repository root. **Lore Principles** are the rules for creating Lore and live only in this document. The **Lore Format** governs how Lore Entries are written and lives only in [`../lore_specification.md`](../lore_specification.md). Documents in `docs/` never contain Lore, and Lore Entries never contain design rules.

### Why
The AI DM reads Lore during play, and the game loads `lore/` directly. If design rules ended up in Lore, the AI DM would read them as facts about the world. Design documents are written for the people and agents building the game.

### Boundaries
Player-created history is not Lore; how fixed Lore and player-created history coexist is defined in [`game_design.md`](game_design.md) Section 1.

---

## 2. Top-Down Process

### What
The world is built top-down in this fixed order, and every layer is settled before work begins on the layer below it:

1. Cosmology
2. Gods and powers
3. History eras
4. Continents and geography
5. Regions and nations
6. Settlements and sites

### Why
Each layer depends on the layers above it. A nation's religion depends on its gods, and its borders depend on the continent's geography. If a lower layer were written first, a later change to a higher layer could contradict it, and every party and the AI DM share the same world.

### Boundaries
Does not define any Lore content or the Lore Entry types used for each layer.

---

## 3. Inspiration

### What
Lore draws from real mythology, folklore, and older or obscure literature from cultures around the world, never from modern D&D settings or other derivative fantasy. There is no approved list of sources, and Lore Entries do not cite sources.

### Why
Early D&D writers drew on world mythology and obscure fiction, and their settings are more memorable than settings built from other fantasy games.

### Boundaries
Does not restrict the story structure the Lore uses, only where its content comes from.

---

## 4. Time in Lore

### What
Lore describes the world on the day the game starts. Past changes, such as a city conquered or a border moved, are recorded only as events.

The objective calendar counts from 1 historical event, which is year 1. The year before it is year 0, and years before that are negative, so the number of years between 2 dates is always the later year minus the earlier year. Cultures that count years from a different event keep their own calendars as perspectives, and every date in them converts to the objective calendar.

### Why
Each Lore Entry records only 1 state, so it never needs a list of past owners or past borders with years attached. A single objective calendar lets the game and the AI DM sort every event, and in-world calendars still differ between cultures, as real calendars do.

### Boundaries
Does not define the calendar's months, days, or year length, which depend on the Lore. How Lore Entries record events is defined in [`../lore_specification.md`](../lore_specification.md) Section 4.
