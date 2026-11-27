//! Deterministic per-tile ground cover classification and the shared tile hash.
//!
//! Rules: `docs/architecture/world_geography.md`. Pure and deterministic: the same inputs give
//! the same [`GroundCover`] on the server, every client, and every run. No Bevy.

use serde::{Deserialize, Serialize};

use super::climate::ClimateSample;
use super::cube_sphere::{FACE_TILES, PlanetTile, tile_to_unit_vec};
use super::sampler::{TileSample, splitmix64, value_noise_3d};

/// Distance (in tiles) at which [`shore_distances`] saturates. Callers that compute shore
/// distance for a chunk pass a window padded by this many tiles on every side.
pub const SHORE_DISTANCE_MAX_TILES: u8 = 16;

/// Seed of [`tile_hash`]. Changing it reshuffles every deterministic scatter on the planet.
const TILE_HASH_SEED: u64 = 0x5A3E_7C1D_0B9F_2468;

/// Seed of the low-frequency dirt patch noise.
const DIRT_PATCH_SEED: u64 = 0x1F2E_3D4C_5B6A_7988;
/// Seed of the low-frequency swamp patch noise.
const SWAMP_PATCH_SEED: u64 = 0x7A6B_5C4D_3E2F_1009;

/// Typical diameter of a bare dirt patch, in tiles.
const DIRT_PATCH_TILES: f64 = 20.0;
/// Typical diameter of a swamp pool, in tiles.
const SWAMP_PATCH_TILES: f64 = 48.0;

/// Tiles per radian of arc at a face center (`FACE_TILES` squares span `pi / 2`).
const TILES_PER_RADIAN: f64 = FACE_TILES as f64 / std::f64::consts::FRAC_PI_2;

/// Water within this many tiles of land is shallow.
const SHALLOW_WATER_SHORE_TILES: u8 = 8;
/// Water no deeper than this (feet below sea level) is shallow regardless of shore distance.
const SHALLOW_WATER_MAX_DEPTH_FT: i32 = 20;
/// Largest elevation step (feet) to an edge neighbor that still counts as walkable ground;
/// anything steeper is exposed rock.
const ROCK_MIN_SLOPE_STEP_FT: i32 = 10;
/// Land at or above this elevation with sparse foliage is alpine rock.
const ALPINE_ROCK_MIN_ELEVATION_FT: i32 = 10_000;
/// Foliage below this counts as sparse for the alpine rock rule.
const ALPINE_ROCK_MAX_FOLIAGE: f32 = 0.2;
/// Land at or below this temperature (degrees Celsius, after jitter) is snow.
const SNOW_MAX_TEMPERATURE_C: f32 = -6.0;
/// Beach band width in tiles from the waterline (plus up to 2 tiles of hash jitter).
const BEACH_SHORE_TILES: u8 = 3;
/// Beaches only form on land this close to sea level (feet).
const BEACH_MAX_ELEVATION_FT: i32 = 25;
/// Moisture below this (after jitter) is open sand desert.
const DESERT_MAX_MOISTURE: f32 = 0.12;
/// Swamps need at least this moisture.
const SWAMP_MIN_MOISTURE: f32 = 0.75;
/// Swamps form only on flat land at or below this elevation (feet).
const SWAMP_MAX_ELEVATION_FT: i32 = 300;
/// Swamps freeze over below this temperature (degrees Celsius).
const SWAMP_MIN_TEMPERATURE_C: f32 = 2.0;
/// Foliage at or above this (after jitter) is forest floor.
const FOREST_MIN_FOLIAGE: f32 = 0.55;
/// Land colder than this (degrees Celsius, after jitter) but above snow is tundra.
const TUNDRA_MAX_TEMPERATURE_C: f32 = 0.0;
/// Moisture below this (after jitter) is dry grass.
const DRY_GRASS_MAX_MOISTURE: f32 = 0.3;
/// Moisture at or above this (after jitter) is lush grass.
const LUSH_GRASS_MIN_MOISTURE: f32 = 0.6;

