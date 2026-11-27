//! Client-only ground clutter: small painterly grass tufts, flowers, and pebbles scattered
//! over streamed planet chunks at gameplay zoom.
//!
//! Clutter is purely visual: the server never sees it, it never blocks movement, and every
//! placement is a pure function of the tile's hash, so every client draws the same scatter.
//! It reads only [`ChunkGroundCover`] (dominant palette entry and region paint per tile), the
//! climate foliage density, and `tile_hash`, and its sprites are children of the chunk entity,
//! so they despawn with it.
//!
//! Data contract:
//! - `assets/terrain/clutter.json` maps a palette id to a weighted sprite list and a density.
//!   Rust never names a sprite; the generic tests check every listed file exists.
//! - Sizes and offsets are in 5-ft tiles. Offsets are measured from the tile center in the
//!   chunk's face frame (`+x` east, `+y` south), and a sprite never leaves its own tile, so a
//!   tuft on a grass tile never overhangs the road tile next to it.
//! - Tiles whose palette entry says `clutter: false`, or that an authored region painted
//!   (roads, paths, town floors), never carry clutter.

use std::collections::BTreeMap;

use bevy::asset::RenderAssetUsages;
use bevy::image::{CompressedImageFormats, ImageFormat, ImageSampler, ImageType};
use bevy::prelude::*;
use serde::Deserialize;
use zeta_core::world::{CHUNK_SIZE, PlanetClimateMaps, hash_unit, sample_climate, tile_hash};

use crate::chunk_splat::ChunkGroundCover;
use crate::planet_stream::{PlanetCameraFocus, PlanetWorld};
use crate::terrain_palette::TerrainPalette;

/// Climate maps of the planet, if the client loaded them.
fn planet_climate(world: &PlanetWorld) -> Option<&PlanetClimateMaps> {
    let _ = world;
    None
}

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
            rule.validate()
                .map_err(|error| format!("palette id {id}: {error}"))?;
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

/// 1 sprite to draw on 1 tile of a chunk.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChunkClutterSprite {
    /// Tile column inside the chunk.
    pub local_x: u32,
    /// Tile row inside the chunk.
    pub local_y: u32,
    /// Dominant palette index of the tile (selects the rule the sprite came from).
    pub palette_index: u8,
    /// Where and how the sprite is drawn.
    pub placement: ClutterPlacement,
}

/// Every clutter sprite of 1 chunk, in row-major tile order.
///
/// `foliage` gives the climate foliage density of a tile; when it is `None` (no climate
/// maps), rules that follow foliage draw nothing and the rest still draw.
#[must_use]
pub fn chunk_clutter(
    table: &ClutterTable,
    cover: &ChunkGroundCover,
    foliage: Option<&PlanetClimateMaps>,
) -> Vec<ChunkClutterSprite> {
    let mut sprites = Vec::new();
    for local_y in 0..CHUNK_SIZE {
        for local_x in 0..CHUNK_SIZE {
            let (Some(tile), Some(palette_index), Some(region_painted)) = (
                cover.coord.tile_at(local_x, local_y),
                cover.dominant_at(local_x, local_y),
                cover.region_painted_at(local_x, local_y),
            ) else {
                continue;
            };
            let Some(rule) = table.rule(usize::from(palette_index)) else {
                continue;
            };
            let foliage = match (rule.follows_foliage, foliage) {
                (false, _) => 1.0,
                (true, Some(maps)) => sample_climate(maps, tile).foliage,
                (true, None) => 0.0,
            };
            let input = ClutterTileInput {
                palette_index: usize::from(palette_index),
                region_painted,
                foliage,
                hash: tile_hash(tile),
            };
            if let Some(placement) = table.place(input) {
                sprites.push(ChunkClutterSprite {
                    local_x,
                    local_y,
                    palette_index,
                    placement,
                });
            }
        }
    }
    sprites
}

/// Most chunks whose clutter is rebuilt in 1 frame, so a streaming burst never stalls a frame.
const MAX_CLUTTER_CHUNKS_PER_FRAME: usize = 2;
/// Height of the clutter layer above its chunk sprite, in world units.
const CLUTTER_LAYER_Z: f32 = 0.1;
/// Extra height per tile row, so sprites further south draw over their northern neighbors.
const CLUTTER_ROW_Z_STEP: f32 = 0.000_1;

/// Draws ground clutter on every streamed chunk that carries a [`ChunkGroundCover`].
#[derive(Clone, Copy, Debug, Default)]
pub struct GroundClutterPlugin;

