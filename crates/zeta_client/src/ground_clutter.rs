//! Client-only ground clutter: small painterly grass tufts, flowers, and pebbles scattered
//! over streamed planet chunks at gameplay zoom.
//!
//! Rules: `docs/architecture/world_geography.md` (ground clutter section). Clutter is purely
//! visual: the server never sees it, it never blocks movement, and every placement is a pure
//! function of the tile's hash, so every client draws the same scatter.
//!
//! Data contract:
//! - `assets/terrain/clutter.json` maps a palette id to a weighted sprite list and a density.
//! - Sizes and offsets are in 5-ft tiles. Offsets are measured from the tile center in the
//!   chunk's face frame (`+x` east, `+y` south), and a sprite never leaves its own tile, so a
//!   tuft on a grass tile never overhangs the road tile next to it.

use std::collections::BTreeMap;

use bevy::math::Vec2;
use serde::Deserialize;
use zeta_core::world::hash_unit;

/// Path of the clutter table, relative to `assets/`.
pub const CLUTTER_CONFIG_PATH: &str = "terrain/clutter.json";

/// External clutter table loaded from [`CLUTTER_CONFIG_PATH`].
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ClutterConfig {
    /// Palette id (`assets/terrain/palette.json` `id`) to the clutter drawn on that cover.
    pub palette: BTreeMap<String, ClutterRule>,
}

/// Clutter drawn on every tile whose dominant palette entry has 1 id.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ClutterRule {
    /// Chance in `[0, 1]` that 1 tile carries 1 sprite.
    pub density: f32,
    /// Whether the chance is multiplied by the climate foliage density (plants: yes,
    /// pebbles: no).
    pub follows_foliage: bool,
    /// Weighted sprites to pick from.
    pub sprites: Vec<ClutterSprite>,
}

/// 1 clutter sprite and its size range.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ClutterSprite {
    /// Image path relative to `assets/`.
    pub texture: String,
    /// Relative pick weight, at least 1.
    pub weight: u32,
    /// Smallest drawn size of the sprite's longest side, in tiles.
    pub min_size_tiles: f32,
    /// Largest drawn size of the sprite's longest side, in tiles, at most `1.0`.
    pub max_size_tiles: f32,
}

/// 1 entry of the terrain palette as the clutter table needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaletteClutterFlag<'a> {
    /// Palette id.
    pub id: &'a str,
    /// Whether clutter may grow on this cover (`false` for water, roads, paths, town floors).
    pub clutter: bool,
}

/// [`ClutterConfig`] resolved against the palette: index `i` holds the rule for palette
/// entry `i`, or `None` when that cover carries no clutter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClutterTable {
    rules: Vec<Option<ClutterRule>>,
}

impl ClutterConfig {
    /// Parses and validates a clutter table.
    ///
    /// # Errors
    /// Returns a message naming the first invalid field.
    pub fn from_json(raw: &str) -> Result<Self, String> {
        let config: Self =
            serde_json::from_str(raw).map_err(|error| format!("invalid clutter table: {error}"))?;
        for (id, rule) in &config.palette {
            rule.validate().map_err(|error| format!("palette id {id}: {error}"))?;
        }
        Ok(config)
    }

    /// Reads and validates [`CLUTTER_CONFIG_PATH`] from the workspace `assets/` folder.
    ///
    /// # Errors
    /// Returns a message if the file cannot be read or is invalid.
    pub fn load_default() -> Result<Self, String> {
        let full_path = crate::terrain::assets_root().join(CLUTTER_CONFIG_PATH);
        let raw = std::fs::read_to_string(&full_path)
            .map_err(|error| format!("could not read {}: {error}", full_path.display()))?;
        Self::from_json(&raw).map_err(|error| format!("{}: {error}", full_path.display()))
    }

    /// Every distinct texture path the table references, in a stable order.
    #[must_use]
    pub fn textures(&self) -> Vec<&str> {
        let mut textures: Vec<&str> = self
            .palette
            .values()
            .flat_map(|rule| rule.sprites.iter().map(|sprite| sprite.texture.as_str()))
            .collect();
        textures.sort_unstable();
        textures.dedup();
        textures
    }

