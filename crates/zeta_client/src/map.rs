//! 2D grid map: a camera, authored region terrain splatmaps over Sera's planetary chunks,
//! a sprite or colored square for every entity with a grid position, and a label with its
//! name and Tag badges.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 5 (independent plugin over
//! replicated components), `docs/architecture/world_geography.md` Sections 1–4, and
//! `docs/prototypes/realism_prototype.md`.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use bevy::sprite::Anchor;
use bevy::sprite_render::{Material2dPlugin, MeshMaterial2d};
// `RequestRedraw`: bevy_window-0.20.1/src/event.rs:61.
use bevy::window::RequestRedraw;
use zeta_core::entity::EntityId;
use zeta_core::grid::{GridPos, MapBounds};
use zeta_core::protocol::{Collider, Creature, EntityName, MapArea, PlayerCharacter, Tags};
use zeta_core::world::{CHUNK_SIZE, ChunkCoord, CubeFace, FACE_TILES, FaceRotation};

use crate::layout::{FloatingOrigin, map_center, square_center_at, tile_point_at};
use crate::planet_region::{LoadedAuthoredRegions, load_authored_regions, world_regions_dir};
use crate::planet_stream::{PlanetCameraFocus, PlanetFocusSync, PlanetRegions};
use crate::player::{LocalPlayer, LocalPlayerUpdate};
use crate::settings::{
    ClientSettings, GRID_DARK_COLOR, GRID_LIGHT_COLOR, IMPASSABLE_COLOR, LOCAL_PLAYER_COLOR,
    OTHER_ENTITY_COLOR, PLAYER_CHARACTER_COLOR,
};
use crate::terrain::{
    DEFAULT_MAP_CONFIG, LoadedMapConfig, TerrainEntityCell, TerrainSplatMaterial,
    build_splatmap_from_config, entity_matches_any_terrain_layer, full_region_mask_image,
    load_map_config, terrain_params,
};

/// Draw order of grid squares and authored region splatmaps (above planetary chunks at `-1.0`).
const SQUARE_DEPTH: f32 = 0.0;
/// Draw order of entities, above the terrain.
const ENTITY_DEPTH: f32 = 1.0;
/// Draw order of a label relative to its entity.
const LABEL_DEPTH: f32 = 1.0;
/// Share of a square an entity's colored square covers, so neighbors stay apart.
const ENTITY_SCALE: f32 = 0.8;
/// Font size of entity labels relative to the square size.
const LABEL_FONT_SCALE: f32 = 0.35;
/// Cube face currently hosting playable grid coordinates.
const DEFAULT_PLANET_FACE: CubeFace = CubeFace::PosX;
/// Streaming ring half-width used during normal gameplay (`5x5` chunks = `320x320` tiles).
const GAME_STREAM_RADIUS_CHUNKS: i32 = 2;
/// Maximum map dimension for spawning individual fallback `MapSquare` sprite entities in
/// headless unit tests; planetary maps (`2^21`) must never spawn per-tile entities.
const MAX_FALLBACK_GRID_DIMENSION: u32 = 64;
/// Margin in 5-foot squares around [`FloatingOrigin`] within which authored regions stay mounted.
const REGION_VIEW_MARGIN_TILES: f64 = 96.0;
/// Exponential damping rate (per second) for entity visual gliding between discrete 5-ft squares.
const ENTITY_GLIDE_RATE: f64 = 16.0;
/// Exponential damping rate (per second) for smooth camera follow behind the local character.
const CAMERA_FOLLOW_RATE: f64 = 6.5;
/// Distance in 5-ft squares above which camera or entity visuals snap instead of gliding (e.g. spawn/teleport).
const SNAP_DISTANCE_TILES: f64 = 16.0;
/// Remaining distance in 5-ft squares below which damping settles cleanly (`0.002 * 32px ≈ 0.06px`).
const SETTLE_EPSILON_TILES: f64 = 0.002;

/// What deciding an entity's visual reads.
type VisualQueryData = (
    Option<&'static EntityId>,
    &'static GridPos,
    Has<PlayerCharacter>,
    Option<&'static Collider>,
    Option<&'static EntityName>,
    Option<&'static Tags>,
    &'static mut Sprite,
    &'static mut Transform,
);

/// Marks a GPU terrain splatmap quad.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerrainMesh;

/// Marks a mounted GPU terrain quad belonging to 1 authored planetary region.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct AuthoredRegionMesh {
    /// Identifier of the authored region rendered by this mesh.
    pub region_id: String,
}

/// Marks the camera that shows the map.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MapCamera;

/// Marks 1 drawn fallback square of a small headless map area.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapSquare(pub GridPos);

/// Marks the label below an entity, showing its name and Tag badges.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MapLabel;

/// Added to an entity once the map draws it: the entity of its label.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapLabelEntity(pub Entity);

/// Smoothly interpolated visual center of a drawn entity in 5-foot tile coordinates (`x + 0.5, y + 0.5`).
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct VisualTilePos {
    /// Continuous horizontal tile coordinate.
    pub tile_x: f64,
    /// Continuous vertical tile coordinate.
    pub tile_y: f64,
}

impl VisualTilePos {
    /// Visual center of discrete square `position`.
    #[must_use]
    pub fn from_grid(position: GridPos) -> Self {
        Self {
            tile_x: f64::from(position.x) + 0.5,
            tile_y: f64::from(position.y) + 0.5,
        }
    }
}

