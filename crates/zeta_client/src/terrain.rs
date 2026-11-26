//! GPU terrain splatmap material and splatmap generator.
//!
//! Blends seamless repeating ground textures (`grass`, `dirt`, `rock`, `water`) on the GPU
//! using a soft-feathered RGBA splatmap generated from the replicated map layout.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Extent3d, TextureDimension, TextureFormat};
use bevy::shader::ShaderRef;
use bevy::sprite_render::Material2d;
use zeta_core::grid::{GridPos, MapBounds};

/// Pixels of splatmap resolution per 5-foot map square.
const SPLAT_PIXELS_PER_TILE: u32 = 8;
/// How many 5-foot squares 1 repeat of a seamless terrain texture spans.
const TILES_PER_TEXTURE_REPEAT: f32 = 4.5;
/// Opacity of the subtle 5-foot VTT grid lines drawn in the terrain shader.
const GRID_LINE_ALPHA: f32 = 0.12;

/// GPU material that blends 4 seamless terrain textures through a soft RGBA splatmap.
// `Material2d` + `AsBindGroup`: bevy_sprite_render-0.20.1/src/mesh2d/material.rs:148.
#[derive(AsBindGroup, Debug, Clone, Asset, TypePath)]
pub struct TerrainSplatMaterial {
    /// `(grid_width, grid_height, tiles_per_texture_repeat, grid_line_alpha)`.
    #[uniform(0)]
    pub params: Vec4,
    /// RGBA brush weights: `R = dirt`, `G = bank/rock`, `B = water`, `A = wet-shore shadow`.
    #[texture(1)]
    #[sampler(2)]
    pub splat: Handle<Image>,
    /// Base grass groundcover texture (repeating).
    #[texture(3)]
    #[sampler(4)]
    pub grass: Handle<Image>,
    /// Dirt road / path texture (repeating).
    #[texture(5)]
    #[sampler(6)]
    pub dirt: Handle<Image>,
    /// Rocky riverbank texture (repeating).
    #[texture(7)]
    #[sampler(8)]
    pub rock: Handle<Image>,
    /// River water texture (repeating).
    #[texture(9)]
    #[sampler(10)]
    pub water: Handle<Image>,
}

impl Material2d for TerrainSplatMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/terrain_splat.wgsl".into()
    }
}

/// Configures `AssetPlugin` to load from the workspace `assets/` directory whether launched
/// through `cargo run -p zeta_client` (where `CARGO_MANIFEST_DIR` is `crates/zeta_client`)
/// or as a standalone binary beside `assets/`.
pub fn client_asset_plugin() -> AssetPlugin {
    let workspace_assets = std::path::Path::new("assets");
    let file_path = if workspace_assets.is_dir() {
        std::fs::canonicalize(workspace_assets)
            .unwrap_or_else(|_| workspace_assets.to_path_buf())
            .to_string_lossy()
            .into_owned()
    } else {
        "assets".to_owned()
    };
    AssetPlugin {
        file_path,
        ..default()
    }
}

/// Loads a seamless terrain texture configured with repeating UV addressing.
pub fn load_repeating_texture(asset_server: &AssetServer, path: &'static str) -> Handle<Image> {
    // `AssetServer::load_builder`: bevy_asset-0.20.1/src/server/mod.rs:371.
    // `LoadBuilder::with_settings`: bevy_asset-0.20.1/src/server/mod.rs:1737.
    // `ImageLoaderSettings::sampler`: bevy_image-0.20.1/src/image_loader.rs:151.
    // `ImageAddressMode::Repeat`: bevy_image-0.20.1/src/image.rs:747.
    asset_server
        .load_builder()
        .with_settings(|settings: &mut ImageLoaderSettings| {
            settings.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                address_mode_u: ImageAddressMode::Repeat,
                address_mode_v: ImageAddressMode::Repeat,
                ..ImageSamplerDescriptor::linear()
            });
        })
        .load(path)
}

/// Converts a small map or pixel count to `f32`.
#[expect(
    clippy::cast_precision_loss,
    reason = "map dimensions and splatmap pixel counts stay far below 2^23"
)]
fn count_to_f32(value: u32) -> f32 {
    value as f32
}

/// Converts a `[0.0, 1.0]` weight to a `0..=255` byte.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is clamped to [0.0, 1.0] before scaling to 0..=255"
)]
fn weight_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0) as u8
}