/// Closed set of natural ground surfaces a 5-foot square can show.
///
/// `assets/terrain/palette.json` entries name these variants (snake case) to say which texture
/// renders each one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroundCover {
    /// Open water far from shore or deeper than wading depth.
    DeepWater,
    /// Water near shore or within wading depth.
    ShallowWater,
    /// Sand beach along the waterline.
    Beach,
    /// Open sand desert.
    DesertSand,
    /// Green, well-watered grassland.
    LushGrass,
    /// Temperate grassland.
    Grass,
    /// Dry steppe or savanna grass.
    DryGrass,
    /// Cold, sparse tundra vegetation just above the snow line.
    Tundra,
    /// Waterlogged lowland marsh.
    Swamp,
    /// Leaf litter and undergrowth under dense foliage.
    ForestFloor,
    /// Bare earth patches.
    Dirt,
    /// Exposed rock on steep slopes and alpine heights.
    Rock,
    /// Permanent snow cover.
    Snow,
}

impl GroundCover {
    /// Every variant in declaration order.
    pub const ALL: [Self; 13] = [
        Self::DeepWater,
        Self::ShallowWater,
        Self::Beach,
        Self::DesertSand,
        Self::LushGrass,
        Self::Grass,
        Self::DryGrass,
        Self::Tundra,
        Self::Swamp,
        Self::ForestFloor,
        Self::Dirt,
        Self::Rock,
        Self::Snow,
    ];

    /// Snake case name, identical to the serde name used in asset files.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DeepWater => "deep_water",
            Self::ShallowWater => "shallow_water",
            Self::Beach => "beach",
            Self::DesertSand => "desert_sand",
            Self::LushGrass => "lush_grass",
            Self::Grass => "grass",
            Self::DryGrass => "dry_grass",
            Self::Tundra => "tundra",
            Self::Swamp => "swamp",
            Self::ForestFloor => "forest_floor",
            Self::Dirt => "dirt",
            Self::Rock => "rock",
            Self::Snow => "snow",
        }
    }

    /// Parses a snake case name produced by [`Self::as_str`].
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|cover| cover.as_str() == name)
    }

    /// Whether this cover is water.
    pub const fn is_water(self) -> bool {
        matches!(self, Self::DeepWater | Self::ShallowWater)
    }
}

/// Stable 64-bit hash of a tile, shared by every deterministic per-tile scatter (ground cover
/// dithering, splat edge noise, clutter placement).
///
/// Defined as `splitmix64(splitmix64(splitmix64(SEED ^ face_index) ^ x) ^ y)` over the tile's
/// raw coordinates. Callers must pass a canonical tile (`x`, `y` below `FACE_TILES`); an
/// out-of-range tile hashes differently from the same place on the neighboring face. The value is part of the world's identity: changing it
/// moves every scattered detail, so it never changes.
pub const fn tile_hash(tile: PlanetTile) -> u64 {
    let mut state = splitmix64(TILE_HASH_SEED ^ tile.face.index() as u64);
    state = splitmix64(state ^ tile.x as u64);
    splitmix64(state ^ tile.y as u64)
}

/// Derives an independent uniform value in `[0.0, 1.0)` from a [`tile_hash`] and a `salt`.
///
/// Different salts give uncorrelated values for the same tile, so each consumer (sprite pick,
/// offset, scale) uses its own salt.
pub fn hash_unit(hash: u64, salt: u64) -> f32 {
    let mixed = splitmix64(hash ^ splitmix64(salt));
    // Top 24 bits map exactly onto the f32 mantissa.
    #[allow(
        clippy::cast_precision_loss,
        reason = "a 24-bit integer is exactly representable in f32"
    )]
    let unit = (mixed >> 40) as f32 / (1_u32 << 24) as f32;
    unit
}

/// Everything [`ground_cover`] needs to classify 1 tile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroundCoverInput {
    /// The tile being classified (source of hash and patch noise).
    pub tile: PlanetTile,
    /// Deterministic terrain sample of the tile (`sample_tile`).
    pub sample: TileSample,
    /// Climate at the tile (`sample_climate`).
    pub climate: ClimateSample,
    /// Largest absolute difference in feet between this tile's `elevation_step_ft` and those
    /// of its 4 edge neighbors.
    pub slope_step_ft: i32,
    /// Distance in tiles to the nearest tile of the opposite land/water kind, saturating at
    /// [`SHORE_DISTANCE_MAX_TILES`] (see [`shore_distances`]).
    pub shore_distance_tiles: u8,
}