/// Draws the map area, authored planetary regions, entities with a grid position, and their labels.
#[derive(Clone, Copy, Debug, Default)]
pub struct MapPlugin;

impl Plugin for MapPlugin {
    fn build(&self, app: &mut App) {
        // `App::is_plugin_added`: bevy_app-0.20.1/src/app.rs:570.
        if app.is_plugin_added::<AssetPlugin>() {
            app.add_plugins(Material2dPlugin::<TerrainSplatMaterial>::default())
                .add_systems(Startup, load_authored_regions_startup);
        }
        app.init_resource::<ClientSettings>()
            .init_resource::<LocalPlayer>()
            .init_resource::<FloatingOrigin>()
            .add_message::<RequestRedraw>()
            .add_systems(Startup, spawn_camera)
            .add_systems(
                Update,
                (
                    attach_entity_visuals,
                    glide_entity_visual_positions,
                    sync_floating_origin,
                )
                    .chain()
                    .in_set(PlanetFocusSync)
                    .after(LocalPlayerUpdate),
            )
            .add_systems(
                Update,
                (
                    draw_map_area,
                    move_entity_visuals,
                    style_entities,
                    update_labels,
                )
                    .chain()
                    .after(PlanetFocusSync),
            );
    }
}

/// The color an entity is drawn in.
pub fn entity_color(is_player_character: bool, is_local_player: bool, passable: bool) -> Color {
    if is_local_player {
        LOCAL_PLAYER_COLOR
    } else if is_player_character {
        PLAYER_CHARACTER_COLOR
    } else if passable {
        OTHER_ENTITY_COLOR
    } else {
        IMPASSABLE_COLOR
    }
}

/// Whether an entity is an alive creature (gets a floating name label on the map).
#[must_use]
pub fn is_alive_creature(is_creature: bool, is_player: bool, tags: Option<&Tags>) -> bool {
    let is_dead = tags.is_some_and(|t| t.0.iter().any(|tag| tag.eq_ignore_ascii_case("dead")));
    (is_creature || is_player) && !is_dead
}

/// The label text of an entity: its name, then its Tags as bracketed badges.
pub fn label_text(name: Option<&EntityName>, tags: Option<&Tags>) -> String {
    let name = name.map_or("", |name| name.0.as_str());
    let badges: Vec<String> = tags.map_or_else(Vec::new, |tags| {
        tags.0.iter().map(|tag| format!("[{tag}]")).collect()
    });
    if badges.is_empty() {
        name.to_owned()
    } else {
        format!("{name}\n{}", badges.join(" "))
    }
}