    /// Resolves the table against the palette, in palette order.
    ///
    /// # Errors
    /// Returns a message if the table names a palette id that does not exist, or gives
    /// clutter to a cover whose palette entry says `clutter: false`.
    pub fn resolve(&self, palette: &[PaletteClutterFlag<'_>]) -> Result<ClutterTable, String> {
        for id in self.palette.keys() {
            match palette.iter().find(|entry| entry.id == id) {
                None => return Err(format!("clutter table names unknown palette id {id}")),
                Some(entry) if !entry.clutter => {
                    return Err(format!(
                        "clutter table gives clutter to palette id {id}, which has clutter: false"
                    ));
                }
                Some(_) => {}
            }
        }
        let rules = palette
            .iter()
            .map(|entry| {
                entry
                    .clutter
                    .then(|| self.palette.get(entry.id).cloned())
                    .flatten()
            })
            .collect();
        Ok(ClutterTable { rules })
    }
}

impl ClutterRule {
    fn validate(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.density) {
            return Err(format!("density {} is outside [0, 1]", self.density));
        }
        if self.sprites.is_empty() {
            return Err("sprites is empty".to_owned());
        }
        for sprite in &self.sprites {
            if sprite.weight == 0 {
                return Err(format!("{} has weight 0", sprite.texture));
            }
            let sizes_valid = sprite.min_size_tiles > 0.0
                && sprite.min_size_tiles <= sprite.max_size_tiles
                && sprite.max_size_tiles <= 1.0;
            if !sizes_valid {
                return Err(format!(
                    "{} needs 0 < min_size_tiles <= max_size_tiles <= 1",
                    sprite.texture
                ));
            }
        }
        Ok(())
    }
}

/// Everything the placement of 1 tile depends on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClutterTileInput {
    /// Dominant palette index of the tile.
    pub palette_index: usize,
    /// Whether an authored region painted this tile (roads, paths, town floors).
    pub region_painted: bool,
    /// Climate foliage density in `[0, 1]` (`ClimateSample::foliage`).
    pub foliage: f32,
    /// Stable per-tile hash (`zeta_core::world::tile_hash`).
    pub hash: u64,
}

/// Where and how 1 clutter sprite is drawn inside its tile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClutterPlacement {
    /// Index into the tile's [`ClutterRule::sprites`].
    pub sprite: usize,
    /// Offset of the sprite center from the tile center, in tiles (`+x` east, `+y` south).
    pub offset_tiles: Vec2,
    /// Drawn size of the sprite's longest side, in tiles.
    pub size_tiles: f32,
    /// Whether the sprite is mirrored horizontally.
    pub flip_x: bool,
}

impl ClutterTable {
    /// Returns the rule for palette index `palette_index`, if that cover carries clutter.
    #[must_use]
    pub fn rule(&self, palette_index: usize) -> Option<&ClutterRule> {
        self.rules.get(palette_index).and_then(Option::as_ref)
    }

    /// Decides whether 1 tile carries a sprite and, if so, which one and where.
    ///
    /// Pure: the same input always gives the same answer on every client.
    #[must_use]
    pub fn place(&self, input: ClutterTileInput) -> Option<ClutterPlacement> {
        if input.region_painted {
            return None;
        }
        let rule = self.rule(input.palette_index)?;
        let chance = if rule.follows_foliage {
            rule.density * input.foliage.clamp(0.0, 1.0)
        } else {
            rule.density
        };
        if hash_unit(input.hash, SALT_SPAWN) >= chance {
            return None;
        }
        let sprite_index = pick_weighted(&rule.sprites, hash_unit(input.hash, SALT_SPRITE));
        let sprite = &rule.sprites[sprite_index];
        let size_tiles = sprite.min_size_tiles
            + (sprite.max_size_tiles - sprite.min_size_tiles) * hash_unit(input.hash, SALT_SIZE);
        let slack = (1.0 - size_tiles) * 0.5;
        let offset_tiles = Vec2::new(
            (hash_unit(input.hash, SALT_OFFSET_X) * 2.0 - 1.0) * slack,
            (hash_unit(input.hash, SALT_OFFSET_Y) * 2.0 - 1.0) * slack,
        );
        Some(ClutterPlacement {
            sprite: sprite_index,
            offset_tiles,
            size_tiles,
            flip_x: hash_unit(input.hash, SALT_FLIP) < 0.5,
        })
    }
}