/// Classifies the natural ground cover of 1 tile.
///
/// Rules, first match wins (thresholds are the constants in this module):
/// 1. Water: shallow within 8 tiles of shore or 20 ft of depth, otherwise deep.
/// 2. Rock: an elevation step of 10 ft or more to an edge neighbor, or alpine heights
///    (10,000 ft and up) with sparse foliage.
/// 3. Snow: temperature at or below -6 °C (jittered ±1.5 °C per tile).
/// 4. Beach: a band 3-5 tiles wide from the waterline, at most 25 ft above sea level.
/// 5. Desert sand: moisture below 0.12.
/// 6. Swamp: flat land (no step to neighbors) at most 300 ft high, moisture 0.75 or more,
///    above 2 °C, inside low-frequency swamp pools.
/// 7. Forest floor: foliage 0.55 or more.
/// 8. Tundra: below 0 °C.
/// 9. Dirt: low-frequency patches, more frequent where drier.
/// 10. Dry grass below moisture 0.3, lush grass from moisture 0.6, grass otherwise.
///
/// Per-tile hash jitter on thresholds dithers boundaries so bands never form straight lines.
/// The biome override id is not interpreted here.
pub fn ground_cover(input: &GroundCoverInput) -> GroundCover {
    let hash = tile_hash(input.tile);
    let sample = input.sample;
    let climate = input.climate;

    if sample.is_water {
        let shallow = input.shore_distance_tiles <= SHALLOW_WATER_SHORE_TILES
            || sample.elevation_ft >= -SHALLOW_WATER_MAX_DEPTH_FT;
        return if shallow {
            GroundCover::ShallowWater
        } else {
            GroundCover::DeepWater
        };
    }

    if input.slope_step_ft >= ROCK_MIN_SLOPE_STEP_FT
        || (sample.elevation_ft >= ALPINE_ROCK_MIN_ELEVATION_FT
            && climate.foliage < ALPINE_ROCK_MAX_FOLIAGE)
    {
        return GroundCover::Rock;
    }

    let jitter = |salt: u64| hash_unit(hash, salt) - 0.5;
    let temperature = climate.temperature + jitter(1) * 3.0;
    if temperature <= SNOW_MAX_TEMPERATURE_C {
        return GroundCover::Snow;
    }

    let beach_extra = if hash_unit(hash, 2) < 0.5 { 0 } else { 2 };
    if input.shore_distance_tiles <= BEACH_SHORE_TILES + beach_extra
        && sample.elevation_ft <= BEACH_MAX_ELEVATION_FT
    {
        return GroundCover::Beach;
    }

    let moisture = climate.moisture + jitter(3) * 0.04;
    if moisture < DESERT_MAX_MOISTURE {
        return GroundCover::DesertSand;
    }

    if input.slope_step_ft == 0
        && sample.elevation_ft <= SWAMP_MAX_ELEVATION_FT
        && moisture >= SWAMP_MIN_MOISTURE
        && temperature >= SWAMP_MIN_TEMPERATURE_C
    {
        // Wetter ground lowers the bar, so swamps grow from pools into wide marshes.
        let swamp_threshold = 0.3 - (moisture - SWAMP_MIN_MOISTURE) * 1.2;
        if patch_noise(input.tile, SWAMP_PATCH_TILES, SWAMP_PATCH_SEED) + jitter(6) * 0.1
            > swamp_threshold
        {
            return GroundCover::Swamp;
        }
    }

    if climate.foliage + jitter(4) * 0.08 >= FOREST_MIN_FOLIAGE {
        return GroundCover::ForestFloor;
    }

    if temperature < TUNDRA_MAX_TEMPERATURE_C {
        return GroundCover::Tundra;
    }

    // Patches: smooth noise in [-1, 1]; drier ground exposes more dirt.
    let dirt_threshold = 0.35 + moisture.clamp(0.0, 1.0) * 0.3;
    if patch_noise(input.tile, DIRT_PATCH_TILES, DIRT_PATCH_SEED) + jitter(5) * 0.1 > dirt_threshold
    {
        return GroundCover::Dirt;
    }

    if moisture < DRY_GRASS_MAX_MOISTURE {
        GroundCover::DryGrass
    } else if moisture >= LUSH_GRASS_MIN_MOISTURE {
        GroundCover::LushGrass
    } else {
        GroundCover::Grass
    }
}

