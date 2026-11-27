# World Geography

| Field | Value |
|---|---|
| **Status** | `Draft` |
| **Scope** | Continuous 6-face cube-sphere coordinate contract, Layer 0 planetary heightmaps, climate layer and ground cover, chunk streaming and terrain splat rendering, non-overlapping authored map regions, and client ground clutter |
| **Last Updated** | `2026-11-27` |
| **Decision Authority** | Game Director (Project Owner) |

---

## Overview
This document defines how the playable world is represented continuously from planetary scale down to individual 5-foot tactical squares with zero loading zones. Every coordinate, elevation unit, climate value, chunk size, and authored region boundary follows 1 contract shared by the server, client, and offline tooling. At gameplay zoom the planet base layer and authored regions render as 1 continuous textured ground with no visible rectangle edge.

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
Does not store per-tile 5-foot wilderness grids on disk. Climate stacks onto this same 6-face `1024 × 1024` grid in Section 3; rivers stack onto it in a later layer.

---

## 3. Climate Layer & Ground Cover

### What
Climate lives in `assets/world/faces/` as 6 `RGBA8` PNG files named `{face}_climate.png`, each `1024 × 1024` pixels, with the same face names and orientation as the `{face}_elevation.png` heightmaps. The channel encoding is listed in the Climate Face Encoding schema. The deterministic generator [`tools/build_climate_faces.py`](../../tools/build_climate_faces.py) builds the 6 files from latitude, an elevation lapse rate, and distance to ocean, following the conventions of [`tools/build_planet_faces.py`](../../tools/build_planet_faces.py). Every generator input comes from the 3D direction of a pixel, so values match across face edges.

[`crates/zeta_core/src/world/climate.rs`](../../crates/zeta_core/src/world/climate.rs) samples climate with `sample_climate(&PlanetClimateMaps, PlanetTile) -> ClimateSample { temperature, moisture, foliage, biome_override }`, returning temperature in degrees Celsius and moisture and foliage in `0–1`. Temperature, moisture, and foliage use the same bicubic cross-face sampling as `sample_macro_elevation_ft`, so values stay continuous across all 6 face edges. The biome override channel uses nearest sampling and is never interpolated.

[`crates/zeta_core/src/world/ground.rs`](../../crates/zeta_core/src/world/ground.rs) derives each tile's ground cover with a pure function, `ground_cover`, that returns 1 value of the closed unit enum `GroundCover`: `deep_water`, `shallow_water`, `beach`, `desert_sand`, `lush_grass`, `grass`, `dry_grass`, `tundra`, `swamp`, `forest_floor`, `dirt`, `rock`, or `snow`. Its inputs are the climate sample, the elevation sample, the slope from neighbor step heights, the distance to shore, and coherent sphere-space noise that breaks cover borders into natural clumps. Steep slopes and alpine heights become rock, peaks above the snowline become snow, tiles near shore become beach, and low-frequency noise places dirt patches and swamp pools; the rule order and thresholds live in the `ground_cover` doc comment. Every deterministic scatter in the world uses 1 stable, documented hash, `tile_hash(PlanetTile) -> u64`. The client loads climate files with `load_planet_climate_maps` in [`crates/zeta_client/src/planet_io.rs`](../../crates/zeta_client/src/planet_io.rs), next to its 16-bit elevation loader.

### Why
Storing climate at the heightmap's resolution keeps the whole planet's climate to a few megabytes and lets the generator derive temperature from the same elevations players walk on. Computing ground cover per tile from stored climate plus deterministic detail gives every 5-foot square a stable cover type with 0 bytes of per-tile storage. The function is pure and has no Bevy dependency, so every caller gets the same cover for the same tile.

### Boundaries
Does not define river networks or named biomes. No biome override ids are defined yet, so every generated pixel stores `0`, meaning no override.

---

## 4. Deterministic Zoom & Chunk Streaming

### What
The planet is divided into `64 × 64` 5-foot tile chunks (`320 × 320` feet, `32,768 × 32,768` chunks per face). Both server and client generate untouched wilderness chunks deterministically in `zeta_core` by bicubic-sampling Layer 0 heightmaps and adding coordinate-seeded fractal detail clamped so macro coastlines never drift.

At gameplay zoom each streamed chunk renders through `TerrainSplatMaterial`, which samples a `texture_2d_array`. A per-chunk control map at `SPLAT_PIXELS_PER_TILE` pixels per tile stores layer indexes and weights for each pixel. The material also applies baked hillshade and a climate tint. The overview zoom keeps the existing color raster path. If the climate maps fail to load, the client logs an error and draws every zoom with the raster path. [`crates/zeta_client/src/chunk_splat.rs`](../../crates/zeta_client/src/chunk_splat.rs) bakes the control map.

The base control map comes from `ground_cover` per tile. Each texel blends the covers of its nearest tiles, and value noise in face texel coordinates moves the border so 5-foot squares never read as blocks. [`assets/terrain/palette.json`](../../assets/terrain/palette.json) lists every texture in the array, and the array index of a texture is its entry order; the entry fields are listed in the Terrain Palette Entry schema. All palette textures share 1 size and 1 format. The palette holds several texture variants per `GroundCover` value, and value noise cross-fades the variants of 1 cover so a large area never reads as 1 repeating texture. Map configurations in `assets/maps/` and region files in `assets/world/regions/` reference palette ids, never raw texture paths. [`crates/zeta_client/src/terrain_palette.rs`](../../crates/zeta_client/src/terrain_palette.rs) loads the palette into the `TerrainPalette` resource.