// Each decision uses its own salt so the decisions stay uncorrelated with each other and
// with other systems (terrain blending) that read the same tile hash.
const SALT_SPAWN: u64 = 0x636c_7574_7465_7201;
const SALT_SPRITE: u64 = 0x636c_7574_7465_7202;
const SALT_SIZE: u64 = 0x636c_7574_7465_7203;
const SALT_OFFSET_X: u64 = 0x636c_7574_7465_7204;
const SALT_OFFSET_Y: u64 = 0x636c_7574_7465_7205;
const SALT_FLIP: u64 = 0x636c_7574_7465_7206;

/// Picks a sprite index with probability proportional to its weight; `roll` is in `[0, 1)`.
fn pick_weighted(sprites: &[ClutterSprite], roll: f32) -> usize {
    let total: f64 = sprites.iter().map(|sprite| f64::from(sprite.weight)).sum();
    let target = f64::from(roll) * total;
    let mut cumulative = 0.0;
    for (index, sprite) in sprites.iter().enumerate() {
        cumulative += f64::from(sprite.weight);
        if target < cumulative {
            return index;
        }
    }
    sprites.len().saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_TABLE: &str = r#"{
        "palette": {
            "meadow": {
                "density": 1.0,
                "follows_foliage": true,
                "sprites": [
                    { "texture": "a.png", "weight": 3, "min_size_tiles": 0.3, "max_size_tiles": 0.6 },
                    { "texture": "b.png", "weight": 1, "min_size_tiles": 0.2, "max_size_tiles": 0.2 }
                ]
            },
            "gravel": {
                "density": 0.5,
                "follows_foliage": false,
                "sprites": [
                    { "texture": "c.png", "weight": 1, "min_size_tiles": 0.2, "max_size_tiles": 0.4 }
                ]
            }
        }
    }"#;

    const PALETTE: [PaletteClutterFlag<'static>; 4] = [
        PaletteClutterFlag { id: "meadow", clutter: true },
        PaletteClutterFlag { id: "road", clutter: false },
        PaletteClutterFlag { id: "gravel", clutter: true },
        PaletteClutterFlag { id: "snow", clutter: true },
    ];

    fn test_table() -> ClutterTable {
        ClutterConfig::from_json(TEST_TABLE)
            .and_then(|config| config.resolve(&PALETTE))
            .expect("test table is valid")
    }

    fn input(palette_index: usize, hash: u64) -> ClutterTileInput {
        ClutterTileInput {
            palette_index,
            region_painted: false,
            foliage: 1.0,
            hash,
        }
    }

    #[test]
    fn placement_is_identical_for_the_same_tile_input() {
        let table = test_table();
        for hash in 0..2_000_u64 {
            let first = table.place(input(0, hash.wrapping_mul(0x9e37_79b9_7f4a_7c15)));
            let second = table.place(input(0, hash.wrapping_mul(0x9e37_79b9_7f4a_7c15)));
            assert_eq!(first, second);
        }
    }

    #[test]
    fn region_painted_tiles_never_carry_clutter() {
        let table = test_table();
        for hash in 0..2_000_u64 {
            for palette_index in 0..PALETTE.len() {
                let painted = ClutterTileInput {
                    region_painted: true,
                    ..input(palette_index, hash)
                };
                assert_eq!(table.place(painted), None);
            }
        }
    }

    #[test]
    fn covers_without_clutter_or_without_a_rule_never_carry_clutter() {
        let table = test_table();
        for hash in 0..2_000_u64 {
            // `road` has clutter: false; `snow` has no rule; 99 is outside the palette.
            assert_eq!(table.place(input(1, hash)), None);
            assert_eq!(table.place(input(3, hash)), None);
            assert_eq!(table.place(input(99, hash)), None);
        }
    }

    #[test]
    fn spawn_rate_follows_density_and_foliage() {
        let table = test_table();
        let count = |palette_index: usize, foliage: f32| {
            (0..20_000_u64)
                .filter(|&hash| {
                    table
                        .place(ClutterTileInput {
                            foliage,
                            ..input(palette_index, hash.wrapping_mul(0x9e37_79b9_7f4a_7c15))
                        })
                        .is_some()
                })
                .count()
        };
        assert_eq!(count(0, 1.0), 20_000, "density 1 at full foliage fills every tile");
        assert_eq!(count(0, 0.0), 0, "no foliage means no plants");
        let half = count(0, 0.5);
        assert!((9_000..11_000).contains(&half), "half foliage gave {half}");
        let gravel_bare = count(2, 0.0);
        assert!(
            (9_000..11_000).contains(&gravel_bare),
            "pebbles ignore foliage, gave {gravel_bare}"
        );
    }

    #[test]
    fn sprites_stay_inside_their_tile_within_their_size_range() {
        let table = test_table();
        let mut picks = [0_usize; 2];
        for hash in 0..20_000_u64 {
            let placement = table.place(input(0, hash)).expect("density 1 places");
            let sprite = &table.rule(0).expect("meadow rule").sprites[placement.sprite];
            assert!(placement.size_tiles >= sprite.min_size_tiles);
            assert!(placement.size_tiles <= sprite.max_size_tiles);
            let reach = placement.size_tiles * 0.5;
            assert!(placement.offset_tiles.x.abs() + reach <= 0.5 + f32::EPSILON);
            assert!(placement.offset_tiles.y.abs() + reach <= 0.5 + f32::EPSILON);
            picks[placement.sprite] += 1;
        }
        // Weights 3:1.
        assert!((14_000..16_000).contains(&picks[0]), "weighted picks {picks:?}");
    }

    #[test]
    fn resolve_rejects_unknown_ids_and_clutter_on_covers_that_forbid_it() {
        let config = ClutterConfig::from_json(TEST_TABLE).expect("valid");
        let missing = [PaletteClutterFlag { id: "meadow", clutter: true }];
        assert!(config.resolve(&missing).is_err());
        let forbidden = [
            PaletteClutterFlag { id: "meadow", clutter: false },
            PaletteClutterFlag { id: "gravel", clutter: true },
        ];
        assert!(config.resolve(&forbidden).is_err());
    }

    #[test]
    fn from_json_rejects_invalid_rules() {
        let bad = [
            r#"{"palette":{"x":{"density":1.5,"follows_foliage":false,"sprites":[{"texture":"a","weight":1,"min_size_tiles":0.2,"max_size_tiles":0.3}]}}}"#,
            r#"{"palette":{"x":{"density":0.5,"follows_foliage":false,"sprites":[]}}}"#,
            r#"{"palette":{"x":{"density":0.5,"follows_foliage":false,"sprites":[{"texture":"a","weight":0,"min_size_tiles":0.2,"max_size_tiles":0.3}]}}}"#,
            r#"{"palette":{"x":{"density":0.5,"follows_foliage":false,"sprites":[{"texture":"a","weight":1,"min_size_tiles":0.5,"max_size_tiles":0.3}]}}}"#,
            r#"{"palette":{"x":{"density":0.5,"follows_foliage":false,"sprites":[{"texture":"a","weight":1,"min_size_tiles":0.5,"max_size_tiles":1.3}]}}}"#,
        ];
        for raw in bad {
            assert!(ClutterConfig::from_json(raw).is_err(), "accepted {raw}");
        }
    }
}