/// Two-octave sphere-space value noise in about `[-1, 1]` with features about
/// `feature_tiles` tiles across; continuous across face seams because it is evaluated in 3D.
fn patch_noise(tile: PlanetTile, feature_tiles: f64, seed: u64) -> f32 {
    let [x, y, z] = tile_to_unit_vec(tile);
    let frequency = TILES_PER_RADIAN / feature_tiles;
    let coarse = value_noise_3d(x * frequency, y * frequency, z * frequency, seed);
    let fine_frequency = frequency * 2.7;
    let fine = value_noise_3d(
        x * fine_frequency,
        y * fine_frequency,
        z * fine_frequency,
        seed ^ 0xFFFF,
    );
    #[allow(
        clippy::cast_possible_truncation,
        reason = "noise lies in [-1, 1], well inside f32 range"
    )]
    let noise = (coarse * 0.7 + fine * 0.3) as f32;
    noise
}

/// Distance in tiles from each cell to the nearest cell of the opposite land/water kind, over
/// a row-major `width × height` window (`is_water[y * width + x]`).
///
/// Uses an 8-neighbor chamfer transform (orthogonal step 1, diagonal step 1.33), saturating
/// at [`SHORE_DISTANCE_MAX_TILES`]. Cells outside the window are unknown, so callers pad the
/// window by [`SHORE_DISTANCE_MAX_TILES`] tiles on each side to get exact values inside it.
///
/// # Panics
/// Panics if `is_water.len() != width * height`.
pub fn shore_distances(is_water: &[bool], width: usize, height: usize) -> Vec<u8> {
    assert_eq!(
        is_water.len(),
        width * height,
        "shore_distances window must hold width * height cells"
    );
    let to_water = chamfer(is_water, width, height, true);
    let to_land = chamfer(is_water, width, height, false);
    is_water
        .iter()
        .zip(to_water.iter().zip(to_land.iter()))
        .map(|(&water, (&dist_water, &dist_land))| {
            let units = if water { dist_land } else { dist_water };
            let tiles = units.div_ceil(3).min(u32::from(SHORE_DISTANCE_MAX_TILES));
            u8::try_from(tiles).unwrap_or(SHORE_DISTANCE_MAX_TILES)
        })
        .collect()
}