fn spawn_camera(mut commands: Commands, cameras: Query<(), With<Camera>>) {
    if cameras.is_empty() {
        commands.spawn((MapCamera, Camera2d));
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn load_authored_regions_startup(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut images: ResMut<Assets<Image>>,
    mut planet_regions: Option<ResMut<PlanetRegions>>,
) {
    match load_authored_regions(&asset_server, &mut images, &world_regions_dir()) {
        Ok(loaded) => {
            if let Ok(validated) = PlanetRegions::new(loaded.footprints()) {
                if let Some(existing) = planet_regions.as_mut() {
                    **existing = validated;
                } else {
                    commands.insert_resource(validated);
                }
            }
            commands.insert_resource(loaded);
        }
        Err(error) => {
            warn!("authored regions not loaded ({error}); falling back to default map config");
            match load_map_config(&asset_server, DEFAULT_MAP_CONFIG) {
                Ok(loaded) => commands.insert_resource(loaded),
                Err(fallback_error) => {
                    warn!("map visual config not loaded ({fallback_error}); using fallback grid");
                }
            }
        }
    }
}

/// Frame-rate-independent exponential damping in 5-ft tile coordinates.
///
/// Snaps immediately when `dt <= 0.0` (headless tests / initial frame) or when distance exceeds
/// [`SNAP_DISTANCE_TILES`] (initial spawn or teleport). Returns `(next_pos, still_moving)`.
#[must_use]
pub fn damp_tile_towards(
    current: (f64, f64),
    target: (f64, f64),
    rate: f64,
    dt: f64,
) -> ((f64, f64), bool) {
    let dx = target.0 - current.0;
    let dy = target.1 - current.1;
    let dist_sq = dx * dx + dy * dy;
    if dist_sq <= SETTLE_EPSILON_TILES * SETTLE_EPSILON_TILES {
        return (target, false);
    }
    if dt <= 0.0 || dist_sq > SNAP_DISTANCE_TILES * SNAP_DISTANCE_TILES {
        return (target, false);
    }
    let alpha = 1.0 - (-rate * dt).exp();
    let next = (current.0 + dx * alpha, current.1 + dy * alpha);
    let remaining_x = target.0 - next.0;
    let remaining_y = target.1 - next.1;
    if remaining_x * remaining_x + remaining_y * remaining_y
        <= SETTLE_EPSILON_TILES * SETTLE_EPSILON_TILES
    {
        (target, false)
    } else {
        (next, true)
    }
}

/// Smoothly glides each entity's [`VisualTilePos`] toward its authoritative discrete [`GridPos`].
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn glide_entity_visual_positions(
    time: Option<Res<Time>>,
    mut entities: Query<(&GridPos, &mut VisualTilePos), With<MapLabelEntity>>,
    mut redraws: MessageWriter<RequestRedraw>,
) {
    // `Time::delta_secs`: bevy_time-0.20.1/src/time.rs:283.
    let dt = time.map_or(0.0, |t| f64::from(t.delta_secs()));
    let mut animating = false;
    for (position, mut visual) in &mut entities {
        let target = (f64::from(position.x) + 0.5, f64::from(position.y) + 0.5);
        let current = (visual.tile_x, visual.tile_y);
        if current == target {
            continue;
        }
        let ((next_x, next_y), still_moving) =
            damp_tile_towards(current, target, ENTITY_GLIDE_RATE, dt);
        visual.tile_x = next_x;
        visual.tile_y = next_y;
        animating |= still_moving;
    }
    if animating {
        redraws.write(RequestRedraw);
    }
}

/// Smoothly follows the local player character's [`VisualTilePos`] with [`FloatingOrigin`] and
/// [`PlanetCameraFocus`] (or snaps to the first authored region while awaiting character
/// assignment on a planetary map), keeping [`MapCamera`] pinned at `(0.0, 0.0)` so coordinates
/// stay sub-pixel accurate anywhere on Sera without 5-foot snap stutter.
#[expect(
    clippy::too_many_arguments,
    reason = "a Bevy system lists every resource and query it reads as a parameter"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn sync_floating_origin(
    time: Option<Res<Time>>,
    settings: Res<ClientSettings>,
    local_player: Res<LocalPlayer>,
    authored_regions: Option<Res<LoadedAuthoredRegions>>,
    areas: Query<&MapArea>,
    characters: Query<(&EntityId, &GridPos, Option<&VisualTilePos>)>,
    mut origin: ResMut<FloatingOrigin>,
    mut planet_focus: Option<ResMut<PlanetCameraFocus>>,
    mut cameras: Query<&mut Transform, With<MapCamera>>,
    mut redraws: MessageWriter<RequestRedraw>,
) {
    let target_tile = characters
        .iter()
        .find(|(id, _, _)| local_player.is(**id))
        .map(|(_, pos, visual)| {
            visual.map_or_else(
                || (f64::from(pos.x) + 0.5, f64::from(pos.y) + 0.5),
                |v| (v.tile_x, v.tile_y),
            )
        })
        .or_else(|| {
            let is_planetary = areas.iter().last().is_some_and(|area| {
                area.0.width > MAX_FALLBACK_GRID_DIMENSION
                    || area.0.height > MAX_FALLBACK_GRID_DIMENSION
            });
            if !is_planetary {
                return None;
            }
            authored_regions
                .as_deref()
                .and_then(|loaded| loaded.regions.first())
                .map(|first| first.region.bounds.center_tile())
        });

    let Some(target) = target_tile else {
        return;
    };

    let dt = time.map_or(0.0, |t| f64::from(t.delta_secs()));
    let ((tile_x, tile_y), still_following) = if *origin == FloatingOrigin::default() {
        (target, false)
    } else {
        damp_tile_towards(
            (origin.tile_x, origin.tile_y),
            target,
            CAMERA_FOLLOW_RATE,
            dt,
        )
    };
    if still_following {
        redraws.write(RequestRedraw);
    }

    let desired_origin = FloatingOrigin { tile_x, tile_y };
    if *origin != desired_origin {
        *origin = desired_origin;
    }

    for mut camera in &mut cameras {
        if camera.translation.x != 0.0 || camera.translation.y != 0.0 {
            camera.translation.x = 0.0;
            camera.translation.y = 0.0;
        }
    }

    if let Some(focus) = planet_focus.as_mut() {
        let max_tile = f64::from(FACE_TILES - 1);
        let clamped_x = tile_x.clamp(0.0, max_tile);
        let clamped_y = tile_y.clamp(0.0, max_tile);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped_x and clamped_y are floored inside [0, FACE_TILES - 1]"
        )]
        let base_x = clamped_x.floor() as u32;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped_x and clamped_y are floored inside [0, FACE_TILES - 1]"
        )]
        let base_y = clamped_y.floor() as u32;
        let chunk_x = base_x / CHUNK_SIZE;
        let chunk_y = base_y / CHUNK_SIZE;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "local chunk offsets lie in [0.0, 64.0), exact enough in f32"
        )]
        let local_x = (clamped_x - f64::from(chunk_x * CHUNK_SIZE)) as f32;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "local chunk offsets lie in [0.0, 64.0), exact enough in f32"
        )]
        let local_y = (clamped_y - f64::from(chunk_y * CHUNK_SIZE)) as f32;
        let desired_focus = PlanetCameraFocus {
            anchor: ChunkCoord {
                face: DEFAULT_PLANET_FACE,
                cx: chunk_x,
                cy: chunk_y,
            },
            local_offset_tiles: Vec2::new(local_x, local_y),
            view_rotation: FaceRotation::Deg0,
            stream_radius_chunks: GAME_STREAM_RADIUS_CHUNKS,
            tile_pixel_size: settings.tile_size,
        };
        if **focus != desired_focus {
            **focus = desired_focus;
        }
    }
}

type TerrainEntities<'w, 's> = Query<
    'w,
    's,
    (
        &'static GridPos,
        Option<&'static EntityName>,
        Option<&'static Tags>,
    ),
>;

/// Snapshot of mounted authored regions and legacy fallback map state.
#[derive(Default)]
struct RenderedTerrainState {
    bounds: Option<MapBounds>,
    cells: Vec<TerrainEntityCell>,
    region_meshes: HashMap<String, (Entity, Vec<TerrainEntityCell>)>,
}

