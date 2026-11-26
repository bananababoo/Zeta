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
use zeta_core::grid::GridPos;
use zeta_core::protocol::{Collider, EntityName, MapArea, PlayerCharacter, Tags};

use crate::layout::{map_center, square_center};
use crate::player::LocalPlayer;
use crate::settings::{
    ClientSettings, GRID_DARK_COLOR, GRID_LIGHT_COLOR, IMPASSABLE_COLOR, LOCAL_PLAYER_COLOR,
    OTHER_ENTITY_COLOR, PLAYER_CHARACTER_COLOR,
};
use crate::terrain::{
    DEFAULT_MAP_CONFIG, LoadedMapConfig, TerrainSplatMaterial, build_splatmap_from_config,
    load_map_config, terrain_params,
};

/// Draw order of grid squares and the GPU terrain splatmap.
const SQUARE_DEPTH: f32 = 0.0;
/// Draw order of entities, above the squares.
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

/// Marks the GPU terrain splatmap quad.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerrainMesh;

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
                .add_systems(Startup, load_default_map_config);
        }
        app.init_resource::<ClientSettings>()
            .init_resource::<LocalPlayer>()
            .add_systems(Startup, spawn_camera)
            .add_systems(
                Update,
                (
                    draw_map_area,
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
fn load_default_map_config(mut commands: Commands, asset_server: Res<AssetServer>) {
    match load_map_config(&asset_server, DEFAULT_MAP_CONFIG) {
        Ok(loaded) => commands.insert_resource(loaded),
        Err(error) => warn!("map visual config not loaded ({error}); using fallback grid"),
    }
}

/// Redraws the map terrain (GPU splatmap from `LoadedMapConfig` when present, or fallback
/// grid squares in headless tests) whenever `MapArea` changes, and centers the camera.
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
    map_config: Option<Res<LoadedMapConfig>>,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<TerrainSplatMaterial>>>,
    images: Option<ResMut<Assets<Image>>>,
    areas: Query<&MapArea, Changed<MapArea>>,
    squares: Query<Entity, With<MapSquare>>,
    terrains: Query<Entity, With<TerrainMesh>>,
    mut cameras: Query<&mut Transform, With<MapCamera>>,
) {
    let Some(area) = areas.iter().last() else {
        return;
    };
    for square in &squares {
        commands.entity(square).despawn();
    }
    for terrain in &terrains {
        commands.entity(terrain).despawn();
    }
    let bounds = area.0;
    let tile_size = settings.tile_size;
    let center = map_center(bounds, tile_size);

    if let (Some(loaded), Some(mut meshes), Some(mut materials), Some(mut images)) =
        (map_config, meshes, materials, images)
    {
        let splat = images.add(build_splatmap_from_config(bounds, &loaded.config));
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
                splat,
                layer0: loaded.material_textures[0].clone(),
                layer1: loaded.material_textures[1].clone(),
                layer2: loaded.material_textures[2].clone(),
                layer3: loaded.material_textures[3].clone(),
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
                        square_center(position, tile_size).extend(SQUARE_DEPTH),
                    ),
                ));
            }
        }
    }

    for mut camera in &mut cameras {
        camera.translation.x = center.x;
        camera.translation.y = center.y;
    }
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

/// Styles entities from the loaded external map config (or colored fallback tokens when no
/// style rule matches).
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn style_entities(
    settings: Res<ClientSettings>,
    local_player: Res<LocalPlayer>,
    map_config: Option<Res<LoadedMapConfig>>,
    mut entities: Query<VisualQueryData, With<MapLabelEntity>>,
) {
    let tile_size = settings.tile_size;
    for (id, is_player_character, collider, name, tags, mut sprite, mut transform) in &mut entities
    {
        if let Some((rule, sprite_handle)) = map_config
            .as_deref()
            .and_then(|loaded| loaded.style_for(name, tags))
        {
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
                if sprite.color != Color::WHITE {
                    sprite.color = Color::WHITE;
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

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn update_labels(
    map_config: Option<Res<LoadedMapConfig>>,
    owners: Query<(&MapLabelEntity, Option<&EntityName>, Option<&Tags>)>,
    mut labels: Query<&mut Text2d, With<MapLabel>>,
) {
    for (label, name, tags) in &owners {
        let Ok(mut text) = labels.get_mut(label.0) else {
            continue;
        };
        let hide = map_config
            .as_deref()
            .and_then(|loaded| loaded.style_for(name, tags))
            .is_some_and(|(rule, _)| rule.hide_label);
        let wanted = if hide {
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
