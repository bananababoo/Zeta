//! 2D grid map: a camera, the squares of the map area, a colored square for every entity
//! with a grid position, and a label with its name and Tag badges.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 5 (independent plugin over
//! replicated components) and `docs/prototypes/realism_prototype.md`. Colors stand in for
//! sprites until the sprite library exists.

use bevy::prelude::*;
use bevy::sprite::Anchor;
use bevy::sprite_render::{Material2dPlugin, MeshMaterial2d};
use zeta_core::entity::EntityId;
use zeta_core::grid::{GridPos, MapBounds};
use zeta_core::protocol::{Collider, EntityName, MapArea, PlayerCharacter, Tags};

use crate::layout::{map_center, square_center};
use crate::player::LocalPlayer;
use crate::settings::{
    ClientSettings, GRID_DARK_COLOR, GRID_LIGHT_COLOR, IMPASSABLE_COLOR, LOCAL_PLAYER_COLOR,
    OTHER_ENTITY_COLOR, PLAYER_CHARACTER_COLOR,
};
use crate::terrain::{
    TerrainSplatMaterial, build_splatmap_image, load_repeating_texture, terrain_params,
};

/// Draw order of grid squares and the GPU terrain splatmap.
const SQUARE_DEPTH: f32 = 0.0;
/// Draw order of passable surface props like bridges, above terrain and below characters.
const SURFACE_DEPTH: f32 = 0.5;
/// Draw order of entities, above the squares and bridges.
const ENTITY_DEPTH: f32 = 1.0;
/// Draw order of a label relative to its entity.
const LABEL_DEPTH: f32 = 1.0;
/// Share of a square an entity's colored square covers, so neighbors stay apart.
const ENTITY_SCALE: f32 = 0.8;
/// Font size of entity labels relative to the square size.
const LABEL_FONT_SCALE: f32 = 0.35;

/// Drawn entities whose grid position changed.
type MovedEntityFilter = (Changed<GridPos>, With<MapLabelEntity>);
/// What deciding an entity's visual reads.
type VisualQueryData = (
    Option<&'static EntityId>,
    Has<PlayerCharacter>,
    Option<&'static Collider>,
    Option<&'static EntityName>,
    Option<&'static Tags>,
    &'static mut Sprite,
    &'static mut Transform,
);

/// Loaded terrain and object texture handles, present when the asset plugin is enabled.
#[derive(Resource, Clone, Debug)]
struct MapTextures {
    grass: Handle<Image>,
    dirt: Handle<Image>,
    rock: Handle<Image>,
    water: Handle<Image>,
    wooden_bridge: Handle<Image>,
    oil_barrel: Handle<Image>,
    oil_barrel_broken: Handle<Image>,
}

/// Marks the GPU terrain splatmap quad.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerrainMesh;

/// Snapshot of the inputs used to build the current GPU splatmap, so it rebuilds only on change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TerrainFingerprint {
    bounds: Option<MapBounds>,
    water: Vec<GridPos>,
    bridges: Vec<GridPos>,
}

/// Marks the camera that shows the map.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MapCamera;

/// Marks 1 drawn square of the map area.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapSquare(pub GridPos);

/// Marks the label below an entity, showing its name and Tag badges.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MapLabel;

/// Added to an entity once the map draws it: the entity of its label.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapLabelEntity(pub Entity);

/// Draws the map area, entities with a grid position, and their labels.
#[derive(Clone, Copy, Debug, Default)]
pub struct MapPlugin;