/// Mounts, updates, and repositions GPU splatmap meshes for authored regions near `origin`.
#[expect(
    clippy::too_many_arguments,
    reason = "helper forwards Bevy asset stores and queries from draw_map_area"
)]
fn sync_authored_region_meshes(
    commands: &mut Commands,
    tile_size: f32,
    origin: FloatingOrigin,
    regions: &LoadedAuthoredRegions,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<TerrainSplatMaterial>,
    images: &mut Assets<Image>,
    terrain_entities: &TerrainEntities,
    region_transforms: &mut Query<&mut Transform, (With<AuthoredRegionMesh>, Without<MapCamera>)>,
    rendered: &mut RenderedTerrainState,
) {
    let nearby: Vec<_> = regions
        .regions_near(DEFAULT_PLANET_FACE, origin, REGION_VIEW_MARGIN_TILES)
        .collect();
    let nearby_ids: HashSet<&str> = nearby.iter().map(|r| r.region.id.as_str()).collect();

    let stale_ids: Vec<String> = rendered
        .region_meshes
        .keys()
        .filter(|id| !nearby_ids.contains(id.as_str()))
        .cloned()
        .collect();
    for stale_id in stale_ids {
        if let Some((entity, _)) = rendered.region_meshes.remove(&stale_id) {
            commands.entity(entity).despawn();
        }
    }

    for loaded in nearby {
        let region = &loaded.region;
        let mut local_cells: Vec<TerrainEntityCell> = terrain_entities
            .iter()
            .filter_map(|(pos, name, tags)| {
                let local_pos = region.bounds.to_local_pos(*pos)?;
                entity_matches_any_terrain_layer(&region.config, name, tags).then(|| {
                    TerrainEntityCell {
                        position: local_pos,
                        name: name.map(|n| n.0.clone()),
                        tags: tags.map_or_default(|t| t.0.iter().cloned().collect()),
                    }
                })
            })
            .collect();
        local_cells.sort_by(|a, b| {
            (a.position.y, a.position.x, &a.name, &a.tags).cmp(&(
                b.position.y,
                b.position.x,
                &b.name,
                &b.tags,
            ))
        });

        let (center_tile_x, center_tile_y) = region.bounds.center_tile();
        let world_center = tile_point_at(center_tile_x, center_tile_y, tile_size, origin);

        if let Some((entity, prev_cells)) = rendered.region_meshes.get(&region.id)
            && *prev_cells == local_cells
            && let Ok(mut transform) = region_transforms.get_mut(*entity)
        {
            if transform.translation.truncate() != world_center {
                transform.translation.x = world_center.x;
                transform.translation.y = world_center.y;
            }
            continue;
        }

        if let Some((old_entity, _)) = rendered.region_meshes.remove(&region.id) {
            commands.entity(old_entity).despawn();
        }

        let region_bounds = region.bounds.map_bounds();
        let splat = images.add(build_splatmap_from_config(
            region_bounds,
            &region.config,
            &local_cells,
        ));
        #[expect(
            clippy::cast_precision_loss,
            reason = "authored region dimensions stay far below 2^23"
        )]
        let (width, height) = (
            region_bounds.width as f32 * tile_size,
            region_bounds.height as f32 * tile_size,
        );
        let entity = commands
            .spawn((
                TerrainMesh,
                AuthoredRegionMesh {
                    region_id: region.id.clone(),
                },
                Mesh2d(meshes.add(Rectangle::new(width, height))),
                MeshMaterial2d(materials.add(TerrainSplatMaterial {
                    params: terrain_params(region_bounds, &region.config),
                    scales: loaded.material_scales,
                    ambient_tint: loaded.ambient_vec4(),
                    splat,
                    layer0: loaded.material_textures[0].clone(),
                    layer1: loaded.material_textures[1].clone(),
                    layer2: loaded.material_textures[2].clone(),
                    layer3: loaded.material_textures[3].clone(),
                    region_mask: loaded.mask_handle.clone(),
                })),
                Transform::from_translation(world_center.extend(SQUARE_DEPTH)),
            ))
            .id();
        rendered
            .region_meshes
            .insert(region.id.clone(), (entity, local_cells));
    }
}

