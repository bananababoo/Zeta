//! Terrain palette: the single source of truth for every ground texture the client draws.
//!
//! `assets/terrain/palette.json` lists the palette entries. The GPU texture array layer of an
//! entry is its position in that list, so a palette index (`u8`) names both the entry and the
//! array layer. Rust never names a texture: ground covers pick entries through each entry's
//! `ground_cover` field, and authored maps and regions reference entries by `id`.

use std::collections::HashSet;
use std::f64::consts::FRAC_PI_2;
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
use zeta_core::world::{
    CubeFace, FACE_TILES, GroundCover, PlanetTile, face_uv_to_unit_vec, hash_unit,
};

use crate::terrain::{TerrainSplatMaterial, assets_root, smoothstep};

/// Palette file path relative to `assets/`.
pub const PALETTE_PATH: &str = "terrain/palette.json";
/// Most entries a palette may hold; sized to the splat shader's per-layer uniform array.
pub const MAX_PALETTE_ENTRIES: usize = 32;
/// 3D sphere-space variant noise octaves: feature diameter in tiles, weight, and seed.
const VARIANT_SPHERE_OCTAVES: [(f64, f32, u64); 2] = [
    (18.0, 0.68, 0x7661_7269_616e_7401),
    (5.5, 0.32, 0x7661_7269_616e_7402),
];
/// Tiles per radian of arc at a cube-face center (`FACE_TILES` spans `pi / 2`).
const TILES_PER_RADIAN: f64 = FACE_TILES as f64 / FRAC_PI_2;
/// Half width, in variant bands, of the painterly cross-fade between 2 neighboring variants.
const VARIANT_BLEND_HALF_WIDTH: f32 = 0.36;

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
/// Evaluated in 3D unit-sphere space via [`face_uv_to_unit_vec`] so variant patches stay
/// continuous across both `64x64` chunk borders and rotated cube-face seams.
#[must_use]
pub fn variant_value(face: CubeFace, fine_x: i64, fine_y: i64, fine_per_tile: i64) -> f32 {
    let denom = f64::from(FACE_TILES) * fine_i64_to_f64(fine_per_tile.max(1));
    let face_u = (fine_i64_to_f64(fine_x) / denom) * 2.0 - 1.0;
    let face_v = (fine_i64_to_f64(fine_y) / denom) * 2.0 - 1.0;
    let unit_vec = face_uv_to_unit_vec(face, face_u, face_v);
    let raw: f32 = VARIANT_SPHERE_OCTAVES
        .iter()
        .map(|&(feature_tiles, weight, seed)| {
            sphere_noise_signed(unit_vec, TILES_PER_RADIAN / feature_tiles, seed) * weight
        })
        .sum();
    smoothstep(-0.52, 0.52, raw).clamp(0.0, 0.999_999)
}

/// Deterministic 3D trilinear value noise in `[-1.0, 1.0]` on the unit sphere.
#[must_use]
pub(crate) fn sphere_noise_signed(unit_vec: [f64; 3], frequency: f64, seed: u64) -> f32 {
    let [px, py, pz] = unit_vec.map(|v| v * frequency);
    let (fx, fy, fz) = (px.floor(), py.floor(), pz.floor());
    let quintic = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let (tx, ty, tz) = (quintic(px - fx), quintic(py - fy), quintic(pz - fz));
    let (ix, iy, iz) = (
        floor_f64_to_i64(fx),
        floor_f64_to_i64(fy),
        floor_f64_to_i64(fz),
    );
    let corner = |dx: i64, dy: i64, dz: i64| {
        let mut state = splitmix64_palette(seed ^ ix.wrapping_add(dx).cast_unsigned());
        state = splitmix64_palette(state ^ iy.wrapping_add(dy).cast_unsigned());
        state = splitmix64_palette(state ^ iz.wrapping_add(dz).cast_unsigned());
        lattice_mantissa_to_signed(state)
    };
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let x00 = lerp(corner(0, 0, 0), corner(1, 0, 0), tx);
    let x10 = lerp(corner(0, 1, 0), corner(1, 1, 0), tx);
    let x01 = lerp(corner(0, 0, 1), corner(1, 0, 1), tx);
    let x11 = lerp(corner(0, 1, 1), corner(1, 1, 1), tx);
    let y0 = lerp(x00, x10, ty);
    let y1 = lerp(x01, x11, ty);
    narrow_f64_to_f32(lerp(y0, y1, tz))
}

const fn splitmix64_palette(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "fine face coordinates stay below 2^26, exact in 53-bit f64"
)]
fn fine_i64_to_f64(value: i64) -> f64 {
    value as f64
}

