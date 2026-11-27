//! Terrain palette: the single source of truth for every ground texture the client draws.
//!
//! `assets/terrain/palette.json` lists the palette entries. The GPU texture array layer of an
//! entry is its position in that list, so a palette index (`u8`) names both the entry and the
//! array layer. Rust never names a texture: ground covers pick entries through each entry's
//! `ground_cover` field, and authored maps and regions reference entries by `id`.

use std::collections::HashSet;
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::image::{
    CompressedImageFormats, ImageAddressMode, ImageSampler, ImageSamplerDescriptor, ImageType,
};
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};
use bevy::sprite_render::Material2dPlugin;
use serde::Deserialize;
use zeta_core::world::{CubeFace, GroundCover, PlanetTile, hash_unit};

use crate::terrain::{TerrainSplatMaterial, assets_root, smoothstep};

/// Palette file path relative to `assets/`.
pub const PALETTE_PATH: &str = "terrain/palette.json";
/// Most entries a palette may hold; sized to the splat shader's per-layer uniform array.
pub const MAX_PALETTE_ENTRIES: usize = 32;
/// Variant noise octaves: lattice spacing in tiles and amplitude (amplitudes sum to 1).
const VARIANT_OCTAVES: [(i64, f32); 2] = [(10, 0.7), (3, 0.3)];
/// [`hash_unit`] salt of the variant lattice values.
const VARIANT_LATTICE_SALT: u64 = 0x7661_7269_616e_7401;
/// Half width, in variant bands, of the cross-fade between 2 neighboring variants.
const VARIANT_BLEND_HALF_WIDTH: f32 = 0.2;

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
                return Err(format!(
                    "palette entry {:?} has tiles_per_repeat 0",
                    entry.id
                ));
            }
            if let Some(name) = &entry.ground_cover {
                let cover = GroundCover::from_name(name).ok_or_else(|| {
                    format!(
                        "palette entry {:?} names unknown ground_cover {name:?}",
                        entry.id
                    )
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

    /// Palette index drawn for `cover` on `tile`: the heavier of [`Self::variant_mix`] at the
    /// tile center, so per-tile facts agree with the splat texels around that center.
    #[must_use]
    pub fn variant_at(&self, cover: GroundCover, tile: PlanetTile) -> u8 {
        let value = variant_value(
            tile.face,
            i64::from(tile.x) * 2 + 1,
            i64::from(tile.y) * 2 + 1,
            2,
        );
        let [(first, first_weight), (second, second_weight)] = self.variant_mix(cover, value);
        if second_weight > first_weight {
            second
        } else {
            first
        }
    }

    /// The 2 variants of `cover` drawn at variant noise `value` (from [`variant_value`]) and
    /// their weights, which sum to 1.
    ///
    /// `value` is split into 1 equal band per variant; neighboring variants cross-fade over
    /// [`VARIANT_BLEND_HALF_WIDTH`] slots around each band border, so variant patches have
    /// soft, organic edges instead of tile-shaped ones.
    #[must_use]
    pub fn variant_mix(&self, cover: GroundCover, value: f32) -> [(u8, f32); 2] {
        let variants = self.variants_for(cover);
        let count = variants.len();
        if count <= 1 {
            let only = variants.first().copied().unwrap_or(0);
            return [(only, 1.0), (only, 0.0)];
        }
        let position = value.clamp(0.0, 0.999_999) * len_to_f32(count) - 0.5;
        let low = position.floor();
        let blend = smoothstep(
            0.5 - VARIANT_BLEND_HALF_WIDTH,
            0.5 + VARIANT_BLEND_HALF_WIDTH,
            position - low,
        );
        let lower = variants[floor_to_index(low, count)];
        let upper = variants[floor_to_index(low + 1.0, count)];
        [(lower, 1.0 - blend), (upper, blend)]
    }
}

/// Variant noise in `[0.0, 1.0)` at face position `(fine_x, fine_y) / fine_per_tile` tiles.
///
/// Callers pass sample centers in fine units (tile centers use `2 * tile + 1` with
/// `fine_per_tile = 2`), so a tile and the splat texels covering it read 1 continuous field.
/// Octaves are [`VARIANT_OCTAVES`], salted per face.
#[must_use]
pub fn variant_value(face: CubeFace, fine_x: i64, fine_y: i64, fine_per_tile: i64) -> f32 {
    let face_salt = u64::try_from(face.index()).unwrap_or(0) << 56;
    let total: f32 = VARIANT_OCTAVES
        .iter()
        .zip(0_u64..)
        .map(|(&(spacing_tiles, amplitude), octave)| {
            value_noise_unit(
                (VARIANT_LATTICE_SALT ^ face_salt).wrapping_add(octave),
                fine_x,
                fine_y,
                spacing_tiles * fine_per_tile,
            ) * amplitude
        })
        .sum();
    total.clamp(0.0, 0.999_999)
}

/// Smooth value noise in `[0.0, 1.0)` at integer position `(x, y)` over a lattice every
/// `spacing` units, from [`hash_unit`] of each lattice point.
#[must_use]
pub fn value_noise_unit(salt: u64, x: i64, y: i64, spacing: i64) -> f32 {
    let spacing = spacing.max(1);
    let (cell_x, cell_y) = (x.div_euclid(spacing), y.div_euclid(spacing));
    let smooth = |value: i64| {
        let t = small_i64_to_f32(value.rem_euclid(spacing)) / small_i64_to_f32(spacing);
        t * t * (3.0 - 2.0 * t)
    };
    let (tx, ty) = (smooth(x), smooth(y));
    // Face positions stay below 2^32 fine units, so the low 32 bits name each lattice point.
    let low = |value: i64| u64::try_from(value & 0xFFFF_FFFF).unwrap_or(0);
    let corner = |dx: i64, dy: i64| {
        hash_unit((low(cell_x + dx) << 32) | low(cell_y + dy), salt)
    };
    let top = corner(0, 0) + (corner(1, 0) - corner(0, 0)) * tx;
    let bottom = corner(0, 1) + (corner(1, 1) - corner(0, 1)) * tx;
    (top + (bottom - top) * ty).clamp(0.0, 0.999_999)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "noise offsets are below the lattice spacing, far below 2^23"
)]
fn small_i64_to_f32(value: i64) -> f32 {
    value as f32
}

/// Position of `cover` in [`GroundCover::ALL`].
fn cover_slot(cover: GroundCover) -> usize {
    GroundCover::ALL
        .iter()
        .position(|candidate| *candidate == cover)
        .unwrap_or(0)
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

/// Per palette layer uniform of `TerrainSplatMaterial` for a quad whose top-left tile is
/// `(origin_x, origin_y)`: `xy` = origin modulo the layer repeat, `z` = 1 / repeat.
///
/// The modulo is integer math, so UVs stay continuous across quads without ever putting a raw
/// face tile coordinate into an `f32` (`terrain_contract.md` Section 1).
#[must_use]
pub fn layer_params(
    palette: &TerrainPalette,
    origin_x: u32,
    origin_y: u32,
) -> [Vec4; MAX_PALETTE_ENTRIES] {
    let mut params = [Vec4::new(0.0, 0.0, 1.0, 0.0); MAX_PALETTE_ENTRIES];
    for (slot, entry) in params.iter_mut().zip(palette.entries()) {
        let repeat = entry.tiles_per_repeat;
        *slot = Vec4::new(
            len_to_f32((origin_x % repeat) as usize),
            len_to_f32((origin_y % repeat) as usize),
            1.0 / len_to_f32(repeat as usize),
            0.0,
        );
    }
    params
}

/// GPU side of the palette: the mipmapped texture array and the shared unit quad mesh.
#[derive(Resource, Clone, Debug)]
pub struct TerrainPaletteGpu {
    /// Parsed palette whose entry order matches the array layers.
    pub palette: Arc<TerrainPalette>,
    /// `texture_2d_array` with 1 layer per palette entry.
    pub layers: Handle<Image>,
    /// 1 x 1 rectangle scaled by each terrain quad's transform.
    pub unit_mesh: Handle<Mesh>,
}

/// Loads the palette and its texture array at startup and registers the splat material.
///
/// Added by `MapPlugin` and `PlanetStreamPlugin` when `AssetPlugin` exists. A palette error is
/// logged and leaves [`TerrainPaletteGpu`] absent, so terrain falls back to the raster path.
pub struct TerrainPalettePlugin;

impl Plugin for TerrainPalettePlugin {
    fn build(&self, app: &mut App) {
        // `App::is_plugin_added`: bevy_app-0.20.1/src/app.rs:570.
        if !app.is_plugin_added::<Material2dPlugin<TerrainSplatMaterial>>() {
            app.add_plugins(Material2dPlugin::<TerrainSplatMaterial>::default());
        }
        app.add_systems(PreStartup, load_terrain_palette);
    }
}

/// Adds [`TerrainPalettePlugin`] unless an earlier plugin already did.
pub fn add_terrain_palette_plugin(app: &mut App) {
    if !app.is_plugin_added::<TerrainPalettePlugin>() {
        app.add_plugins(TerrainPalettePlugin);
    }
}

fn load_terrain_palette(
    mut commands: Commands,
    images: Option<ResMut<Assets<Image>>>,
    meshes: Option<ResMut<Assets<Mesh>>>,
) {
    let (Some(mut images), Some(mut meshes)) = (images, meshes) else {
        return;
    };
    let palette = match TerrainPalette::load() {
        Ok(palette) => palette,
        Err(error) => {
            error!("terrain palette unavailable, splat terrain disabled: {error}");
            return;
        }
    };
    let array = match build_palette_array(&palette) {
        Ok(array) => array,
        Err(error) => {
            error!("terrain palette textures unavailable, splat terrain disabled: {error}");
            commands.insert_resource(palette);
            return;
        }
    };
    commands.insert_resource(palette.clone());
    commands.insert_resource(TerrainPaletteGpu {
        palette: Arc::new(palette),
        layers: images.add(array),
        // `Rectangle::new`: bevy_shape-0.20.1/src/dim2.rs:1827; UV (0, 0) is the top-left
        // corner (bevy_mesh-0.20.1/src/primitives/dim2.rs:1068).
        unit_mesh: meshes.add(Rectangle::new(1.0, 1.0)),
    });
}

/// Decodes every palette texture and builds 1 mipmapped `Rgba8UnormSrgb` texture array.
///
/// # Errors
///
/// Returns an error naming the file when a texture is missing, does not decode, or differs in
/// size from the first entry.
pub fn build_palette_array(palette: &TerrainPalette) -> Result<Image, String> {
    let mut size = None;
    let mut layers = Vec::with_capacity(palette.entries().len());
    for entry in palette.entries() {
        let path = assets_root().join(&entry.texture);
        let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
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
        .map_err(|error| format!("{}: {error}", path.display()))?;
        // `Image::convert`: bevy_image-0.20.1/src/image.rs:1556.
        let image = if image.texture_descriptor.format == TextureFormat::Rgba8UnormSrgb {
            image
        } else {
            image
                .convert(TextureFormat::Rgba8UnormSrgb)
                .ok_or_else(|| format!("{} cannot convert to RGBA8 sRGB", path.display()))?
        };
        let this = (image.width(), image.height());
        if *size.get_or_insert(this) != this {
            return Err(format!(
                "{} differs in size from the first entry",
                path.display()
            ));
        }
        let data = image
            .data
            .ok_or_else(|| format!("{} has no pixel data", path.display()))?;
        layers.push(data);
    }
    let (width, height) = size.ok_or("palette has no entries")?;

    let mut mip_count = 1_u32;
    let mut bytes = Vec::new();
    for layer in &layers {
        let mut level = layer.clone();
        let (mut w, mut h) = (width, height);
        let mut levels = 1;
        bytes.extend_from_slice(&level);
        while w > 1 || h > 1 {
            let (next, nw, nh) = downsample_srgb(&level, w, h);
            bytes.extend_from_slice(&next);
            level = next;
            (w, h) = (nw, nh);
            levels += 1;
        }
        mip_count = levels;
    }

    let layer_count = u32::try_from(layers.len()).map_err(|_| "palette layer overflow")?;
    // `Image::new_uninit` skips the 1-mip length assert of `Image::new`
    // (bevy_image-0.20.1/src/image.rs:1143); data is layer-major, each layer with its full mip
    // chain (`TextureDataOrder::LayerMajor`, wgpu-types texture.rs:41).
    let mut image = Image::new_uninit(
        Extent3d {
            width,
            height,
            depth_or_array_layers: layer_count,
        },
        TextureDimension::D2,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_descriptor.mip_level_count = mip_count;
    image.data = Some(bytes);
    // `Image::texture_view_descriptor`: bevy_image-0.20.1/src/image.rs:662; same pattern as
    // the DDS loader (bevy_image-0.20.1/src/dds.rs:97).
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::D2Array),
        ..default()
    });
    // `ImageSamplerDescriptor::linear` sets a linear mipmap filter
    // (bevy_image-0.20.1/src/image.rs:903); `ImageAddressMode::Repeat` (image.rs:747).
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        anisotropy_clamp: 8,
        ..ImageSamplerDescriptor::linear()
    });
    Ok(image)
}