/// Uniform parameters for a `MapBounds` terrain quad.
pub fn terrain_params(bounds: MapBounds) -> Vec4 {
    Vec4::new(
        count_to_f32(bounds.width),
        count_to_f32(bounds.height),
        TILES_PER_TEXTURE_REPEAT,
        GRID_LINE_ALPHA,
    )
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Smooth deterministic pseudo-noise in `[-1.0, 1.0]` for organic brush edges.
fn organic_wave(x: f32, y: f32) -> f32 {
    (x * 0.63 + (y * 0.41).sin() * 1.4).sin() * 0.55
        + (y * 0.87 - (x * 0.52).cos() * 1.1).cos() * 0.45
}

/// Generates an RGBA8 linear splatmap image for `bounds` from the map's water and bridge
/// squares.
///
/// Channels:
/// - `R`: dirt road leading across any bridge squares (plus light meadow patches when no
///   bridge exists)
/// - `G`: muddy/rocky riverbank brushed around water squares
/// - `B`: river water brushed over water squares
/// - `A`: wet-shore darkening rim right outside water squares
pub fn build_splatmap_image(
    bounds: MapBounds,
    water_squares: &[GridPos],
    bridge_squares: &[GridPos],
) -> Image {
    let width = (bounds.width.max(1)) * SPLAT_PIXELS_PER_TILE;
    let height = (bounds.height.max(1)) * SPLAT_PIXELS_PER_TILE;
    let mut data = vec![0_u8; (width * height * 4) as usize];

    let scale = count_to_f32(SPLAT_PIXELS_PER_TILE);
    for py in 0..height {
        let tile_y = (count_to_f32(py) + 0.5) / scale;
        for px in 0..width {
            let tile_x = (count_to_f32(px) + 0.5) / scale;
            let wobble = organic_wave(tile_x, tile_y);

            // Nearest distance (in tiles) to any water square center.
            let water_dist = water_squares
                .iter()
                .map(|pos| {
                    let dx = tile_x - (count_to_f32(pos.x) + 0.5);
                    let dy = tile_y - (count_to_f32(pos.y) + 0.5);
                    dx.hypot(dy)
                })
                .fold(f32::INFINITY, f32::min);

            // Distance to a winding east-west dirt road passing through any bridge row.
            let road_dist = bridge_squares
                .iter()
                .map(|pos| {
                    let road_y =
                        (count_to_f32(pos.y) + 0.5) + 0.45 * (tile_x * 0.35).sin() + 0.20 * wobble;
                    (tile_y - road_y).abs()
                })
                .fold(f32::INFINITY, f32::min);

            let water_w = if water_squares.is_empty() {
                0.0
            } else {
                1.0 - smoothstep(0.55, 0.92, water_dist + 0.14 * wobble)
            };
            let bank_w = if water_squares.is_empty() {
                0.0
            } else {
                (1.0 - smoothstep(0.85, 1.65, water_dist + 0.22 * wobble)) * (1.0 - water_w * 0.5)
            };
            let wet_rim = if water_squares.is_empty() {
                0.0
            } else {
                smoothstep(0.55, 0.88, water_dist) * (1.0 - smoothstep(0.88, 1.35, water_dist))
            };
            let dirt_w = if bridge_squares.is_empty() {
                // Subtle organic earth patches on plain meadows.
                smoothstep(0.45, 0.95, wobble) * 0.22
            } else {
                1.0 - smoothstep(0.38, 0.92, road_dist)
            };

            let index = ((py * width + px) * 4) as usize;
            data[index] = weight_to_u8(dirt_w);
            data[index + 1] = weight_to_u8(bank_w);
            data[index + 2] = weight_to_u8(water_w);
            data[index + 3] = weight_to_u8(wet_rim);
        }
    }

    // `Image::new`: bevy_image-0.20.1/src/image.rs:1123.
    let mut image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor::linear());
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splatmap_paints_water_over_river_and_dirt_along_bridge_road() {
        let bounds = MapBounds {
            width: 10,
            height: 6,
        };
        let water: Vec<GridPos> = (0..6)
            .filter(|&y| y != 3)
            .map(|y| GridPos::new(5, y))
            .collect();
        let bridge = [GridPos::new(5, 3)];
        let image = build_splatmap_image(bounds, &water, &bridge);
        let data = image.data.as_ref().expect("splatmap has pixel bytes");
        let width = bounds.width * SPLAT_PIXELS_PER_TILE;

        let sample = |tile_x: u32, tile_y: u32| -> [u8; 4] {
            let px = tile_x * SPLAT_PIXELS_PER_TILE + SPLAT_PIXELS_PER_TILE / 2;
            let py = tile_y * SPLAT_PIXELS_PER_TILE + SPLAT_PIXELS_PER_TILE / 2;
            let i = ((py * width + px) * 4) as usize;
            [data[i], data[i + 1], data[i + 2], data[i + 3]]
        };

        // River tile has high water weight (blue channel).
        assert!(sample(5, 1)[2] > 200);
        // Far meadow tile has zero water.
        assert_eq!(sample(1, 0)[2], 0);
        // Tile on the road leading to the bridge has strong dirt weight (red channel).
        assert!(sample(2, 3)[0] > 140);
    }
}