#[expect(
    clippy::cast_precision_loss,
    reason = "53-bit mantissa fits losslessly in IEEE-754 f64"
)]
fn lattice_mantissa_to_signed(state: u64) -> f64 {
    let unit = ((state >> 11) as f64) * (1.0 / ((1_u64 << 53) as f64));
    unit * 2.0 - 1.0
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "unit-sphere coordinates scaled by variant frequencies fit comfortably in i64"
)]
fn floor_f64_to_i64(value: f64) -> i64 {
    value as i64
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "trilinear noise lies in [-1.0, 1.0], well inside f32 range"
)]
fn narrow_f64_to_f32(value: f64) -> f32 {
    value as f32
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
    let corner = |dx: i64, dy: i64| hash_unit((low(cell_x + dx) << 32) | low(cell_y + dy), salt);
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

/// Integer rotation basis `(a, b)` for secondary UV anti-tiling (`atan2(2, 3) ≈ 33.7°`).
const SECONDARY_ROTATION_A: i64 = 3;
const SECONDARY_ROTATION_B: i64 = 2;
/// Multiplier from primary `tiles_per_repeat` to the secondary macro torus period (`32 = 2^5`).
const MACRO_PERIOD_MULTIPLIER: i64 = 32;
/// Upper bound on `tiles_per_repeat` for 12-bit secondary modulo packing (`32 * 127 < 4096`).
const MAX_SECONDARY_REPEAT_TILES: u32 = 127;
/// Packing radix (`2^12`) keeping `u * 4096 + v < 2^24` exact in IEEE-754 `f32`.
const SECONDARY_PACK_RADIX: i64 = 4096;

#[expect(
    clippy::cast_precision_loss,
    reason = "packed 2 x 12-bit modulo origin stays strictly below 2^24, exact in f32"
)]
fn packed_secondary_origin(origin_x: u32, origin_y: u32, repeat: u32) -> f32 {
    let origin_col = i64::from(origin_x);
    let origin_row = i64::from(origin_y);
    let secondary_repeat = i64::from(repeat.clamp(1, MAX_SECONDARY_REPEAT_TILES));
    let macro_period = MACRO_PERIOD_MULTIPLIER * secondary_repeat;
    let secondary_u = (SECONDARY_ROTATION_A * origin_col - SECONDARY_ROTATION_B * origin_row)
        .rem_euclid(macro_period);
    let secondary_v = (SECONDARY_ROTATION_B * origin_col + SECONDARY_ROTATION_A * origin_row)
        .rem_euclid(macro_period);
    (secondary_u * SECONDARY_PACK_RADIX + secondary_v) as f32
}

/// Per palette layer uniform of `TerrainSplatMaterial` for a quad whose top-left tile is
/// `(origin_x, origin_y)`: `xy` = origin modulo the layer repeat, `z` = 1 / repeat, `w` = packed
/// rotated macro origin `(u * 4096 + v)` modulo `32 * min(repeat, 127)`.
///
/// Both primary and secondary modulos are computed in integer math on the processor, so UVs stay
/// continuous across quads without ever putting a raw face tile coordinate into an `f32`
/// (`docs/architecture/world_geography.md` Section 4).
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
            packed_secondary_origin(origin_x, origin_y, repeat),
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
    // (bevy_image-0.20.1/src/image.rs:903); `ImageAddressMode::Repeat` (image.rs:758).
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
            assert!(param.w >= 0.0 && param.w < 16_777_216.0);
            assert_eq!(param.w.fract(), 0.0);
        }
    }

    #[test]
    fn secondary_layer_params_wrap_continuously_across_chunk_edges() {
        let palette = TerrainPalette::load().expect("valid palette");
        let origin_x = 1_821_024_u32;
        let origin_y = 316_448_u32;
        let base = layer_params(&palette, origin_x, origin_y);
        let east = layer_params(&palette, origin_x + 64, origin_y);
        let south = layer_params(&palette, origin_x, origin_y + 64);

        let eval_base_uv = |param: Vec4, tile: Vec2| -> Vec2 {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "param.w holds a non-negative integer strictly below 2^24"
            )]
            let packed = param.w as u32;
            let high = u16::try_from(packed >> 12).expect("12-bit u fits in u16");
            let low = u16::try_from(packed & 4095).expect("12-bit v fits in u16");
            let sec_origin = Vec2::new(f32::from(high), f32::from(low));
            let rot_tile = Vec2::new(3.0 * tile.x - 2.0 * tile.y, 2.0 * tile.x + 3.0 * tile.y);
            let base_scale = param.z.max(1.0 / 127.0) * 0.03125;
            let raw = (rot_tile + sec_origin) * base_scale;
            Vec2::new(raw.x.rem_euclid(1.0), raw.y.rem_euclid(1.0))
        };

        for index in 0..palette.entries().len() {
            for sample_t in [0.0_f32, 13.25, 47.75, 64.0] {
                let lhs_east = eval_base_uv(base[index], Vec2::new(64.0, sample_t));
                let rhs_east = eval_base_uv(east[index], Vec2::new(0.0, sample_t));
                let diff_east = (lhs_east - rhs_east).abs();
                let wrap_east = diff_east.min(Vec2::ONE - diff_east);
                assert!(
                    wrap_east.x < 1e-5 && wrap_east.y < 1e-5,
                    "east seam mismatch on layer {index}: {lhs_east:?} vs {rhs_east:?}"
                );

                let lhs_south = eval_base_uv(base[index], Vec2::new(sample_t, 64.0));
                let rhs_south = eval_base_uv(south[index], Vec2::new(sample_t, 0.0));
                let diff_south = (lhs_south - rhs_south).abs();
                let wrap_south = diff_south.min(Vec2::ONE - diff_south);
                assert!(
                    wrap_south.x < 1e-5 && wrap_south.y < 1e-5,
                    "south seam mismatch on layer {index}: {lhs_south:?} vs {rhs_south:?}"
                );
            }
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
