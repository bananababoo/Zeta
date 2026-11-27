//! Terrain palette: the single source of truth for every ground texture the client draws.
//!
//! `assets/terrain/palette.json` lists the palette entries. The GPU texture array layer of an
//! entry is its position in that list, so a palette index (`u8`) names both the entry and the
//! array layer. Rust never names a texture: ground covers pick entries through each entry's
//! `ground_cover` field, and authored maps and regions reference entries by `id`.

use std::collections::HashSet;

use bevy::prelude::*;
use serde::Deserialize;
use zeta_core::world::{GroundCover, PlanetTile, hash_unit, tile_hash};

use crate::terrain::assets_root;

/// Palette file path relative to `assets/`.
pub const PALETTE_PATH: &str = "terrain/palette.json";
/// Most entries a palette may hold; sized to the splat shader's per-layer uniform array.
pub const MAX_PALETTE_ENTRIES: usize = 32;
/// Spacing in tiles of the value-noise lattice that picks between texture variants of 1 cover.
const VARIANT_LATTICE_TILES: u32 = 8;
/// [`hash_unit`] salt of the variant lattice values.
const VARIANT_LATTICE_SALT: u64 = 0x7661_7269_616e_7401;
/// [`hash_unit`] salt of the per-tile jitter that dithers variant boundaries.
const VARIANT_JITTER_SALT: u64 = 0x7661_7269_616e_7402;
/// Amplitude of the per-tile jitter added to the variant noise value.
const VARIANT_JITTER: f32 = 0.12;

/// 1 ground texture of the palette.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PaletteEntry {
    /// Stable id referenced by map configs, region configs, and clutter rules.
    pub id: String,
    /// Seamless texture path relative to `assets/`.
    pub texture: String,
    /// How many 5-foot tiles 1 repeat of the texture spans.
    pub tiles_per_repeat: u32,
    /// Snake case [`GroundCover`] this entry renders, if any; several entries of 1 cover are
    /// variants blended by low-frequency noise.
    #[serde(default)]
    pub ground_cover: Option<String>,
    /// Whether clutter may scatter on tiles dominated by this entry.
    #[serde(default)]
    pub clutter: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PaletteFile {
    entries: Vec<PaletteEntry>,
}

/// Parsed and validated terrain palette, kept as a client resource.
#[derive(Resource, Clone, Debug)]
pub struct TerrainPalette {
    entries: Vec<PaletteEntry>,
    /// Palette indices per [`GroundCover`], in [`GroundCover::ALL`] order.
    variants: Vec<Vec<u8>>,
}