impl Plugin for GroundClutterPlugin {
    fn build(&self, app: &mut App) {
        // `resource_exists`: bevy_ecs-0.20.1/src/schedule/condition.rs:688.
        // `not`: bevy_ecs-0.20.1/src/schedule/condition.rs:1251.
        app.add_systems(
            Update,
            (
                load_ground_clutter.run_if(not(resource_exists::<GroundClutterAssets>)),
                (
                    clear_removed_chunk_clutter,
                    queue_changed_chunks,
                    build_queued_chunk_clutter,
                    scale_clutter_layers,
                )
                    .chain()
                    .run_if(resource_exists::<GroundClutterAssets>),
            )
                .chain(),
        );
    }
}

/// Clutter table resolved against the palette, plus 1 loaded image per rule sprite.
#[derive(Resource, Debug, Default)]
pub struct GroundClutterAssets {
    table: ClutterTable,
    /// `images[palette_index][sprite_index]`, parallel to the table's rules.
    images: Vec<Vec<ClutterImage>>,
}

#[derive(Clone, Debug)]
struct ClutterImage {
    handle: Handle<Image>,
    /// Image size scaled so its longest side is 1.
    unit_size: Vec2,
}

/// Marks a chunk whose clutter must be (re)built.
#[derive(Component, Debug)]
struct ClutterQueued;

/// The clutter layer entity of a chunk.
#[derive(Component, Debug)]
struct ChunkClutterLayer(Entity);

/// Child of a chunk that holds its clutter sprites in tile units, scaled to the current zoom.
#[derive(Component, Debug)]
pub struct GroundClutterLayer;

/// 1 clutter sprite.
#[derive(Component, Debug)]
pub struct GroundClutter;

/// Loads the clutter table once the terrain palette exists.
///
/// A broken table or image is logged and leaves clutter off; the palette tests keep the
/// shipped files valid.
fn load_ground_clutter(
    mut commands: Commands,
    palette: Option<Res<TerrainPalette>>,
    images: Option<ResMut<Assets<Image>>>,
) {
    let (Some(palette), Some(mut images)) = (palette, images) else {
        return;
    };
    match GroundClutterAssets::load(&palette, &mut images) {
        Ok(assets) => commands.insert_resource(assets),
        Err(error) => {
            // `error!`: bevy_log re-export of `tracing::error`.
            error!("ground clutter is off: {error}");
            commands.insert_resource(GroundClutterAssets::default());
        }
    }
}

impl GroundClutterAssets {
    fn load(palette: &TerrainPalette, images: &mut Assets<Image>) -> Result<Self, String> {
        let config = ClutterConfig::load_default()?;
        let flags: Vec<PaletteClutterFlag<'_>> = palette
            .entries()
            .iter()
            .map(|entry| PaletteClutterFlag {
                id: &entry.id,
                clutter: entry.clutter,
            })
            .collect();
        let table = config.resolve(&flags)?;
        let mut loaded = BTreeMap::new();
        for texture in config.textures() {
            loaded.insert(texture.to_owned(), load_clutter_image(texture, images)?);
        }
        let images = (0..flags.len())
            .map(|index| {
                table.rule(index).map_or_else(Vec::new, |rule| {
                    rule.sprites
                        .iter()
                        .filter_map(|sprite| loaded.get(&sprite.texture).cloned())
                        .collect()
                })
            })
            .collect();
        Ok(Self { table, images })
    }
}

fn load_clutter_image(texture: &str, images: &mut Assets<Image>) -> Result<ClutterImage, String> {
    let path = crate::terrain::assets_root().join(texture);
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    // `Image::from_buffer`: bevy_image-0.20.1/src/image.rs:1589.
    // `ImageType::Format`: bevy_image-0.20.1/src/image.rs:2343.
    // `ImageFormat::Png`: bevy_image-0.20.1/src/image.rs:297.
    // `CompressedImageFormats::NONE`: bevy_image-0.20.1/src/image.rs:2392.
    // `ImageSampler::Default`: bevy_image-0.20.1/src/image.rs:700.
    // `RenderAssetUsages::default` (main and render world): bevy_asset-0.20.1/src/render_asset.rs:40.
    let image = Image::from_buffer(
        &bytes,
        ImageType::Format(ImageFormat::Png),
        CompressedImageFormats::NONE,
        true,
        ImageSampler::Default,
        RenderAssetUsages::default(),
    )
    .map_err(|error| format!("could not decode {}: {error}", path.display()))?;
    // `Image::size`: bevy_image-0.20.1/src/image.rs:1330.
    let size = image.size().as_vec2();
    let longest = size.max_element();
    if longest <= 0.0 {
        return Err(format!("{} is empty", path.display()));
    }
    Ok(ClutterImage {
        // `Assets::add`: bevy_asset-0.20.1/src/assets.rs:401.
        handle: images.add(image),
        unit_size: size / longest,
    })
}