impl Plugin for MapPlugin {
    fn build(&self, app: &mut App) {
        // `App::is_plugin_added`: bevy_app-0.20.1/src/app.rs:570.
        if app.is_plugin_added::<AssetPlugin>() {
            app.add_plugins(Material2dPlugin::<TerrainSplatMaterial>::default())
                .add_systems(Startup, load_map_textures);
        }
        app.init_resource::<ClientSettings>()
            .init_resource::<LocalPlayer>()
            .add_systems(Startup, spawn_camera)
            .add_systems(
                Update,
                (
                    draw_map_area,
                    sync_terrain_splatmap,
                    attach_entity_visuals,
                    move_entity_visuals,
                    style_entities,
                    update_labels,
                )
                    .chain(),
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

fn spawn_camera(mut commands: Commands) {
    commands.spawn((MapCamera, Camera2d));
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn load_map_textures(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.insert_resource(MapTextures {
        grass: load_repeating_texture(&asset_server, "terrain/grass.png"),
        dirt: load_repeating_texture(&asset_server, "terrain/dirt.png"),
        rock: load_repeating_texture(&asset_server, "terrain/rock.png"),
        water: load_repeating_texture(&asset_server, "terrain/water.png"),
        wooden_bridge: asset_server.load("objects/wooden_bridge.png"),
        oil_barrel: asset_server.load("objects/oil_barrel.png"),
        oil_barrel_broken: asset_server.load("objects/oil_barrel_broken.png"),
    });
}

fn is_water_entity(name: Option<&EntityName>, tags: Option<&Tags>) -> bool {
    tags.is_some_and(|tags| tags.0.iter().any(|tag| tag.eq_ignore_ascii_case("water")))
        || name.is_some_and(|name| name.0.eq_ignore_ascii_case("river"))
}

fn is_bridge_entity(name: Option<&EntityName>) -> bool {
    name.is_some_and(|name| name.0.to_ascii_lowercase().contains("bridge"))
}

fn is_barrel_entity(name: Option<&EntityName>) -> bool {
    name.is_some_and(|name| name.0.to_ascii_lowercase().contains("barrel"))
}

fn is_broken(tags: Option<&Tags>) -> bool {
    tags.is_some_and(|tags| {
        tags.0
            .iter()
            .any(|tag| tag.eq_ignore_ascii_case("broken") || tag.eq_ignore_ascii_case("destroyed"))
    })
}

/// Redraws every fallback square when the map area appears or changes (when running without
/// the GPU terrain renderer) and centers the camera on it.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn draw_map_area(
    mut commands: Commands,
    settings: Res<ClientSettings>,
    textures: Option<Res<MapTextures>>,
    areas: Query<&MapArea, Changed<MapArea>>,
    squares: Query<Entity, With<MapSquare>>,
    mut cameras: Query<&mut Transform, With<MapCamera>>,
) {
    let Some(area) = areas.iter().last() else {
        return;
    };
    for square in &squares {
        commands.entity(square).despawn();
    }
    let bounds = area.0;
    let tile_size = settings.tile_size;
    if textures.is_none() {
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
                        square_center(position, tile_size).extend(SQUARE_DEPTH),
                    ),
                ));
            }
        }
    }
    let center = map_center(bounds, tile_size);
    for mut camera in &mut cameras {
        camera.translation.x = center.x;
        camera.translation.y = center.y;
    }
}

/// Builds or updates the GPU splatmap terrain mesh whenever the map bounds or its water and
/// bridge squares change.
#[expect(
    clippy::too_many_arguments,
    reason = "a Bevy system lists every resource and query it reads as a parameter"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn sync_terrain_splatmap(
    mut commands: Commands,
    settings: Res<ClientSettings>,
    textures: Option<Res<MapTextures>>,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<TerrainSplatMaterial>>>,
    images: Option<ResMut<Assets<Image>>>,
    areas: Query<&MapArea>,
    entities: Query<(&GridPos, Option<&EntityName>, Option<&Tags>)>,
    existing: Query<Entity, With<TerrainMesh>>,
    mut last: Local<TerrainFingerprint>,
) {
    let (Some(textures), Some(mut meshes), Some(mut materials), Some(mut images)) =
        (textures, meshes, materials, images)
    else {
        return;
    };
    let Some(area) = areas.iter().last() else {
        return;
    };
    let bounds = area.0;
    let mut water = Vec::new();
    let mut bridges = Vec::new();
    for (pos, name, tags) in &entities {
        if is_water_entity(name, tags) {
            water.push(*pos);
        } else if is_bridge_entity(name) {
            bridges.push(*pos);
        }
    }
    water.sort_by_key(|pos| (pos.x, pos.y));
    bridges.sort_by_key(|pos| (pos.x, pos.y));
    let current = TerrainFingerprint {
        bounds: Some(bounds),
        water,
        bridges,
    };
    if *last == current {
        return;
    }
    for entity in &existing {
        commands.entity(entity).despawn();
    }
    let splat = images.add(build_splatmap_image(
        bounds,
        &current.water,
        &current.bridges,
    ));
    #[expect(
        clippy::cast_precision_loss,
        reason = "map dimensions are small grid counts well within f32 precision"
    )]
    let (width, height) = (
        bounds.width as f32 * settings.tile_size,
        bounds.height as f32 * settings.tile_size,
    );
    let center = map_center(bounds, settings.tile_size);
    commands.spawn((
        TerrainMesh,
        Mesh2d(meshes.add(Rectangle::new(width, height))),
        MeshMaterial2d(materials.add(TerrainSplatMaterial {
            params: terrain_params(bounds),
            splat,
            grass: textures.grass.clone(),
            dirt: textures.dirt.clone(),
            rock: textures.rock.clone(),
            water: textures.water.clone(),
        })),
        Transform::from_translation(center.extend(SQUARE_DEPTH)),
    ));
    *last = current;
}