/// Draws a small rectangular fallback map (single `LoadedMapConfig` splatmap or headless
/// `MapSquare` grid) when planetary authored regions are not loaded.
#[expect(
    clippy::too_many_arguments,
    reason = "helper forwards Bevy asset stores and queries from draw_map_area"
)]
fn draw_fallback_map_area(
    commands: &mut Commands,
    bounds: MapBounds,
    tile_size: f32,
    origin: FloatingOrigin,
    map_config: Option<&LoadedMapConfig>,
    meshes: Option<&mut Assets<Mesh>>,
    materials: Option<&mut Assets<TerrainSplatMaterial>>,
    images: Option<&mut Assets<Image>>,
    terrain_entities: &TerrainEntities,
    squares: &Query<Entity, With<MapSquare>>,
    terrains: &Query<Entity, (With<TerrainMesh>, Without<AuthoredRegionMesh>)>,
    cameras: &mut Query<&mut Transform, (With<MapCamera>, Without<AuthoredRegionMesh>)>,
    rendered: &mut RenderedTerrainState,
) {
    let mut cells: Vec<TerrainEntityCell> = map_config.map_or_default(|loaded| {
        terrain_entities
            .iter()
            .filter(|(_, name, tags)| {
                entity_matches_any_terrain_layer(&loaded.config, *name, *tags)
            })
            .map(|(pos, name, tags)| TerrainEntityCell {
                position: *pos,
                name: name.map(|n| n.0.clone()),
                tags: tags.map_or_default(|t| t.0.iter().cloned().collect()),
            })
            .collect()
    });
    cells.sort_by(|a, b| {
        (a.position.y, a.position.x, &a.name, &a.tags).cmp(&(
            b.position.y,
            b.position.x,
            &b.name,
            &b.tags,
        ))
    });

    let bounds_changed = rendered.bounds != Some(bounds);
    let cells_changed = rendered.cells != cells;
    if !bounds_changed && !cells_changed {
        return;
    }
    rendered.bounds = Some(bounds);
    rendered.cells.clone_from(&cells);

    for square in squares {
        commands.entity(square).despawn();
    }
    for terrain in terrains {
        commands.entity(terrain).despawn();
    }
    let center = map_center(bounds, tile_size);

    if let (Some(loaded), Some(meshes), Some(materials), Some(images)) =
        (map_config, meshes, materials, images)
    {
        let splat = images.add(build_splatmap_from_config(bounds, &loaded.config, &cells));
        let region_mask = images.add(full_region_mask_image());
        #[expect(
            clippy::cast_precision_loss,
            reason = "map dimensions are small grid counts well within f32 precision"
        )]
        let (width, height) = (
            bounds.width as f32 * tile_size,
            bounds.height as f32 * tile_size,
        );
        commands.spawn((
            TerrainMesh,
            Mesh2d(meshes.add(Rectangle::new(width, height))),
            MeshMaterial2d(materials.add(TerrainSplatMaterial {
                params: terrain_params(bounds, &loaded.config),
                scales: loaded.material_scales,
                ambient_tint: loaded.ambient_vec4(),
                splat,
                layer0: loaded.material_textures[0].clone(),
                layer1: loaded.material_textures[1].clone(),
                layer2: loaded.material_textures[2].clone(),
                layer3: loaded.material_textures[3].clone(),
                region_mask,
            })),
            Transform::from_translation(center.extend(SQUARE_DEPTH)),
        ));
    } else {
        for x in 0..bounds.width {
            for y in 0..bounds.height {
                let position = GridPos::new(x, y);
                let color = if (x + y) % 2 == 0 {
                    GRID_LIGHT_COLOR
                } else {
                    GRID_DARK_COLOR
                };
                commands.spawn((
                    MapSquare(position),
                    Sprite::from_color(color, Vec2::splat(tile_size)),
                    Transform::from_translation(
                        square_center_at(position, tile_size, origin).extend(SQUARE_DEPTH),
                    ),
                ));
            }
        }
    }

    if bounds_changed && origin == FloatingOrigin::default() {
        for mut camera in cameras {
            camera.translation.x = center.x;
            camera.translation.y = center.y;
        }
    }
}

/// Mounts and repositions authored region polygon quads relative to [`FloatingOrigin`], or
/// falls back to small rectangular grid squares in headless tests.
#[expect(
    clippy::too_many_arguments,
    reason = "a Bevy system lists every resource and query it reads as a parameter"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn draw_map_area(
    mut commands: Commands,
    settings: Res<ClientSettings>,
    origin: Res<FloatingOrigin>,
    authored_regions: Option<Res<LoadedAuthoredRegions>>,
    map_config: Option<Res<LoadedMapConfig>>,
    mut meshes: Option<ResMut<Assets<Mesh>>>,
    mut materials: Option<ResMut<Assets<TerrainSplatMaterial>>>,
    mut images: Option<ResMut<Assets<Image>>>,
    areas: Query<&MapArea>,
    terrain_entities: TerrainEntities,
    squares: Query<Entity, With<MapSquare>>,
    terrains: Query<Entity, (With<TerrainMesh>, Without<AuthoredRegionMesh>)>,
    mut region_transforms: Query<&mut Transform, (With<AuthoredRegionMesh>, Without<MapCamera>)>,
    mut cameras: Query<&mut Transform, (With<MapCamera>, Without<AuthoredRegionMesh>)>,
    mut rendered: Local<RenderedTerrainState>,
) {
    let Some(area) = areas.iter().last() else {
        return;
    };
    let bounds = area.0;
    let tile_size = settings.tile_size;

    if let (Some(regions), Some(meshes), Some(materials), Some(images)) = (
        authored_regions,
        meshes.as_deref_mut(),
        materials.as_deref_mut(),
        images.as_deref_mut(),
    ) {
        for square in &squares {
            commands.entity(square).despawn();
        }
        for terrain in &terrains {
            commands.entity(terrain).despawn();
        }
        sync_authored_region_meshes(
            &mut commands,
            tile_size,
            *origin,
            &regions,
            meshes,
            materials,
            images,
            &terrain_entities,
            &mut region_transforms,
            &mut rendered,
        );
        rendered.bounds = Some(bounds);
        return;
    }

    // Never spawn per-tile fallback quads or single-map splatmaps for a planetary-scale face.
    if bounds.width > MAX_FALLBACK_GRID_DIMENSION || bounds.height > MAX_FALLBACK_GRID_DIMENSION {
        rendered.bounds = Some(bounds);
        return;
    }

    draw_fallback_map_area(
        &mut commands,
        bounds,
        tile_size,
        *origin,
        map_config.as_deref(),
        meshes.as_deref_mut(),
        materials.as_deref_mut(),
        images.as_deref_mut(),
        &terrain_entities,
        &squares,
        &terrains,
        &mut cameras,
        &mut rendered,
    );
}

