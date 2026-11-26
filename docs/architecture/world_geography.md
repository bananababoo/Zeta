# World Geography

| Field | Value |
|---|---|
| **Status** | `Draft` |
| **Scope** | Continuous 6-face cube-sphere coordinate contract, Layer 0 planetary heightmaps, chunk streaming, and non-overlapping authored map regions |
| **Last Updated** | `2026-11-26` |
| **Decision Authority** | Game Director (Project Owner) |

---

## Overview
This document defines how the playable world is represented continuously from planetary scale down to individual 5-foot tactical squares with zero loading zones. Every coordinate, elevation unit, chunk size, and authored region boundary follows 1 contract shared by the server, client, and offline tooling.

---

## 1. Cube-Sphere Coordinate Contract

### What
The planet is projected onto 6 square faces of a cube-sphere: `PosX`, `NegX`, `PosY`, `NegY`, `PosZ`, and `NegZ`. Each face is a square grid of **2,097,152 × 2,097,152** (`2^21 × 2^21`) 5-foot squares (`1.524` meters per square), giving an arc length of roughly `3,196` kilometers per face and a planetary circumference of roughly `12,784` kilometers (roughly 1:3 to 1:4 Earth scale).

Every world square is addressed by `PlanetTile { face: CubeFace, x: u32, y: u32 }` with `x` and `y` in `[0, 2097152)`. Mapping between face coordinates `(u, v)` in `[-1, 1]` and unit-sphere direction `(X, Y, Z)` uses the tangent projection `tan(pi / 4 * u)` so 5-foot squares stay nearly uniform in physical area even near cube corners.

A fixed 6 × 4 adjacency table in `zeta_core` translates any step off a face edge into the neighboring `(face, x, y)` and local 90-degree rotation. The client rotates neighboring-face chunks into the camera's local view frame and keeps rendering coordinates near `(0, 0)` with a floating visual origin, so crossing a face seam or walking across a continent has no loading screen and no floating-point jitter.

### Why
A 6-face cube-sphere gives true spherical topology with traversable poles and minimal area distortion. Choosing `2^21` squares per face provides 21 exact power-of-two quadtree levels from whole-planet overview down to individual 5-foot tactical squares and `64 × 64` streaming chunks without fractional splits.

### Boundaries
Does not define multi-server region sharding or orbital mechanics. How Lore records continents and places is defined in [`../lore_specification.md`](../lore_specification.md).

---

## 2. Layer 0 Planetary Base Data

### What
Macro planetary terrain lives in `assets/world/faces/` as 6 unsigned 16-bit grayscale PNG heightmaps (`pos_x_elevation.png`, `neg_x_elevation.png`, `pos_y_elevation.png`, `neg_y_elevation.png`, `pos_z_elevation.png`, `neg_z_elevation.png`), each `1024 × 1024` pixels (~`3.1` kilometers per pixel).

Elevation uses 1 fixed encoding across all 6 faces:
- `32768` is exact sea level (`0` feet).
- Values below `32768` are ocean depth; values at or above `32768` are land elevation.
- Each unit represents `1` foot of vertical offset (`-32768` feet to `+32767` feet), quantized to 5-foot steps for tactical gameplay.

The land and ocean mask is always derived from `elevation >= 32768`, never stored in a separate file.

### Why
Deriving land and ocean directly from the heightmap keeps coastlines and elevations synchronized by construction. Storing `6 × 1024 × 1024` 16-bit samples keeps the entire planet's base geography to a few megabytes while preserving smooth coastlines and mountain chains across all 6 faces.

### Boundaries
Does not store per-tile 5-foot wilderness grids on disk. Climate, rivers, and biomes stack onto this same 6-face `1024 × 1024` grid in later layers.

---

## 3. Deterministic Zoom & Chunk Streaming

### What
The planet is divided into `64 × 64` 5-foot tile chunks (`320 × 320` feet, `32,768 × 32,768` chunks per face). Both server and client generate untouched wilderness chunks deterministically in `zeta_core` by bicubic-sampling Layer 0 heightmaps and adding coordinate-seeded fractal detail clamped so macro coastlines never drift.

Each loaded chunk renders its own 4-material GPU terrain splatmap, allowing adjacent biomes and towns to use different 4-texture palettes without changing the terrain shader.

### Why
Deterministic generation uses 0 bytes of disk storage for untouched wilderness while keeping server passability and client visuals identical on every 5-foot square. Per-chunk splatmaps reuse the existing 4-material splatmap shader across the entire planet.

### Boundaries
Does not define the fractal noise parameters or biome texture palettes, which are tuned as later geography layers are added.

---

## 4. Authored Map Regions & Live Overrides

### What
Hand-authored places such as towns, road corridors, and landmarks are stored as self-contained region files that claim an explicit polygon footprint in planetary coordinates (matching the polygon convention of `assets/world/world_map.geojson`). Each region file holds its boundary polygon, optional elevation edits, terrain brush layers, and authored entities.

No 2 authored region polygons may overlap. A generic validation check inspects every authored region file and fails with the colliding region names and coordinates if any 2 polygons intersect.

When a `64 × 64` chunk loads, it composites layers in 1 fixed order:
1. Layer 0 and Layer 1 deterministic planetary terrain.
2. Any non-overlapping authored region polygon intersecting the chunk.
3. Dynamic gameplay edits stored per chunk in PostgreSQL and PostGIS, defined in [`persistence.md`](persistence.md) Section 3.

### Why
Self-contained polygon region files keep authored towns readable and clean in version control while fitting curved rivers, coastlines, and borders without rectangular overlap conflicts. Enforcing non-overlap in automated checks catches boundary collisions at once instead of hiding silent layer-order bugs.

### Boundaries
Does not define the in-engine map editor interface or live Event Log persistence schema.