/// Gives every newly seen entity with a grid position a colored square and a label.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn attach_entity_visuals(
    mut commands: Commands,
    settings: Res<ClientSettings>,
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
        commands.entity(entity).try_insert((
            MapLabelEntity(label),
            Sprite::from_color(OTHER_ENTITY_COLOR, Vec2::splat(tile_size * ENTITY_SCALE)),
            Transform::from_translation(square_center(*position, tile_size).extend(ENTITY_DEPTH)),
        ));
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn move_entity_visuals(
    settings: Res<ClientSettings>,
    mut entities: Query<(&GridPos, &mut Transform), MovedEntityFilter>,
) {
    for (position, mut transform) in &mut entities {
        let center = square_center(*position, settings.tile_size);
        transform.translation.x = center.x;
        transform.translation.y = center.y;
    }
}

/// Styles entities every frame (sprites for known world props, transparent for water drawn
/// into the GPU terrain splatmap, and colored tokens for everything else), because the local
/// player, markers, names, and colliders can arrive in any order.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn style_entities(
    settings: Res<ClientSettings>,
    local_player: Res<LocalPlayer>,
    textures: Option<Res<MapTextures>>,
    mut entities: Query<VisualQueryData, With<MapLabelEntity>>,
) {
    let tile_size = settings.tile_size;
    for (id, is_player_character, collider, name, tags, mut sprite, mut transform) in &mut entities
    {
        if let Some(textures) = textures.as_deref() {
            if is_water_entity(name, tags) {
                if sprite.color != Color::NONE {
                    sprite.color = Color::NONE;
                }
                continue;
            }
            if is_bridge_entity(name) {
                let wanted_size = Some(Vec2::new(tile_size * 1.45, tile_size * 1.15));
                if sprite.image != textures.wooden_bridge {
                    sprite.image = textures.wooden_bridge.clone();
                }
                if sprite.color != Color::WHITE {
                    sprite.color = Color::WHITE;
                }
                if sprite.custom_size != wanted_size {
                    sprite.custom_size = wanted_size;
                }
                transform.translation.z = SURFACE_DEPTH;
                continue;
            }
            if is_barrel_entity(name) {
                let handle = if is_broken(tags) {
                    &textures.oil_barrel_broken
                } else {
                    &textures.oil_barrel
                };
                let wanted_size = Some(Vec2::new(tile_size * 0.9, tile_size * 0.65));
                if sprite.image != *handle {
                    sprite.image = handle.clone();
                }
                if sprite.color != Color::WHITE {
                    sprite.color = Color::WHITE;
                }
                if sprite.custom_size != wanted_size {
                    sprite.custom_size = wanted_size;
                }
                transform.translation.z = ENTITY_DEPTH;
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

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn update_labels(
    textures: Option<Res<MapTextures>>,
    owners: Query<(&MapLabelEntity, Option<&EntityName>, Option<&Tags>)>,
    mut labels: Query<&mut Text2d, With<MapLabel>>,
) {
    let has_gpu_terrain = textures.is_some();
    for (label, name, tags) in &owners {
        let Ok(mut text) = labels.get_mut(label.0) else {
            continue;
        };
        let wanted = if has_gpu_terrain && is_water_entity(name, tags) {
            String::new()
        } else {
            label_text(name, tags)
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

    use super::*;

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
    fn grid_entities_are_drawn_at_their_square_and_follow_moves() {
        let mut app = test_app();
        let barrel = app.world_mut().spawn(GridPos::new(2, 1)).id();
        app.update();
        let transform = app.world().get::<Transform>(barrel).expect("placed");
        assert_eq!(transform.translation.truncate(), Vec2::new(80.0, -48.0));

        app.world_mut()
            .entity_mut(barrel)
            .insert(GridPos::new(0, 0));
        app.update();
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
    fn labels_show_the_name_and_tags_as_badges_and_follow_changes() {
        let mut app = test_app();
        let barrel = app
            .world_mut()
            .spawn((GridPos::new(0, 0), EntityName("Oil barrel".to_owned())))
            .id();
        app.update();
        assert_eq!(label_of(&app, barrel), "Oil barrel");

        let tags = Tags(BTreeSet::from(["burning".to_owned(), "leaking".to_owned()]));
        app.world_mut().entity_mut(barrel).insert(tags);
        app.update();
        assert_eq!(label_of(&app, barrel), "Oil barrel\n[burning] [leaking]");
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
}