/// Gives every newly seen entity with a grid position a colored square and a label.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn attach_entity_visuals(
    mut commands: Commands,
    settings: Res<ClientSettings>,
    origin: Res<FloatingOrigin>,
    entities: Query<(Entity, &GridPos), Without<MapLabelEntity>>,
) {
    let tile_size = settings.tile_size;
    for (entity, position) in &entities {
        let label = commands
            .spawn((
                MapLabel,
                Text2d::new(""),
                TextFont::from_font_size(tile_size * LABEL_FONT_SCALE),
                Anchor::TOP_CENTER,
                Transform::from_xyz(0.0, -tile_size / 2.0, LABEL_DEPTH),
                ChildOf(entity),
            ))
            .id();
        let visual = VisualTilePos::from_grid(*position);
        commands.entity(entity).try_insert((
            MapLabelEntity(label),
            visual,
            Sprite::from_color(OTHER_ENTITY_COLOR, Vec2::splat(tile_size * ENTITY_SCALE)),
            Transform::from_translation(
                tile_point_at(visual.tile_x, visual.tile_y, tile_size, *origin)
                    .extend(ENTITY_DEPTH),
            ),
        ));
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn move_entity_visuals(
    settings: Res<ClientSettings>,
    origin: Res<FloatingOrigin>,
    mut entities: Query<(Ref<VisualTilePos>, &mut Transform), With<MapLabelEntity>>,
) {
    let origin_changed = origin.is_changed();
    let tile_size = settings.tile_size;
    for (visual, mut transform) in &mut entities {
        if !origin_changed && !visual.is_changed() {
            continue;
        }
        let center = tile_point_at(visual.tile_x, visual.tile_y, tile_size, *origin);
        transform.translation.x = center.x;
        transform.translation.y = center.y;
    }
}

/// Styles entities from loaded authored regions (or fallback map config / colored tokens when
/// no style rule matches).
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn style_entities(
    settings: Res<ClientSettings>,
    local_player: Res<LocalPlayer>,
    authored_regions: Option<Res<LoadedAuthoredRegions>>,
    map_config: Option<Res<LoadedMapConfig>>,
    mut entities: Query<VisualQueryData, With<MapLabelEntity>>,
) {
    let tile_size = settings.tile_size;
    for (id, position, is_player_character, collider, name, tags, mut sprite, mut transform) in
        &mut entities
    {
        let matched = authored_regions
            .as_deref()
            .and_then(|regions| regions.style_for_at(DEFAULT_PLANET_FACE, *position, name, tags))
            .or_else(|| {
                map_config.as_deref().and_then(|loaded| {
                    loaded
                        .style_for(name, tags)
                        .map(|(rule, handle)| (rule, handle, loaded.ambient_color()))
                })
            });

        if let Some((rule, sprite_handle, ambient)) = matched {
            if rule.hide_sprite {
                if sprite.color != Color::NONE {
                    sprite.color = Color::NONE;
                }
                continue;
            }
            if let Some(handle) = sprite_handle {
                if sprite.image != *handle {
                    sprite.image = handle.clone();
                }
                if sprite.color != ambient {
                    sprite.color = ambient;
                }
                let wanted_size = rule
                    .size
                    .map(|[w, h]| Vec2::new(tile_size * w, tile_size * h))
                    .or_else(|| Some(Vec2::splat(tile_size * ENTITY_SCALE)));
                if sprite.custom_size != wanted_size {
                    sprite.custom_size = wanted_size;
                }
                if let Some(depth) = rule.depth {
                    transform.translation.z = depth;
                }
                continue;
            }
        }
        let is_local_player = id.is_some_and(|id| local_player.is(*id));
        let passable = collider.is_none_or(|collider| collider.passable);
        let color = entity_color(is_player_character, is_local_player, passable);
        if sprite.color != color {
            sprite.color = color;
        }
    }
}

type LabelOwners<'w, 's> = Query<
    'w,
    's,
    (
        &'static MapLabelEntity,
        &'static GridPos,
        Option<&'static EntityName>,
        Option<&'static Tags>,
        Has<Creature>,
        Has<PlayerCharacter>,
    ),
>;

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn update_labels(
    authored_regions: Option<Res<LoadedAuthoredRegions>>,
    map_config: Option<Res<LoadedMapConfig>>,
    owners: LabelOwners,
    mut labels: Query<&mut Text2d, With<MapLabel>>,
) {
    for (label, position, name, tags, is_creature, is_player) in &owners {
        let Ok(mut text) = labels.get_mut(label.0) else {
            continue;
        };
        let hide_by_region = authored_regions
            .as_deref()
            .and_then(|regions| regions.style_for_at(DEFAULT_PLANET_FACE, *position, name, tags))
            .is_some_and(|(rule, _, _)| rule.hide_label);
        let hide_by_config = map_config
            .as_deref()
            .and_then(|loaded| loaded.style_for(name, tags))
            .is_some_and(|(rule, _)| rule.hide_label);
        let show =
            is_alive_creature(is_creature, is_player, tags) && !hide_by_region && !hide_by_config;
        let wanted = if show {
            label_text(name, tags)
        } else {
            String::new()
        };
        if text.0 != wanted {
            text.0 = wanted;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use zeta_core::grid::MapBounds;
    use zeta_core::world::FACE_TILES;

    use super::*;
    use crate::planet_stream::PlanetStreamPlugin;

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(MapPlugin);
        app
    }

    fn sprite_color(app: &App, entity: Entity) -> Color {
        app.world()
            .get::<Sprite>(entity)
            .expect("the map gives every grid entity a sprite")
            .color
    }

    fn label_of(app: &App, entity: Entity) -> String {
        let label = app
            .world()
            .get::<MapLabelEntity>(entity)
            .expect("the map gives every grid entity a label");
        app.world()
            .get::<Text2d>(label.0)
            .expect("labels have text")
            .0
            .clone()
    }

    #[test]
    fn the_map_spawns_1_camera() {
        let mut app = test_app();
        app.update();
        let mut cameras = app.world_mut().query::<&MapCamera>();
        assert_eq!(cameras.iter(app.world()).count(), 1);
    }

    #[test]
    fn map_and_planet_stream_plugins_together_spawn_only_1_camera() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(MapPlugin)
            .add_plugins(PlanetStreamPlugin);
        app.update();
        let mut cameras = app.world_mut().query::<&Camera2d>();
        assert_eq!(cameras.iter(app.world()).count(), 1);
    }

    #[test]
    fn map_area_draws_every_square_and_centers_the_camera() {
        let mut app = test_app();
        let area = app
            .world_mut()
            .spawn(MapArea(MapBounds {
                width: 4,
                height: 3,
            }))
            .id();
        app.update();
        let mut squares = app.world_mut().query::<&MapSquare>();
        assert_eq!(squares.iter(app.world()).count(), 12);
        let mut cameras = app
            .world_mut()
            .query_filtered::<&Transform, With<MapCamera>>();
        let camera = cameras.single(app.world()).expect("1 camera");
        assert_eq!(camera.translation.truncate(), Vec2::new(64.0, -48.0));

        app.world_mut().entity_mut(area).insert(MapArea(MapBounds {
            width: 2,
            height: 2,
        }));
        app.update();
        let mut squares = app.world_mut().query::<&MapSquare>();
        assert_eq!(squares.iter(app.world()).count(), 4);
    }

    #[test]
    fn planetary_map_area_syncs_floating_origin_without_spawning_fallback_squares() {
        let mut app = test_app();
        app.init_resource::<PlanetCameraFocus>();
        app.world_mut().spawn(MapArea(MapBounds {
            width: FACE_TILES,
            height: FACE_TILES,
        }));
        let start = GridPos::new(1_819_652, 318_474);
        let neighbor_pos = GridPos::new(1_819_653, 318_474);
        let player = app
            .world_mut()
            .spawn((EntityId(1), start, PlayerCharacter))
            .id();
        let neighbor = app.world_mut().spawn((EntityId(2), neighbor_pos)).id();
        app.world_mut().resource_mut::<LocalPlayer>().character = Some(EntityId(1));
        app.update();

        let mut squares = app.world_mut().query::<&MapSquare>();
        assert_eq!(squares.iter(app.world()).count(), 0);
        assert_eq!(
            *app.world().resource::<FloatingOrigin>(),
            FloatingOrigin::from_tile_center(start)
        );
        let focus = *app.world().resource::<PlanetCameraFocus>();
        assert_eq!(focus.anchor.face, CubeFace::PosX);
        assert_eq!(focus.anchor.cx, start.x / CHUNK_SIZE);
        assert_eq!(focus.anchor.cy, start.y / CHUNK_SIZE);
        assert_eq!(focus.stream_radius_chunks, GAME_STREAM_RADIUS_CHUNKS);
        assert_eq!(focus.tile_pixel_size, crate::settings::TILE_SIZE);

        let player_pos = app
            .world()
            .get::<Transform>(player)
            .expect("placed")
            .translation
            .truncate();
        let neighbor_world = app
            .world()
            .get::<Transform>(neighbor)
            .expect("placed")
            .translation
            .truncate();
        assert_eq!(player_pos, Vec2::ZERO);
        assert_eq!(neighbor_world, Vec2::new(crate::settings::TILE_SIZE, 0.0));

        // Step the local player 1 tile east: with deterministic 16ms frames, the character glides
        // and the camera trails smoothly until both settle onto `neighbor_pos`.
        // `TimeUpdateStrategy`: bevy_time-0.20.1/src/lib.rs:113.
        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            std::time::Duration::from_millis(16),
        ));
        app.world_mut().entity_mut(player).insert(neighbor_pos);
        for _ in 0..80 {
            app.update();
        }
        assert_eq!(
            app.world()
                .get::<Transform>(player)
                .expect("placed")
                .translation
                .truncate(),
            Vec2::ZERO
        );
        assert_eq!(
            app.world()
                .get::<Transform>(neighbor)
                .expect("placed")
                .translation
                .truncate(),
            Vec2::ZERO
        );
    }

    #[test]
    fn grid_entities_are_drawn_at_their_square_and_follow_moves() {
        let mut app = test_app();
        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            std::time::Duration::from_millis(16),
        ));
        let barrel = app.world_mut().spawn(GridPos::new(2, 1)).id();
        app.update();
        let transform = app.world().get::<Transform>(barrel).expect("placed");
        assert_eq!(transform.translation.truncate(), Vec2::new(80.0, -48.0));

        app.world_mut()
            .entity_mut(barrel)
            .insert(GridPos::new(0, 0));
        for _ in 0..40 {
            app.update();
        }
        let transform = app.world().get::<Transform>(barrel).expect("placed");
        assert_eq!(transform.translation.truncate(), Vec2::new(16.0, -16.0));
    }

    #[test]
    fn the_local_player_is_highlighted_and_other_players_are_not() {
        let mut app = test_app();
        let mine = app
            .world_mut()
            .spawn((EntityId(1), GridPos::new(0, 0), PlayerCharacter))
            .id();
        let theirs = app
            .world_mut()
            .spawn((EntityId(2), GridPos::new(1, 0), PlayerCharacter))
            .id();
        app.update();
        assert_eq!(sprite_color(&app, mine), PLAYER_CHARACTER_COLOR);

        app.world_mut().resource_mut::<LocalPlayer>().character = Some(EntityId(1));
        app.update();
        assert_eq!(sprite_color(&app, mine), LOCAL_PLAYER_COLOR);
        assert_eq!(sprite_color(&app, theirs), PLAYER_CHARACTER_COLOR);
    }

    #[test]
    fn impassable_entities_are_drawn_darker_than_passable_ones() {
        let mut app = test_app();
        let wall = app
            .world_mut()
            .spawn((GridPos::new(0, 0), Collider { passable: false }))
            .id();
        let barrel = app
            .world_mut()
            .spawn((GridPos::new(1, 0), Collider { passable: true }))
            .id();
        app.update();
        assert_eq!(sprite_color(&app, wall), IMPASSABLE_COLOR);
        assert_eq!(sprite_color(&app, barrel), OTHER_ENTITY_COLOR);
    }

    #[test]
    fn labels_show_only_for_alive_creatures_and_follow_tag_changes() {
        let mut app = test_app();
        let barrel = app
            .world_mut()
            .spawn((GridPos::new(0, 0), EntityName("Oil barrel".to_owned())))
            .id();
        let goblin = app
            .world_mut()
            .spawn((
                GridPos::new(1, 0),
                EntityName("Goblin".to_owned()),
                Creature,
            ))
            .id();
        app.update();
        assert_eq!(label_of(&app, barrel), "");
        assert_eq!(label_of(&app, goblin), "Goblin");

        let tags = Tags(BTreeSet::from(["burning".to_owned()]));
        app.world_mut().entity_mut(goblin).insert(tags);
        app.update();
        assert_eq!(label_of(&app, goblin), "Goblin\n[burning]");

        let dead = Tags(BTreeSet::from(["dead".to_owned()]));
        app.world_mut().entity_mut(goblin).insert(dead);
        app.update();
        assert_eq!(label_of(&app, goblin), "");
    }

    #[test]
    fn despawning_an_entity_also_removes_its_label() {
        let mut app = test_app();
        let goblin = app.world_mut().spawn(GridPos::new(0, 0)).id();
        app.update();
        app.world_mut().entity_mut(goblin).despawn();
        app.update();
        let mut labels = app.world_mut().query::<&MapLabel>();
        assert_eq!(labels.iter(app.world()).count(), 0);
    }

    #[test]
    fn entity_visual_and_camera_lerp_smoothly_instead_of_snapping() {
        let mut app = test_app();
        // `TimeUpdateStrategy`: bevy_time-0.20.1/src/lib.rs:113.
        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            std::time::Duration::from_millis(16),
        ));
        let player = app
            .world_mut()
            .spawn((EntityId(1), GridPos::new(100, 100), PlayerCharacter))
            .id();
        app.world_mut().resource_mut::<LocalPlayer>().character = Some(EntityId(1));
        app.update();

        // Initial spawn snaps camera directly onto player center.
        let initial_origin = *app.world().resource::<FloatingOrigin>();
        assert_eq!(initial_origin.tile_x, 100.5);
        assert_eq!(initial_origin.tile_y, 100.5);

        // Move 1 square east and step 16ms: character glides ahead while camera trails smoothly.
        app.world_mut()
            .entity_mut(player)
            .insert(GridPos::new(101, 100));
        app.update();

        let visual = *app
            .world()
            .get::<VisualTilePos>(player)
            .expect("visual pos attached");
        let mid_origin = *app.world().resource::<FloatingOrigin>();
        assert!(
            visual.tile_x > 100.5 && visual.tile_x < 101.5,
            "character should glide smoothly instead of snapping: {visual:?}"
        );
        assert!(
            mid_origin.tile_x > 100.5 && mid_origin.tile_x < visual.tile_x,
            "camera should trail behind character instead of hard-locking: origin={mid_origin:?}, visual={visual:?}"
        );
        let sprite_x = app
            .world()
            .get::<Transform>(player)
            .expect("transform")
            .translation
            .x;
        assert!(
            sprite_x > 0.0,
            "character should visibly lead camera center while walking east: sprite_x={sprite_x}"
        );

        // Over ~1.3s of 16ms frames, both character and camera settle cleanly to (101.5, 100.5).
        for _ in 0..80 {
            app.update();
        }
        let settled_visual = *app
            .world()
            .get::<VisualTilePos>(player)
            .expect("visual pos");
        let settled_origin = *app.world().resource::<FloatingOrigin>();
        assert_eq!(settled_visual.tile_x, 101.5);
        assert_eq!(settled_origin.tile_x, 101.5);
    }
}