/// sRGB byte to linear light.
fn srgb_to_linear(value: u8) -> f32 {
    let c = f32::from(value) / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light to an sRGB byte.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is clamped to [0.0, 255.0] before conversion"
)]
fn linear_to_srgb(value: f32) -> u8 {
    let c = value.clamp(0.0, 1.0);
    let encoded = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

/// Halves an RGBA8 sRGB image with a 2 x 2 box filter in linear light (alpha averaged as is).
fn downsample_srgb(data: &[u8], width: u32, height: u32) -> (Vec<u8>, u32, u32) {
    let (nw, nh) = ((width / 2).max(1), (height / 2).max(1));
    let mut out = Vec::with_capacity((nw * nh * 4) as usize);
    for y in 0..nh {
        for x in 0..nw {
            let mut sum = [0.0_f32; 4];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let sx = (x * 2 + dx).min(width - 1);
                let sy = (y * 2 + dy).min(height - 1);
                let i = ((sy * width + sx) * 4) as usize;
                for (channel, total) in sum.iter_mut().enumerate().take(3) {
                    *total += srgb_to_linear(data[i + channel]);
                }
                sum[3] += f32::from(data[i + 3]) / 255.0;
            }
            out.extend_from_slice(&[
                linear_to_srgb(sum[0] * 0.25),
                linear_to_srgb(sum[1] * 0.25),
                linear_to_srgb(sum[2] * 0.25),
                linear_to_srgb_alpha(sum[3] * 0.25),
            ]);
        }
    }
    (out, nw, nh)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is clamped to [0.0, 255.0] before conversion"
)]
fn linear_to_srgb_alpha(value: f32) -> u8 {
    (value * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use zeta_core::world::CubeFace;

    use super::*;

    #[test]
    fn palette_array_has_1_layer_per_entry_and_a_full_mip_chain() {
        let palette = TerrainPalette::load().expect("valid palette");
        let array = build_palette_array(&palette).expect("palette textures decode");
        let desc = &array.texture_descriptor;
        assert_eq!(
            desc.size.depth_or_array_layers as usize,
            palette.entries().len()
        );
        let largest = desc.size.width.max(desc.size.height);
        assert_eq!(desc.mip_level_count, 32 - largest.leading_zeros());
        let mut texels = 0_u64;
        let (mut w, mut h) = (desc.size.width, desc.size.height);
        for _ in 0..desc.mip_level_count {
            texels += u64::from(w) * u64::from(h);
            (w, h) = ((w / 2).max(1), (h / 2).max(1));
        }
        let expected = texels * 4 * u64::from(desc.size.depth_or_array_layers);
        assert_eq!(
            array.data.as_ref().map(Vec::len),
            usize::try_from(expected).ok()
        );
    }

    #[test]
    fn layer_params_wrap_origins_with_integer_modulo() {
        let palette = TerrainPalette::load().expect("valid palette");
        let params = layer_params(&palette, 4_000_000, 3_999_999);
        for (param, entry) in params.iter().zip(palette.entries()) {
            let repeat = entry.tiles_per_repeat;
            assert_eq!(param.x, len_to_f32((4_000_000 % repeat) as usize));
            assert_eq!(param.y, len_to_f32((3_999_999 % repeat) as usize));
            assert!(param.x < len_to_f32(repeat as usize));
        }
    }

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
            assert_eq!(
                seen.len(),
                variants.len(),
                "{} variants unused",
                cover.as_str()
            );
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