impl TerrainPalette {
    /// Reads and validates [`PALETTE_PATH`].
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or fails [`Self::from_json`].
    pub fn load() -> Result<Self, String> {
        let path = assets_root().join(PALETTE_PATH);
        let raw = std::fs::read_to_string(&path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        Self::from_json(&raw).map_err(|error| format!("{}: {error}", path.display()))
    }

    /// Parses and validates palette JSON.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid JSON, fewer than 1 or more than [`MAX_PALETTE_ENTRIES`]
    /// entries, duplicate or empty ids, a zero `tiles_per_repeat`, an unknown `ground_cover`
    /// name, or a [`GroundCover`] that no entry renders.
    pub fn from_json(raw: &str) -> Result<Self, String> {
        let file: PaletteFile =
            serde_json::from_str(raw).map_err(|error| format!("invalid palette: {error}"))?;
        let entries = file.entries;
        if entries.is_empty() || entries.len() > MAX_PALETTE_ENTRIES {
            return Err(format!(
                "palette has {} entries; expected 1..={MAX_PALETTE_ENTRIES}",
                entries.len()
            ));
        }
        let mut ids = HashSet::new();
        let mut variants = vec![Vec::new(); GroundCover::ALL.len()];
        for (index, entry) in entries.iter().enumerate() {
            if entry.id.is_empty() || !ids.insert(entry.id.as_str()) {
                return Err(format!("palette id {:?} is empty or duplicated", entry.id));
            }
            if entry.texture.is_empty() {
                return Err(format!("palette entry {:?} has no texture", entry.id));
            }
            if entry.tiles_per_repeat == 0 {
                return Err(format!("palette entry {:?} has tiles_per_repeat 0", entry.id));
            }
            if let Some(name) = &entry.ground_cover {
                let cover = GroundCover::from_name(name).ok_or_else(|| {
                    format!("palette entry {:?} names unknown ground_cover {name:?}", entry.id)
                })?;
                let index = u8::try_from(index).map_err(|_| "palette index overflow")?;
                variants[cover_slot(cover)].push(index);
            }
        }
        if let Some(missing) = GroundCover::ALL
            .iter()
            .find(|cover| variants[cover_slot(**cover)].is_empty())
        {
            return Err(format!(
                "no palette entry renders ground_cover {:?}",
                missing.as_str()
            ));
        }
        Ok(Self { entries, variants })
    }

    /// Every entry in palette (texture array layer) order.
    #[must_use]
    pub fn entries(&self) -> &[PaletteEntry] {
        &self.entries
    }

    /// Entry at palette index `index`, if any.
    #[must_use]
    pub fn entry(&self, index: u8) -> Option<&PaletteEntry> {
        self.entries.get(usize::from(index))
    }

    /// Palette index of the entry with `id`.
    #[must_use]
    pub fn index_of(&self, id: &str) -> Option<u8> {
        self.entries
            .iter()
            .position(|entry| entry.id == id)
            .and_then(|index| u8::try_from(index).ok())
    }

    /// Palette indices of every entry that renders `cover` (never empty after validation).
    #[must_use]
    pub fn variants_for(&self, cover: GroundCover) -> &[u8] {
        &self.variants[cover_slot(cover)]
    }

    /// Palette index drawn for `cover` on `tile`.
    ///
    /// Variants of 1 cover form patches several tiles wide: a value-noise lattice every
    /// [`VARIANT_LATTICE_TILES`] tiles, hashed with [`tile_hash`], is interpolated and dithered
    /// by a small per-tile jitter. The same tile always draws the same variant.
    #[must_use]
    pub fn variant_at(&self, cover: GroundCover, tile: PlanetTile) -> u8 {
        let variants = self.variants_for(cover);
        if variants.len() <= 1 {
            return variants.first().copied().unwrap_or(0);
        }
        let value = (variant_noise(tile)
            + (hash_unit(tile_hash(tile), VARIANT_JITTER_SALT) - 0.5) * VARIANT_JITTER)
            .clamp(0.0, 0.999_999);
        let slot = floor_to_index(value * len_to_f32(variants.len()), variants.len());
        variants[slot]
    }
}

/// Position of `cover` in [`GroundCover::ALL`].
fn cover_slot(cover: GroundCover) -> usize {
    GroundCover::ALL
        .iter()
        .position(|candidate| *candidate == cover)
        .unwrap_or(0)
}

/// Smooth value noise in `[0.0, 1.0)` over a lattice every [`VARIANT_LATTICE_TILES`] tiles.
fn variant_noise(tile: PlanetTile) -> f32 {
    let cell_x = tile.x / VARIANT_LATTICE_TILES;
    let cell_y = tile.y / VARIANT_LATTICE_TILES;
    let frac = |value: u32| {
        let t = len_to_f32((value % VARIANT_LATTICE_TILES) as usize)
            / len_to_f32(VARIANT_LATTICE_TILES as usize);
        t * t * (3.0 - 2.0 * t)
    };
    let (tx, ty) = (frac(tile.x), frac(tile.y));
    let corner = |dx: u32, dy: u32| {
        // Lattice corners are hashed as tiles; `tile_hash` accepts any in-range integers and
        // the struct fields are public (zeta_core/src/world/cube_sphere.rs:281).
        let lattice = PlanetTile {
            face: tile.face,
            x: (cell_x + dx) * VARIANT_LATTICE_TILES,
            y: (cell_y + dy) * VARIANT_LATTICE_TILES,
        };
        hash_unit(tile_hash(lattice), VARIANT_LATTICE_SALT)
    };
    let top = corner(0, 0) + (corner(1, 0) - corner(0, 0)) * tx;
    let bottom = corner(0, 1) + (corner(1, 1) - corner(0, 1)) * tx;
    top + (bottom - top) * ty
}

#[expect(
    clippy::cast_precision_loss,
    reason = "palette sizes and lattice offsets stay far below 2^23"
)]
fn len_to_f32(value: usize) -> f32 {
    value as f32
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is non-negative and the result is clamped below len"
)]
fn floor_to_index(value: f32, len: usize) -> usize {
    (value.max(0.0) as usize).min(len.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use bevy::asset::RenderAssetUsages;
    use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};
    use zeta_core::world::CubeFace;

    use super::*;

    #[test]
    fn shipped_palette_is_valid_and_every_texture_shares_1_size_and_format() {
        let palette = TerrainPalette::load().expect("assets/terrain/palette.json is valid");
        let mut shape = None;
        for entry in palette.entries() {
            let path = assets_root().join(&entry.texture);
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|error| panic!("{} is missing: {error}", path.display()));
            let extension = path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default();
            // `Image::from_buffer`: bevy_image-0.20.1/src/image.rs:1589.
            let image = Image::from_buffer(
                &bytes,
                ImageType::Extension(extension),
                CompressedImageFormats::NONE,
                true,
                ImageSampler::Default,
                RenderAssetUsages::MAIN_WORLD,
            )
            .unwrap_or_else(|error| panic!("{} does not decode: {error}", path.display()));
            let this = (image.size(), image.texture_descriptor.format);
            let first = *shape.get_or_insert(this);
            assert_eq!(this, first, "{} differs in size or format", entry.texture);
        }
    }

    #[test]
    fn variant_pick_is_deterministic_and_uses_every_variant_of_a_cover() {
        let palette = TerrainPalette::load().expect("valid palette");
        for cover in GroundCover::ALL {
            let variants = palette.variants_for(cover);
            let mut seen = HashSet::new();
            for step in 0..400_u32 {
                let tile = PlanetTile::new(CubeFace::PosX, 1_000 + step * 7, 2_000 + step * 3)
                    .expect("in range");
                let pick = palette.variant_at(cover, tile);
                assert_eq!(pick, palette.variant_at(cover, tile));
                assert!(variants.contains(&pick));
                seen.insert(pick);
            }
            assert_eq!(seen.len(), variants.len(), "{} variants unused", cover.as_str());
        }
    }

    #[test]
    fn palette_rejects_uncovered_ground_and_unknown_names() {
        let one = r#"{"entries":[{"id":"a","texture":"a.png","tiles_per_repeat":4,"ground_cover":"grass"}]}"#;
        assert!(TerrainPalette::from_json(one).is_err());
        let unknown = r#"{"entries":[{"id":"a","texture":"a.png","tiles_per_repeat":4,"ground_cover":"lava"}]}"#;
        assert!(TerrainPalette::from_json(unknown).is_err());
    }
}