Texture repeats are computed as `tile mod repeat_period` with integer math on the processor, and each chunk passes the result to the shader as a UV offset. Raw face tile coordinates (up to about 1.8 million) never enter an `f32` shader coordinate.

### Why
Deterministic generation uses 0 bytes of disk storage for untouched wilderness while keeping server passability and client visuals identical on every 5-foot square. One texture array indexed per pixel lets any number of cover types meet inside 1 chunk with 1 shader, so chunk edges and region edges need no special case. An `f32` holds about 7 significant digits, so raw tile coordinates would lose sub-tile precision; the integer modulo keeps texture repeats continuous across chunk and region edges.

### Boundaries
Does not define the fractal noise parameters. Texture names live only in the palette file; Rust iterates the palette and never names a texture. The splat noise is salted per cube face, so the variant and border pattern can change at a face edge.

---

## 5. Authored Map Regions & Live Overrides

### What
Hand-authored places such as towns, road corridors, and landmarks are stored as self-contained region files that claim an explicit polygon footprint in planetary coordinates (matching the polygon convention of `assets/world/world_map.geojson`). Each region file holds its boundary polygon, optional elevation edits, terrain brush layers, and authored entities. Region layer shapes (`LayerShape`) use grid-square units, give full widths, and are region-local, offset by `RegionBounds.min`.

No 2 authored region polygons may overlap. A generic validation check inspects every authored region file and fails with the colliding region names and coordinates if any 2 polygons intersect.

When a `64 × 64` chunk loads, it composites layers in 1 fixed order:
1. Layer 0 and Layer 1 deterministic planetary terrain.
2. Any non-overlapping authored region polygon intersecting the chunk.
3. Dynamic gameplay edits stored per chunk in PostgreSQL and PostGIS, defined in [`persistence.md`](persistence.md) Section 3.

An authored region writes only its painted layers into the control map of every chunk it intersects, feathered by the region's `edge_feather`. A region has no separate mesh drawn over the planet. The grid overlay and `entity_cells` stay derived from live state.

At gameplay zoom each chunk entity carries a `ChunkGroundCover` component: `dominant`, the palette index of the texture at the center of each of its `64 × 64` tiles and `region_painted`, a per-tile flag, both stored row by row. Ground clutter (Section 6) reads only this component.

### Why
Self-contained polygon region files keep authored towns readable and clean in version control while fitting curved rivers, coastlines, and borders without rectangular overlap conflicts. Enforcing non-overlap in automated checks catches boundary collisions at once instead of hiding silent layer-order bugs. Writing region paint into the same control maps as the base layer lets a road blend into the surrounding grass through 1 material, where a separate region mesh left a visible rectangle edge.

### Boundaries
Does not define the in-engine map editor interface or live Event Log persistence schema.

---

## 6. Ground Clutter

### What
Ground clutter is small decorative sprites, such as grass tufts, flowers, and stones, scattered on the ground by the client-only Bevy plugin in [`crates/zeta_client/src/ground_clutter.rs`](../../crates/zeta_client/src/ground_clutter.rs). It appears only at gameplay zoom. The plugin reads `ChunkGroundCover`, the climate foliage value, and `tile_hash`, and spawns each sprite as a child of its chunk so the sprite despawns with the chunk. It skips every tile whose palette entry has `clutter: false` and every tile flagged `region_painted`.

[`assets/terrain/clutter.json`](../../assets/terrain/clutter.json) maps each palette id to a rule with a density, an optional scaling of that density by climate foliage, and a weighted sprite list with a size range in tiles. Sprite names, weights, and the palette id mapping live only in this file; the plugin loads whatever the file lists. The sprites are a curated set of 14 copied from the nature folder of the local asset warehouse into `assets/terrain/clutter/`, using only `painterly` art (see [`../design/visual_style_guide.md`](../design/visual_style_guide.md) Section 3), with their attribution in `assets/terrain/clutter/ATTRIBUTIONS.md`.

### Why
Seeding placement from `tile_hash` puts the same sprite on the same square for every player without network traffic. Reading only `ChunkGroundCover` keeps clutter off water, roads, paths, and town floors without the plugin knowing region or palette internals.

### Boundaries
Ground clutter is visual only; the server does not know it exists, and it never blocks movement. Server-side obstacles placed from ground cover are a later phase, and no part of them exists yet.

---

## Schemas

### Climate Face Encoding
Each channel of a `{face}_climate.png` pixel holds 1 value.

| Channel | Value | Range | Sampling |
|---|---|---|---|
| **R** | Mean annual air temperature | `0–255` -> `-50` to `+40` degrees Celsius, linear | Bicubic |
| **G** | Moisture index | `0–255` -> `0–1` (ocean `1`) | Bicubic |
| **B** | Foliage density | `0–255` -> `0–1` (ocean `0`) | Bicubic |
| **A** | Biome override id | `0` = none | Nearest |

### Terrain Palette Entry
Each entry in the terrain palette file has these fields.

| Field | Type | Meaning |
|---|---|---|
| `id` | String | Name that map configurations and the clutter file reference |
| `texture` | Asset path | Texture loaded into the array at this entry's index |
| `tiles_per_repeat` | Number | 5-foot tiles covered by 1 repeat of the texture |
| `ground_cover` | Optional `GroundCover` name | Ground cover value this entry renders |
| `clutter` | Optional boolean, default `false` | Whether ground clutter grows on this entry; `false` for water, roads, paths, and town floors |