/// Two-pass 3-4 chamfer distance (in thirds of a tile) to the nearest cell whose water flag
/// equals `target_water`.
fn chamfer(is_water: &[bool], width: usize, height: usize, target_water: bool) -> Vec<u32> {
    const ORTHOGONAL: u32 = 3;
    const DIAGONAL: u32 = 4;
    let far = u32::from(SHORE_DISTANCE_MAX_TILES) * ORTHOGONAL * 2;
    let mut dist: Vec<u32> = is_water
        .iter()
        .map(|&water| if water == target_water { 0 } else { far })
        .collect();
    let relax = |dist: &mut Vec<u32>, x: usize, y: usize, dx: isize, dy: isize, cost: u32| {
        let (Some(nx), Some(ny)) = (x.checked_add_signed(dx), y.checked_add_signed(dy)) else {
            return;
        };
        if nx >= width || ny >= height {
            return;
        }
        let candidate = dist[ny * width + nx].saturating_add(cost);
        let here = &mut dist[y * width + x];
        *here = (*here).min(candidate);
    };
    for y in 0..height {
        for x in 0..width {
            relax(&mut dist, x, y, -1, 0, ORTHOGONAL);
            relax(&mut dist, x, y, 0, -1, ORTHOGONAL);
            relax(&mut dist, x, y, -1, -1, DIAGONAL);
            relax(&mut dist, x, y, 1, -1, DIAGONAL);
        }
    }
    for y in (0..height).rev() {
        for x in (0..width).rev() {
            relax(&mut dist, x, y, 1, 0, ORTHOGONAL);
            relax(&mut dist, x, y, 0, 1, ORTHOGONAL);
            relax(&mut dist, x, y, 1, 1, DIAGONAL);
            relax(&mut dist, x, y, -1, 1, DIAGONAL);
        }
    }
    dist
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::cube_sphere::CubeFace;

    fn land(elevation_ft: i32) -> TileSample {
        TileSample {
            elevation_ft,
            elevation_step_ft: elevation_ft.div_euclid(5) * 5,
            is_water: elevation_ft < 0,
        }
    }

    fn climate(temperature: f32, moisture: f32, foliage: f32) -> ClimateSample {
        ClimateSample {
            temperature,
            moisture,
            foliage,
            biome_override: None,
        }
    }

    fn tiles(count: u32) -> impl Iterator<Item = PlanetTile> {
        (0..count).map(|index| PlanetTile {
            face: CubeFace::ALL[usize::try_from(index % 6).expect("fits")],
            x: 1_000_000 + index * 7,
            y: 500_000 + index * 13,
        })
    }

    fn classify(
        tile: PlanetTile,
        sample: TileSample,
        clim: ClimateSample,
        slope: i32,
        shore: u8,
    ) -> GroundCover {
        ground_cover(&GroundCoverInput {
            tile,
            sample,
            climate: clim,
            slope_step_ft: slope,
            shore_distance_tiles: shore,
        })
    }

    #[test]
    fn tile_hash_is_stable_and_distinct() {
        let tile = PlanetTile {
            face: CubeFace::PosZ,
            x: 12_345,
            y: 67_890,
        };
        assert_eq!(tile_hash(tile), tile_hash(tile));
        let mut seen = std::collections::BTreeSet::new();
        for face in CubeFace::ALL {
            for offset in 0..64 {
                seen.insert(tile_hash(PlanetTile {
                    face,
                    x: offset,
                    y: 7,
                }));
                seen.insert(tile_hash(PlanetTile {
                    face,
                    x: 7,
                    y: offset,
                }));
            }
        }
        // 6 faces * 127 distinct tiles (the (7, 7) tile appears in both loops).
        assert_eq!(seen.len(), 6 * 127);
    }

    #[test]
    fn hash_unit_is_uniform_in_unit_interval() {
        let mut buckets = [0_u32; 10];
        for tile in tiles(20_000) {
            let unit = hash_unit(tile_hash(tile), 42);
            assert!((0.0..1.0).contains(&unit));
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let bucket = (unit * 10.0) as usize;
            buckets[bucket] += 1;
        }
        for count in buckets {
            assert!(
                (1_600..2_400).contains(&count),
                "non-uniform bucket {count}"
            );
        }
    }

    #[test]
    fn ground_cover_is_deterministic() {
        for tile in tiles(500) {
            let input = (tile, land(40), climate(14.0, 0.45, 0.3), 5, 16);
            assert_eq!(
                classify(input.0, input.1, input.2, input.3, input.4),
                classify(input.0, input.1, input.2, input.3, input.4)
            );
        }
    }

    #[test]
    fn water_tiles_are_water_and_land_tiles_never_are() {
        for tile in tiles(300) {
            assert!(classify(tile, land(-500), climate(20.0, 1.0, 0.0), 0, 2).is_water());
            assert_eq!(
                classify(tile, land(-500), climate(20.0, 1.0, 0.0), 0, 16),
                GroundCover::DeepWater
            );
            assert_eq!(
                classify(tile, land(-5), climate(20.0, 1.0, 0.0), 0, 16),
                GroundCover::ShallowWater
            );
            for clim in [
                climate(-30.0, 0.5, 0.0),
                climate(25.0, 0.05, 0.0),
                climate(15.0, 0.8, 0.9),
            ] {
                assert!(!classify(tile, land(100), clim, 0, 16).is_water());
            }
        }
    }

    #[test]
    fn slopes_shores_cold_and_drought_pick_their_cover() {
        for tile in tiles(300) {
            assert_eq!(
                classify(tile, land(300), climate(15.0, 0.6, 0.8), 15, 16),
                GroundCover::Rock
            );
            assert_eq!(
                classify(tile, land(5), climate(15.0, 0.6, 0.3), 0, 1),
                GroundCover::Beach
            );
            assert_eq!(
                classify(tile, land(100), climate(-25.0, 0.6, 0.3), 0, 16),
                GroundCover::Snow
            );
            assert_eq!(
                classify(tile, land(100), climate(30.0, 0.02, 0.0), 0, 16),
                GroundCover::DesertSand
            );
        }
    }

    #[test]
    fn temperate_land_mixes_grass_with_dirt_patches_and_forest_where_foliage_is_dense() {
        let mut grass = 0;
        let mut soil = 0;
        for tile in tiles(2_000) {
            match classify(tile, land(200), climate(14.0, 0.5, 0.3), 0, 16) {
                GroundCover::Grass => grass += 1,
                GroundCover::Dirt => soil += 1,
                other => panic!("unexpected {other:?} on temperate open land"),
            }
            assert_eq!(
                classify(tile, land(200), climate(14.0, 0.7, 0.9), 0, 16),
                GroundCover::ForestFloor
            );
            assert!(matches!(
                classify(tile, land(200), climate(25.0, 0.2, 0.1), 0, 16),
                GroundCover::DryGrass | GroundCover::Dirt
            ));
            assert!(matches!(
                classify(tile, land(200), climate(18.0, 0.7, 0.3), 0, 16),
                GroundCover::LushGrass | GroundCover::Dirt
            ));
            assert!(matches!(
                classify(tile, land(200), climate(-3.0, 0.5, 0.1), 0, 16),
                GroundCover::Tundra | GroundCover::Snow
            ));
        }
        assert!(grass > soil && soil > 0, "grass {grass} dirt {soil}");
    }

    #[test]
    fn patch_noise_is_spatially_coherent() {
        // Neighboring tiles agree far more often than unrelated tiles would by chance.
        let base = PlanetTile {
            face: CubeFace::PosX,
            x: 900_000,
            y: 900_000,
        };
        let mut agree = 0;
        let total = 4_000;
        for index in 0..total {
            let tile = PlanetTile {
                face: base.face,
                x: base.x + index * 3,
                y: base.y + (index % 50) * 5,
            };
            let next = PlanetTile {
                x: tile.x + 1,
                ..tile
            };
            if (patch_noise(tile, DIRT_PATCH_TILES, DIRT_PATCH_SEED) > 0.4)
                == (patch_noise(next, DIRT_PATCH_TILES, DIRT_PATCH_SEED) > 0.4)
            {
                agree += 1;
            }
        }
        assert!(
            agree * 100 / total >= 90,
            "only {agree}/{total} neighbors agree"
        );
    }

    #[test]
    fn wet_flat_lowlands_grow_swamps_but_slopes_and_highlands_do_not() {
        let mut swamp = 0;
        for tile in tiles(2_000) {
            if classify(tile, land(50), climate(20.0, 0.95, 0.4), 0, 16) == GroundCover::Swamp {
                swamp += 1;
            }
            assert_ne!(
                classify(tile, land(50), climate(20.0, 0.95, 0.4), 5, 16),
                GroundCover::Swamp
            );
            assert_ne!(
                classify(tile, land(2_000), climate(20.0, 0.95, 0.4), 0, 16),
                GroundCover::Swamp
            );
        }
        assert!(
            swamp > 200,
            "only {swamp}/2000 saturated lowland tiles are swamp"
        );
    }

    #[test]
    fn shore_distances_measure_tiles_to_opposite_kind() {
        // 40 x 1 strip: water in columns 0..10, land after.
        let width = 40;
        let is_water: Vec<bool> = (0..width).map(|x| x < 10).collect();
        let dist = shore_distances(&is_water, width, 1);
        assert_eq!(dist[9], 1);
        assert_eq!(dist[10], 1);
        assert_eq!(dist[14], 5);
        assert_eq!(dist[0], 10);
        assert_eq!(dist[39], SHORE_DISTANCE_MAX_TILES);

        // All land: saturates everywhere.
        let all_land = vec![false; 9];
        assert!(
            shore_distances(&all_land, 3, 3)
                .iter()
                .all(|&d| d == SHORE_DISTANCE_MAX_TILES)
        );
    }

    #[test]
    fn ground_cover_names_round_trip() {
        for cover in GroundCover::ALL {
            assert_eq!(GroundCover::from_name(cover.as_str()), Some(cover));
            let json = serde_json::to_string(&cover).expect("serializes");
            assert_eq!(json, format!("\"{}\"", cover.as_str()));
        }
        assert_eq!(GroundCover::from_name("lava"), None);
    }
}