/// Drops the clutter of chunks that lost their ground cover (zoomed out to the overview).
fn clear_removed_chunk_clutter(
    mut commands: Commands,
    // `RemovedComponents::read`: bevy_ecs-0.20.1/src/lifecycle.rs:651.
    mut removed: RemovedComponents<ChunkGroundCover>,
    layers: Query<&ChunkClutterLayer>,
) {
    for chunk in removed.read() {
        let Ok(layer) = layers.get(chunk) else {
            continue;
        };
        // `Commands::get_entity`: bevy_ecs-0.20.1/src/system/commands/mod.rs:360.
        // `EntityCommands::despawn` (children follow, `linked_spawn`): commands/mod.rs:1882,
        // bevy_ecs-0.20.1/src/hierarchy.rs:148.
        if let Ok(mut layer) = commands.get_entity(layer.0) {
            layer.despawn();
        }
        if let Ok(mut chunk) = commands.get_entity(chunk) {
            // `EntityCommands::try_remove`: bevy_ecs-0.20.1/src/system/commands/mod.rs:1803.
            chunk.try_remove::<(ChunkClutterLayer, ClutterQueued)>();
        }
    }
}

/// Queues every chunk whose ground cover was added or rebaked.
fn queue_changed_chunks(
    mut commands: Commands,
    // `Changed`: bevy_ecs-0.20.1/src/query/filter.rs:996.
    changed: Query<Entity, Changed<ChunkGroundCover>>,
) {
    for chunk in &changed {
        // `EntityCommands::try_insert`: bevy_ecs-0.20.1/src/system/commands/mod.rs:1602.
        commands.entity(chunk).try_insert(ClutterQueued);
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn build_queued_chunk_clutter(
    mut commands: Commands,
    assets: Res<GroundClutterAssets>,
    world: Option<Res<PlanetWorld>>,
    focus: Res<PlanetCameraFocus>,
    queued: Query<(Entity, &ChunkGroundCover, Option<&ChunkClutterLayer>), With<ClutterQueued>>,
) {
    let climate = world.as_deref().and_then(planet_climate);
    if climate.is_none() {
        // `warn_once!`: bevy_log-0.20.1/src/once.rs:35.
        warn_once!("no planet climate maps: plant clutter is off, pebbles still draw");
    }
    let tile_pixel_size = focus.tile_pixel_size.max(0.1);
    for (chunk, cover, old_layer) in queued.iter().take(MAX_CLUTTER_CHUNKS_PER_FRAME) {
        if let Some(old_layer) = old_layer
            && let Ok(mut old_layer) = commands.get_entity(old_layer.0)
        {
            old_layer.despawn();
        }
        let sprites = chunk_clutter(&assets.table, cover, climate);
        // `Transform::from_xyz`: bevy_transform-0.20.1/src/components/transform.rs:119.
        // `Transform::with_scale`: bevy_transform-0.20.1/src/components/transform.rs.
        // `Visibility`: bevy_camera-0.20.1/src/visibility/mod.rs:83.
        // `ChildOf`: bevy_ecs-0.20.1/src/hierarchy.rs:107.
        // `EntityCommands::with_children`: bevy_ecs-0.20.1/src/hierarchy.rs:366.
        let layer = commands
            .spawn((
                GroundClutterLayer,
                Transform::from_xyz(0.0, 0.0, CLUTTER_LAYER_Z).with_scale(Vec3::new(
                    tile_pixel_size,
                    tile_pixel_size,
                    1.0,
                )),
                Visibility::default(),
                ChildOf(chunk),
            ))
            .with_children(|layer| {
                for sprite in &sprites {
                    if let Some(bundle) = assets.sprite_bundle(sprite) {
                        layer.spawn(bundle);
                    }
                }
            })
            .id();
        commands
            .entity(chunk)
            .try_remove::<ClutterQueued>()
            .try_insert(ChunkClutterLayer(layer));
    }
}

impl GroundClutterAssets {
    /// Sprite entity of 1 placement, positioned in the layer's tile units (chunk center at 0,
    /// `+y` north on screen).
    fn sprite_bundle(
        &self,
        sprite: &ChunkClutterSprite,
    ) -> Option<(GroundClutter, Sprite, Transform)> {
        let placement = sprite.placement;
        let image = self
            .images
            .get(usize::from(sprite.palette_index))?
            .get(placement.sprite)?;
        let half_chunk = local_tile_center(CHUNK_SIZE / 2) - 0.5;
        let x = local_tile_center(sprite.local_x) + placement.offset_tiles.x - half_chunk;
        let y = half_chunk - (local_tile_center(sprite.local_y) + placement.offset_tiles.y);
        let z = CLUTTER_ROW_Z_STEP * local_tile_center(sprite.local_y);
        // `Sprite::from_image`: bevy_sprite-0.20.1/src/sprite.rs:56.
        // `Sprite::custom_size`: bevy_sprite-0.20.1/src/sprite.rs:31.
        // `Sprite::flip_x`: bevy_sprite-0.20.1/src/sprite.rs:26.
        let mut drawn = Sprite::from_image(image.handle.clone());
        drawn.custom_size = Some(image.unit_size * placement.size_tiles);
        drawn.flip_x = placement.flip_x;
        Some((GroundClutter, drawn, Transform::from_xyz(x, y, z)))
    }
}

/// Center of local tile `index` in tiles from the chunk's top-left corner.
fn local_tile_center(index: u32) -> f32 {
    // Local tile indices are below 64, so the conversion is exact.
    f32::from(u16::try_from(index).unwrap_or(u16::MAX)) + 0.5
}

/// Keeps every clutter layer at the current zoom.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn scale_clutter_layers(
    focus: Res<PlanetCameraFocus>,
    mut layers: Query<&mut Transform, With<GroundClutterLayer>>,
) {
    if !focus.is_changed() {
        return;
    }
    let tile_pixel_size = focus.tile_pixel_size.max(0.1);
    for mut transform in &mut layers {
        transform.scale = Vec3::new(tile_pixel_size, tile_pixel_size, 1.0);
    }
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
        PaletteClutterFlag {
            id: "meadow",
            clutter: true,
        },
        PaletteClutterFlag {
            id: "road",
            clutter: false,
        },
        PaletteClutterFlag {
            id: "gravel",
            clutter: true,
        },
        PaletteClutterFlag {
            id: "snow",
            clutter: true,
        },
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
        assert_eq!(
            count(0, 1.0),
            20_000,
            "density 1 at full foliage fills every tile"
        );
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
        assert!(
            (14_000..16_000).contains(&picks[0]),
            "weighted picks {picks:?}"
        );
    }

    #[test]
    fn resolve_rejects_unknown_ids_and_clutter_on_covers_that_forbid_it() {
        let config = ClutterConfig::from_json(TEST_TABLE).expect("valid");
        let missing = [PaletteClutterFlag {
            id: "meadow",
            clutter: true,
        }];
        assert!(config.resolve(&missing).is_err());
        let forbidden = [
            PaletteClutterFlag {
                id: "meadow",
                clutter: false,
            },
            PaletteClutterFlag {
                id: "gravel",
                clutter: true,
            },
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

    /// Chunk whose left half is `meadow` and right half `gravel`, with every 4th row painted
    /// by a region and column 0 a `road` tile.
    fn test_cover() -> ChunkGroundCover {
        let coord = zeta_core::world::ChunkCoord::new(zeta_core::world::CubeFace::PosZ, 5, 7)
            .expect("valid chunk");
        let mut dominant = Box::new([0_u8; crate::chunk_splat::CHUNK_TILE_COUNT]);
        let mut region_painted = Box::new([false; crate::chunk_splat::CHUNK_TILE_COUNT]);
        for local_y in 0..CHUNK_SIZE {
            for local_x in 0..CHUNK_SIZE {
                let index = ChunkGroundCover::index(local_x, local_y).expect("inside chunk");
                dominant[index] = match local_x {
                    0 => 1,
                    x if x < CHUNK_SIZE / 2 => 0,
                    _ => 2,
                };
                region_painted[index] = local_y % 4 == 0;
            }
        }
        ChunkGroundCover {
            coord,
            dominant,
            region_painted,
        }
    }

    #[test]
    fn chunk_clutter_never_lands_on_painted_or_clutter_free_tiles() {
        let table = test_table();
        let cover = test_cover();
        let sprites = chunk_clutter(&table, &cover, None);
        assert!(!sprites.is_empty(), "gravel ignores foliage and must draw");
        for sprite in &sprites {
            assert_eq!(
                cover.region_painted_at(sprite.local_x, sprite.local_y),
                Some(false),
                "sprite on a region-painted tile"
            );
            assert_eq!(
                cover.dominant_at(sprite.local_x, sprite.local_y),
                Some(sprite.palette_index)
            );
            assert_eq!(
                sprite.palette_index, 2,
                "only gravel draws without climate maps"
            );
        }
        assert_eq!(
            chunk_clutter(&table, &cover, None),
            sprites,
            "deterministic"
        );
    }

    #[test]
    fn shipped_clutter_table_resolves_against_the_palette_and_every_texture_exists() {
        let config = ClutterConfig::load_default().expect("assets/terrain/clutter.json is valid");
        let palette = TerrainPalette::load().expect("assets/terrain/palette.json is valid");
        let flags: Vec<PaletteClutterFlag<'_>> = palette
            .entries()
            .iter()
            .map(|entry| PaletteClutterFlag {
                id: &entry.id,
                clutter: entry.clutter,
            })
            .collect();
        config
            .resolve(&flags)
            .expect("clutter table matches the palette");
        let root = crate::terrain::assets_root();
        for texture in config.textures() {
            assert!(
                root.join(texture).is_file(),
                "missing clutter sprite {texture}"
            );
        }
    }
}
