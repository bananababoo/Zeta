# Visual Style Guide

| Field | Value |
|---|---|
| **Status** | `Draft` |
| **Scope** | Client interface theme, typography, monochrome icons, ground clutter art style, and map lighting cohesion |
| **Last Updated** | `2026-11-27` |
| **Decision Authority** | Game Director (Project Owner) |

---

## Overview
This guide defines the visual direction for the client interface and map presentation so assets from multiple sources read as 1 cohesive fantasy game. All runtime colors, fonts, 9-sliced panel sprites, and monochrome icon handles live in [`crates/zeta_client/src/theme.rs`](../../crates/zeta_client/src/theme.rs).

---

## 1. Visual Direction & Palette

### What
The client interface uses a **Warm Wood & Linen** aesthetic: warm, woody, slightly cozy without being overly cute, and closer to stylized tabletop illustration than realism. Gold and metallic finishes are excluded from interface chrome.

| Role | Surface or Color | Hex | Usage |
|---|---|---|---|
| **Primary Panel** | `assets/ui/panel_wood.png` (9-sliced, 14 px inset) | Warm oak wood | Inventory panel, turn banner, dice roll panel, action prompt bar, action menus |
| **Reading Panel** | `assets/ui/panel_parchment.png` (9-sliced, 14 px inset) | Warm cream linen | Narration log journal, hover tooltips |
| **Primary Button** | `assets/ui/button_wood.png` (9-sliced, 10 px inset) | Warm chestnut wood | Top-level toggle buttons |
| **Secondary Button** | `assets/ui/button_parchment.png` (9-sliced, 10 px inset) | Warm linen | Turn banner action button, inventory menu entries |
| **Text on Wood** | Warm cream | `#FBF5EA` | Headings, button labels on wood, prompt text, monochrome icons |
| **Muted Text on Wood** | Soft oatmeal | `#DDD0BE` | Secondary labels on wood |
| **Text on Parchment** | Deep walnut ink | `#2F2219` | Narration log lines, tooltip text, parchment button labels |
| **Status Highlight** | Warm honey ochre | `#F5D5A8` | Action prompt status line |
| **Success / Failure** | Warm sage / terracotta | `#7CCF7C` / `#E8715D` | Settled dice roll outcomes |
| **Slot Categories** | Terracotta, muted slate, tan ochre, mossy sage | Earthy tones | Weapon, armor, tool, and gear inventory slots (`6 px` corner radius, warm cream `#F7EBD1` equipped border) |

All 9-sliced interface images render with `VisualBox::BorderBox` so `Node::padding` keeps text inside the drawn frame rim.

### Why
Warm wood and linen pair naturally with painterly fantasy maps without competing against them, and excluding gold and metal keeps the interface grounded and consistent across panels.

### Boundaries
Individual client plugins must read colors, fonts, and sprites from `UiTheme` instead of hardcoding `BackgroundColor` or `TextColor` values.

---

## 2. Typography

### What
Every client text node uses 1 of 3 locked font roles stored in `assets/fonts/`:

| Role | Font File | Weight | Usage |
|---|---|---|---|
| **Display & Headings** | `assets/fonts/fraunces.ttf` | Bold | Panel headings, turn banner, d20 roll face |
| **Narration & Journal** | `assets/fonts/patrick_hand.ttf` | Regular | Narration log (`19 px`), hover tooltips (`17 px`), speech bubbles |
| **Controls & Numbers** | `assets/fonts/nunito.ttf` | Semibold | Buttons, action prompt input, dice roll details, slot quantities |

Additional open-licensed font candidates (`alegreya`, `alegreya_sans`, `bitter`, `fredoka`, `lora`, `quicksand`) remain in the local asset warehouse for future review.

### Why
A bold soft serif gives headings and dice rolls weight, a hand-inked journal face makes narration feel like a tabletop log, and a rounded sans keeps compact controls and numbers readable.

### Boundaries
Do not introduce a 4th font family into the client interface without Game Director approval.

---

## 3. Icons & Style Quarantine

### What
Interface icons use **monochrome white-on-transparent silhouettes** rasterized into `assets/ui/icons/` from the Game-Icons library by [`tools/build_ui_assets.py`](../../tools/build_ui_assets.py) and tinted at runtime through `UiTheme::tinted_icon`. Inventory slots resolve icons in 2 steps inside `UiTheme::slot_icon` using the external mapping file [`assets/ui/icon_map.json`](../../assets/ui/icon_map.json):
1. Match the item's SRD catalog id against `items` in `assets/ui/icon_map.json` for a specific silhouette (such as `longsword`, `shield`, `leather_armor`, `torch`, `rope`, `rations`, `thieves_tools`, `healers_kit`, or `painters_supplies`).
2. Fall back to 1 of 4 category icons (`weapon`, `armor`, `tool`, `gear`) from `defaults` in `assets/ui/icon_map.json` for unmapped catalog items and custom items.

The local asset warehouse (`assets/library/`, gitignored) tags every entry in its manifest with an `art_style` field (`painterly`, `wood_ui`, `monochrome_ui`, `pixel_placeholder`, `vfx`, `audio`, `font`). Only `wood_ui` and `monochrome_ui` assets are promoted into the client interface; `pixel_placeholder` sprites must never be mixed into the interface. Ground clutter sprites drawn on the map use only `painterly` assets, as defined in [`../architecture/world_geography.md`](../architecture/world_geography.md) Section 6.

### Why
Monochrome silhouettes tinted from 1 palette keep thousands of item, spell, and condition icons visually identical in weight and color, and style quarantine prevents pixel art from clashing with vector interface chrome.

### Boundaries
Only curated assets actively used by the client are promoted from `assets/library/` into tracked `assets/`.

---

## 4. Map Lighting Cohesion

### What
Each external map configuration JSON (such as [`assets/maps/river_crossing.json`](../../assets/maps/river_crossing.json)) defines an optional `ambient_tint` RGB triple (default `[1.0, 1.0, 1.0]`, set to `[1.05, 1.0, 0.91]` for warm daylight on the river crossing). The GPU terrain shader ([`assets/shaders/terrain_splat.wgsl`](../../assets/shaders/terrain_splat.wgsl)) and entity sprite styling ([`crates/zeta_client/src/map.rs`](../../crates/zeta_client/src/map.rs)) both multiply by this same ambient tint.

### Why
Applying 1 shared lighting tint to terrain textures and entity sprites blends minor saturation and color-temperature differences across painterly art packs.

### Boundaries
Map lighting tint is data-driven per map config file and never hardcoded per entity or biome in Rust.
