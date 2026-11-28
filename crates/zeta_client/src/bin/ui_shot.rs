//! Dev tool: renders the real client UI or planetary biome scenes to PNG files, then exits.
//!
//! Usage:
//! - `cargo run -p zeta_client --bin ui_shot --offline -- <scene> <out.png>`
//! - `cargo run -p zeta_client --bin ui_shot --offline -- --batch <out_dir> [scene...]`
//! - `cargo run -p zeta_client --bin ui_shot --offline -- --list`
//!
//! It builds the game's own `ClientPlugins` (or `PlanetStreamPlugin` + `GroundClutterPlugin` for
//! unobstructed planetary/biome scenes) and fakes only 2 things: the network (protocol messages
//! are plain engine messages, and the scene spawns the entities the server would replicate) and
//! the mouse cursor. No server runs and no window opens: the camera renders to an off-screen
//! image of the window's size, and Bevy's screenshot API saves that image. The game binary never
//! uses this file.
//!
//! Adding a scene: add a `Scene` variant, give it a name in `Scene::name` and `Scene::ALL`,
//! and list its cursor steps in `Scene::steps` (or focus in `Scene::planet_focus`).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use bevy::app::ScheduleRunnerPlugin;
use bevy::camera::RenderTarget;
use bevy::ecs::component::Mutable;
// `Added`: bevy_ecs-0.20.1/src/query/filter.rs:757.
use bevy::ecs::query::Added;
use bevy::ecs::system::SystemParam;
use bevy::input::ButtonState;
use bevy::input::mouse::MouseButtonInput;
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::render_resource::TextureFormat;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured, save_to_disk};
use bevy::ui::UiGlobalTransform;
use bevy::window::{PrimaryWindow, WindowResolution};
use bevy::winit::WinitPlugin;
use serde::Serialize;
use serde::de::DeserializeOwned;
use zeta_client::ClientPlugins;
use zeta_client::chunk_splat::{ChunkGroundCover, SPLAT_MIN_TILE_PIXELS};
use zeta_client::ground_clutter::{
    GroundClutter, GroundClutterAssets, GroundClutterLayer, GroundClutterPlugin,
};
use zeta_client::inventory::{InventorySlot, InventoryToggleButton};
use zeta_client::layout::{FloatingOrigin, square_center_at};
use zeta_client::planet_stream::{
    PlanetCameraFocus, PlanetStreamPlugin, PlanetStreamStats, PlanetStreamSyncMode, focus_tile,
    streamed_ring,
};
use zeta_client::settings::ClientSettings;
use zeta_core::catalog::CatalogId;
use zeta_core::entity::EntityId;
use zeta_core::grid::{GridPos, MapBounds};
use zeta_core::protocol::{
    Collider, Creature, Delivery, DiceRollShown, EntityName, Inventory, Item, LogEntryKind,
    MapArea, NarrationLogEntry, PlayerAssigned, PlayerCharacter, PromptState, PromptStatusUpdate,
    ProtocolRegistrar, Tags, TurnBubble, Visual, register_protocol,
};
use zeta_core::resolution::{D20, Volume};
use zeta_core::world::{ChunkCoord, CubeFace, FACE_TILES, FaceRotation};

/// Width of the rendered image in pixels, at a scale factor of 1.
const IMAGE_WIDTH: u32 = 1280;
/// Height of the rendered image in pixels.
const IMAGE_HEIGHT: u32 = 720;
/// Frames before the first cursor step, so the fake world replicates, assets load, and UI lays out.
const SETTLE_FRAMES: u32 = 25;
/// Minimum frames when switching planet/biome scenes in batch mode before checking chunk readiness.
const PLANET_SWITCH_SETTLE_FRAMES: u32 = 6;
/// Consecutive frames with zero newly added clutter entities required before taking a screenshot.
const CLUTTER_QUIET_FRAMES: u32 = 3;
/// Frames after each cursor step, so the UI reacts and lays out again.
const STEP_FRAMES: u32 = 6;
/// Frames after which a scene that never finishes fails.
const TIMEOUT_FRAMES: u32 = 600;
/// Pause between frames; the tool runs without a window, so nothing else paces it.
const FRAME_WAIT: Duration = Duration::from_millis(5);
/// Argument that prints the scene names, 1 per line, for `tools/ui_shots.ps1`.
const LIST_FLAG: &str = "--list";
/// Argument that renders multiple scenes into an output directory.
const BATCH_FLAG: &str = "--batch";

/// Tactical zoom scale (pixels per 5-foot square) for the 11 planetary biome showcase scenes.
/// At `1280x720`, `52.0` px/tile frames ~`24.6 x 13.8` squares, matching tabletop battlemap scale.
const BIOME_TILE_PIXEL_SIZE: f32 = 52.0;
/// Chunk streaming ring half-width (`3x3` chunks = `192x192` tiles) for tactical biome scenes.
/// Gives >50 tiles of off-screen margin on every side of the `~25x14`-tile viewport (plenty for
/// multi-tile overhanging canopy trees) while baking 9 chunks instead of 25 in debug builds.
const BIOME_STREAM_RADIUS_CHUNKS: i32 = 1;

/// The local character's id in every scene.
const LOCAL_CHARACTER: EntityId = EntityId(1);
/// Planetary top-left tile anchor of the River Crossing authored region on Sera (`PosX`),
/// matching `zeta_server::world::RIVER_CROSSING_ORIGIN` and `assets/world/regions/river_crossing.json`.
const RIVER_CROSSING_ORIGIN: GridPos = GridPos::new(1_819_648, 318_464);

const fn river_crossing_pos(local_x: u32, local_y: u32) -> GridPos {
    GridPos::new(
        RIVER_CROSSING_ORIGIN.x + local_x,
        RIVER_CROSSING_ORIGIN.y + local_y,
    )
}

const fn biome_focus(
    face: CubeFace,
    cx: u32,
    cy: u32,
    local_x: f32,
    local_y: f32,
) -> PlanetCameraFocus {
    PlanetCameraFocus {
        anchor: ChunkCoord { face, cx, cy },
        local_offset_tiles: Vec2::new(local_x, local_y),
        view_rotation: FaceRotation::Deg0,
        stream_radius_chunks: BIOME_STREAM_RADIUS_CHUNKS,
        tile_pixel_size: BIOME_TILE_PIXEL_SIZE,
    }
}

/// Carried by the local character: catalog id and quantity. The first stack is equipped.
const CARRIED: [(&str, u32); 9] = [
    ("longsword", 1),
    ("shield", 1),
    ("leather_armor", 1),
    ("painters_supplies", 1),
    ("thieves_tools", 1),
    ("healers_kit", 1),
    ("rations", 5),
    ("torch", 10),
    ("rope", 1),
];

/// 1 state of the UI or planetary terrain the tool can render.
#[allow(
    clippy::enum_variant_names,
    reason = "each name starts with the panel or biome group it shows"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scene {
    /// The inventory panel open, cursor away.
    InventoryOpen,
    /// The cursor over the Painter's Supplies slot, so its tooltip shows.
    InventoryHover,
    /// The longsword slot clicked, so its action menu shows.
    InventoryMenu,
    /// The river, wooden bridge, oil barrel, goblin, and players rendered with GPU terrain.
    RiverScene,
    /// All HUD panels visible together: Turn Banner, settled Dice Roll, Narration Log, and Prompt.
    CombatHud,
    /// Right-clicking the Wooden Bridge opens its inspect popup with name, description, and attributes.
    InspectBridge,
    /// Live streamed `64x64` planetary terrain chunks over a real dramatic Sera coastline.
    PlanetCoast,
    /// Live streamed `64x64` chunks directly straddling the rotated `NegX <-> PosY` cube-face seam.
    PlanetSeam,
    /// The River Crossing scene with the local character at the region's north-west corner,
    /// so the authored polygon edge and the planet ground around it share the screen.
    RiverEdge,
    /// Temperate woodland (`~15 C`, `forest_floor` opening onto a natural grassy clearing) at tactical zoom.
    BiomeTemperateForest,
    /// Tropical wet jungle (`~26 C`, saturated moisture & foliage, marsh pocket) at tactical zoom.
    BiomeJungle,
    /// Boreal cold conifer taiga (`~3 C`, dense foliage, gentle slope) at tactical zoom.
    BiomeTaiga,
    /// Sunlit temperate lush meadow (`lush_grass` + `grass`, gentle rolling relief) at tactical zoom.
    BiomeLushMeadow,
    /// Warm dry savanna (`dry_grass` + natural `dirt` patches) at tactical zoom.
    BiomeSavanna,
    /// Arid desert sand abutting an exposed rocky escarpment (`desert_sand` + `rock`) at tactical zoom.
    BiomeDesert,
    /// Flat saturated lowland marsh (`swamp` pools + surrounding `forest_floor`) at tactical zoom.
    BiomeSwamp,
    /// Sub-zero tundra meeting exposed highland rock (`tundra` + `rock`) at tactical zoom.
    BiomeTundra,
    /// High mountain snowline (`~13,600 ft`, `rock` steps + `snow` peak) at tactical zoom.
    BiomeAlpine,
    /// Deep-freeze polar/highland snowfield expanse with gentle relief at tactical zoom.
    BiomeSnowfield,
    /// Tactical shoreline (`shallow_water` + `beach` + vegetated `lush_grass` coast).
    BiomeCoast,
}

/// What the fake cursor points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// The button that opens the inventory panel.
    InventoryToggle,
    /// The slot of the local character's stack of this catalog id.
    Slot(&'static str),
    /// The center of 1 map grid square.
    Square(GridPos),
}

/// 1 action of the fake cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// Move the cursor over the target and stay there.
    Hover(Target),
    /// Move the cursor over the target and click the left button.
    Click(Target),
    /// Move the cursor over the target and click the right button.
    RightClick(Target),
}

impl Scene {
    /// Every scene, in the order `tools/ui_shots.ps1` lists them.
    const ALL: [Self; 20] = [
        Self::InventoryOpen,
        Self::InventoryHover,
        Self::InventoryMenu,
        Self::RiverScene,
        Self::CombatHud,
        Self::InspectBridge,
        Self::PlanetCoast,
        Self::PlanetSeam,
        Self::RiverEdge,
        Self::BiomeTemperateForest,
        Self::BiomeJungle,
        Self::BiomeTaiga,
        Self::BiomeLushMeadow,
        Self::BiomeSavanna,
        Self::BiomeDesert,
        Self::BiomeSwamp,
        Self::BiomeTundra,
        Self::BiomeAlpine,
        Self::BiomeSnowfield,
        Self::BiomeCoast,
    ];

    /// The name given on the command line, which is also the PNG file name.
    fn name(self) -> &'static str {
        match self {
            Self::InventoryOpen => "inventory_open",
            Self::InventoryHover => "inventory_hover",
            Self::InventoryMenu => "inventory_menu",
            Self::RiverScene => "river_scene",
            Self::CombatHud => "combat_hud",
            Self::InspectBridge => "inspect_bridge",
            Self::PlanetCoast => "planet_coast",
            Self::PlanetSeam => "planet_seam",
            Self::RiverEdge => "river_edge",
            Self::BiomeTemperateForest => "biome_temperate_forest",
            Self::BiomeJungle => "biome_jungle",
            Self::BiomeTaiga => "biome_taiga",
            Self::BiomeLushMeadow => "biome_lush_meadow",
            Self::BiomeSavanna => "biome_savanna",
            Self::BiomeDesert => "biome_desert",
            Self::BiomeSwamp => "biome_swamp",
            Self::BiomeTundra => "biome_tundra",
            Self::BiomeAlpine => "biome_alpine",
            Self::BiomeSnowfield => "biome_snowfield",
            Self::BiomeCoast => "biome_coast",
        }
    }

    fn planet_focus(self) -> Option<PlanetCameraFocus> {
        match self {
            Self::PlanetCoast => Some(PlanetCameraFocus::sera_coast()),
            Self::PlanetSeam => Some(PlanetCameraFocus::sera_seam()),
            Self::BiomeTemperateForest => {
                Some(biome_focus(CubeFace::NegX, 28_569, 9_776, 13.5, 0.5))
            }
            Self::BiomeJungle => Some(biome_focus(CubeFace::NegX, 4_400, 16_208, 0.5, 0.5)),
            Self::BiomeTaiga => Some(biome_focus(CubeFace::NegY, 5_813, 6_124, 0.5, 16.5)),
            Self::BiomeLushMeadow => Some(biome_focus(CubeFace::NegY, 2_293, 1_964, 0.5, 16.5)),
            Self::BiomeSavanna => Some(biome_focus(CubeFace::PosX, 1_135, 5_967, 50.5, 54.5)),
            Self::BiomeDesert => Some(biome_focus(CubeFace::NegX, 5_016, 27_440, 49.5, 0.5)),
            Self::BiomeSwamp => Some(biome_focus(CubeFace::PosX, 5_648, 13_392, 0.5, 24.5)),
            Self::BiomeTundra => Some(biome_focus(CubeFace::NegY, 785, 2_064, 15.5, 0.5)),
            Self::BiomeAlpine => Some(biome_focus(CubeFace::PosX, 27_683, 6_000, 39.5, 0.5)),
            Self::BiomeSnowfield => Some(biome_focus(CubeFace::PosY, 21_781, 22_444, 0.5, 16.5)),
            Self::BiomeCoast => Some(biome_focus(CubeFace::NegY, 31_555, 29_168, 41.5, 0.5)),
            Self::InventoryOpen
            | Self::InventoryHover
            | Self::InventoryMenu
            | Self::RiverScene
            | Self::CombatHud
            | Self::InspectBridge
            | Self::RiverEdge => None,
        }
    }

    fn is_planet_scene(self) -> bool {
        self.planet_focus().is_some()
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|scene| scene.name() == name)
    }

    /// The cursor steps that bring the UI into this scene, in order.
    fn steps(self) -> Vec<Step> {
        let open = Step::Click(Target::InventoryToggle);
        match self {
            Self::InventoryOpen => vec![open],
            Self::InventoryHover => vec![open, Step::Hover(Target::Slot("painters_supplies"))],
            Self::InventoryMenu => vec![open, Step::Click(Target::Slot("longsword"))],
            Self::InspectBridge => {
                vec![Step::RightClick(Target::Square(river_crossing_pos(10, 10)))]
            }
            Self::RiverScene | Self::CombatHud | Self::RiverEdge => Vec::new(),
            _ if self.is_planet_scene() => Vec::new(),
            _ => Vec::new(),
        }
    }
}

/// Registers the protocol messages as plain engine messages, so plugins that read or write
/// them run without a network. Replicated components need no registration offline.
struct OfflineProtocol<'a>(&'a mut App);

impl ProtocolRegistrar for OfflineProtocol<'_> {
    fn component<C>(&mut self)
    where
        C: Component<Mutability = Mutable> + Serialize + DeserializeOwned,
    {
    }

    fn client_message<M>(&mut self, _delivery: Delivery)
    where
        M: Message + Serialize + DeserializeOwned,
    {
        self.0.add_message::<M>();
    }

    fn server_message<M>(&mut self, _delivery: Delivery)
    where
        M: Message + Serialize + DeserializeOwned,
    {
        self.0.add_message::<M>();
    }
}

/// Progress through the active scene (and any queued planet/biome scenes in `--batch` mode).
#[derive(Resource, Debug)]
struct Driver {
    steps: Vec<Step>,
    next_step: usize,
    frames_to_wait: u32,
    frame: u32,
    cover_ready_frames: u32,
    quiet_clutter_frames: u32,
    release_pending: Option<MouseButton>,
    screenshot_taken: bool,
    saved_current: bool,
    output: PathBuf,
    remaining_planet_scenes: VecDeque<(Scene, PathBuf)>,
}

impl Driver {
    fn new(
        scene: Scene,
        output: PathBuf,
        remaining_planet_scenes: VecDeque<(Scene, PathBuf)>,
    ) -> Self {
        Self {
            steps: scene.steps(),
            next_step: 0,
            frames_to_wait: SETTLE_FRAMES,
            frame: 0,
            cover_ready_frames: 0,
            quiet_clutter_frames: 0,
            release_pending: None,
            screenshot_taken: false,
            saved_current: false,
            output,
            remaining_planet_scenes,
        }
    }

    fn reset_for_planet_scene(&mut self, scene: Scene, output: PathBuf) {
        self.steps = scene.steps();
        self.next_step = 0;
        self.frames_to_wait = PLANET_SWITCH_SETTLE_FRAMES;
        self.frame = 0;
        self.cover_ready_frames = 0;
        self.quiet_clutter_frames = 0;
        self.release_pending = None;
        self.screenshot_taken = false;
        self.saved_current = false;
        self.output = output;
    }
}

/// The off-screen image the camera renders to.
#[derive(Resource, Debug)]
struct ShotImage(Handle<Image>);

/// Marks the top-left caption pill spawned on macro overview planet scenes.
#[derive(Component, Debug)]
struct PlanetCaptionNode;

/// Queries streamed chunk & ground-clutter state so screenshots never capture half-populated
/// chunks when `GroundClutterPlugin` throttles chunk builds per frame.
// `SystemParam` derive: bevy_ecs-0.20.1/src/system/system_param.rs:26.
#[derive(SystemParam)]
struct TerrainReadiness<'w, 's> {
    stats: Option<Res<'w, PlanetStreamStats>>,
    clutter_assets: Option<Res<'w, GroundClutterAssets>>,
    covered_chunks: Query<'w, 's, &'static ChunkGroundCover>,
    clutter_layers: Query<'w, 's, (), With<GroundClutterLayer>>,
    added_layers: Query<'w, 's, (), Added<GroundClutterLayer>>,
    added_sprites: Query<'w, 's, (), Added<GroundClutter>>,
}

impl TerrainReadiness<'_, '_> {
    /// Updates readiness counters in `driver` and returns `true` only once every chunk in the
    /// active streaming ring has finished generating AND all ground clutter has settled.
    fn poll_ready(&self, driver: &mut Driver, focus: Option<&PlanetCameraFocus>) -> bool {
        let clutter_spawning = !self.added_layers.is_empty() || !self.added_sprites.is_empty();
        if clutter_spawning {
            driver.quiet_clutter_frames = 0;
        } else {
            driver.quiet_clutter_frames = driver.quiet_clutter_frames.saturating_add(1);
        }

        let (Some(focus), Some(stats)) = (focus, &self.stats) else {
            return true;
        };
        let ring = streamed_ring(focus);
        if stats.pending_chunks > 0 || stats.in_flight_tasks > 0 || stats.active_chunks < ring.len()
        {
            driver.cover_ready_frames = 0;
            return false;
        }

        if focus.tile_pixel_size < SPLAT_MIN_TILE_PIXELS {
            return true;
        }

        if self.clutter_assets.is_none() {
            driver.cover_ready_frames = 0;
            return false;
        }

        let ring_covered = ring
            .iter()
            .all(|slot| self.covered_chunks.iter().any(|c| c.coord == slot.coord));
        if !ring_covered {
            driver.cover_ready_frames = 0;
            return false;
        }

        driver.cover_ready_frames = driver.cover_ready_frames.saturating_add(1);
        let total_covered = self.covered_chunks.iter().count();
        let layers_built = self.clutter_layers.iter().count();
        // `MAX_CLUTTER_CHUNKS_PER_FRAME` is at least 2, so `div_ceil(2) + 4` frames guarantees
        // every queued chunk has been processed even on chunks with 0 clutter sprites.
        let budget_frames = u32::try_from(total_covered.div_ceil(2))
            .unwrap_or(40)
            .saturating_add(4);
        let all_chunks_built =
            layers_built >= total_covered || driver.cover_ready_frames >= budget_frames;

        all_chunks_built && driver.quiet_clutter_frames >= CLUTTER_QUIET_FRAMES
    }
}

/// The laid-out UI nodes a cursor step can point at. A new target adds its query here.
// `SystemParam` derive: bevy_ecs-0.20.1/src/system/system_param.rs:26.
#[derive(SystemParam)]
struct SceneLayout<'w, 's> {
    toggles: Query<
        'w,
        's,
        (&'static ComputedNode, &'static UiGlobalTransform),
        With<InventoryToggleButton>,
    >,
    slots: Query<
        'w,
        's,
        (
            &'static ComputedNode,
            &'static UiGlobalTransform,
            &'static InventorySlot,
        ),
    >,
    holders: Query<'w, 's, (&'static EntityId, &'static Inventory)>,
    camera: Query<'w, 's, (&'static Camera, &'static GlobalTransform), With<Camera2d>>,
    settings: Res<'w, ClientSettings>,
    origin: Res<'w, FloatingOrigin>,
}

fn catalog_id(text: &str) -> CatalogId {
    CatalogId::new(text).expect("scene catalog ids are valid SRD ids")
}

/// The local character's inventory: every stack of `CARRIED`, the first one equipped.
fn carried_inventory() -> Inventory {
    let mut inventory = Inventory::default();
    let stacks: Vec<_> = CARRIED
        .iter()
        .map(|(id, quantity)| {
            inventory
                .add(Item::Catalog(catalog_id(id)), *quantity)
                .expect("an empty inventory takes every scene item")
        })
        .collect();
    inventory
        .set_equipped(stacks[0], true)
        .expect("the stack was just added");
    inventory
}

/// Which scene is currently being rendered.
#[derive(Resource, Clone, Copy, Debug)]
struct ActiveScene(Scene);

/// Spawns what the server would replicate: the map area, river, wooden bridge, oil barrel,
/// goblin, the local character, and a second Player Character beside it so `Give` has a
/// recipient. Then assigns the local character.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn spawn_fake_world(
    mut commands: Commands,
    scene: Res<ActiveScene>,
    mut assignments: MessageWriter<PlayerAssigned>,
    mut logs: MessageWriter<NarrationLogEntry>,
    mut rolls: MessageWriter<DiceRollShown>,
    mut statuses: MessageWriter<PromptStatusUpdate>,
) {
    // Matches `zeta_server::world::scene` (River Crossing anchored at `RIVER_CROSSING_ORIGIN`
    // inside Sera's `FACE_TILES x FACE_TILES` face) so screenshots verify exact live geometry.
    commands.spawn(MapArea(MapBounds {
        width: FACE_TILES,
        height: FACE_TILES,
    }));
    let mut next_id = 10_u64;
    for row in 0..20 {
        if row == 10 {
            continue;
        }
        commands.spawn((
            EntityId(next_id),
            river_crossing_pos(10, row),
            EntityName("River".to_owned()),
            Tags(
                ["water".to_owned(), "deep".to_owned()]
                    .into_iter()
                    .collect(),
            ),
            Collider { passable: false },
        ));
        next_id += 1;
    }
    commands.spawn((
        EntityId(next_id),
        river_crossing_pos(10, 10),
        EntityName("Wooden Bridge".to_owned()),
        Visual("A narrow weathered timber plank bridge spanning the river.".to_owned()),
        Tags(["wooden".to_owned()].into_iter().collect()),
        Collider { passable: true },
    ));
    commands.spawn((
        EntityId(next_id + 1),
        river_crossing_pos(8, 9),
        EntityName("Oil Barrel".to_owned()),
        Visual("A wooden barrel of lamp oil, sealed with a cork.".to_owned()),
        Tags(
            ["wooden".to_owned(), "flammable".to_owned()]
                .into_iter()
                .collect(),
        ),
        Collider { passable: false },
    ));
    commands.spawn((
        EntityId(next_id + 2),
        river_crossing_pos(12, 10),
        EntityName("Goblin".to_owned()),
        Visual("A wary goblin watching the far end of the bridge.".to_owned()),
        Creature,
        Collider { passable: false },
        Inventory::default(),
    ));
    commands.spawn((
        LOCAL_CHARACTER,
        if scene.0 == Scene::RiverEdge {
            river_crossing_pos(1, 1)
        } else {
            river_crossing_pos(4, 10)
        },
        EntityName("Aria".to_owned()),
        Visual("An adventurer controlled by a player.".to_owned()),
        Creature,
        PlayerCharacter,
        Collider { passable: false },
        carried_inventory(),
    ));
    commands.spawn((
        EntityId(2),
        river_crossing_pos(5, 10),
        EntityName("Bram".to_owned()),
        Visual("An adventurer controlled by a player.".to_owned()),
        Creature,
        PlayerCharacter,
        Collider { passable: false },
        Inventory::default(),
    ));
    assignments.write(PlayerAssigned {
        character: LOCAL_CHARACTER,
    });
    if scene.0 == Scene::CombatHud {
        spawn_combat_hud_state(
            &mut commands,
            EntityId(next_id + 2),
            &mut logs,
            &mut rolls,
            &mut statuses,
        );
    }
}

fn spawn_combat_hud_state(
    commands: &mut Commands,
    goblin: EntityId,
    logs: &mut MessageWriter<NarrationLogEntry>,
    rolls: &mut MessageWriter<DiceRollShown>,
    statuses: &mut MessageWriter<PromptStatusUpdate>,
) {
    commands.spawn(TurnBubble {
        order: vec![LOCAL_CHARACTER, goblin],
        current: 0,
        completed_rounds: 0,
        movement_used: false,
        prompt_used: false,
    });
    logs.write(NarrationLogEntry {
        sequence: 1,
        kind: LogEntryKind::Narration,
        speaker: None,
        speaker_name: None,
        text: "Cold river mist drifts over the timber bridge as the goblin eyes the barrel."
            .to_owned(),
        vol: None,
    });
    logs.write(NarrationLogEntry {
        sequence: 2,
        kind: LogEntryKind::Speech,
        speaker: Some(EntityId(2)),
        speaker_name: Some("Bram".to_owned()),
        text: "Hold the bridge - I'll flank left!".to_owned(),
        vol: Some(Volume::Normal),
    });
    rolls.write(DiceRollShown {
        sequence: 3,
        actor: LOCAL_CHARACTER,
        natural: D20::new(16).expect("16 is a valid d20 face"),
        modifier: 3,
        dc: 14,
        reason: "Athletics: shove the oil barrel".to_owned(),
        success: true,
    });
    statuses.write(PromptStatusUpdate {
        state: PromptState::Rejected,
        reason: Some(
            "AI provider rate-limited or quota exceeded (HTTP 429): Quota exceeded.".to_owned(),
        ),
    });
}

/// Points every camera at an off-screen image of the window's size.
fn render_off_screen(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    cameras: Query<Entity, With<Camera>>,
) {
    // `Image::new_target_texture`: bevy_image-0.20.1/src/image.rs:1253.
    let image = images.add(Image::new_target_texture(
        IMAGE_WIDTH,
        IMAGE_HEIGHT,
        TextureFormat::Rgba8UnormSrgb,
        None,
    ));
    for camera in &cameras {
        // `From<Handle<Image>> for RenderTarget`: bevy_camera-0.20.1/src/camera.rs:1024. UI
        // picks a camera that renders to the primary window unless one is marked
        // `IsDefaultUiCamera` (bevy_ui-0.20.1/src/ui_node.rs:3450, chosen at :3460).
        commands
            .entity(camera)
            .insert((RenderTarget::from(image.clone()), IsDefaultUiCamera));
    }
    commands.insert_resource(ShotImage(image));
}

/// The center, in physical pixels, of the laid-out node the target names.
fn target_center(target: Target, layout: &SceneLayout) -> Option<Vec2> {
    let laid_out = |node: &ComputedNode| node.size() != Vec2::ZERO;
    match target {
        Target::InventoryToggle => layout
            .toggles
            .iter()
            .find(|(node, _)| laid_out(node))
            .map(|(_, transform)| transform.translation),
        Target::Slot(id) => {
            let item = Item::Catalog(catalog_id(id));
            let (_, inventory) = layout
                .holders
                .iter()
                .find(|(id, _)| **id == LOCAL_CHARACTER)?;
            let stack = inventory
                .stacks()
                .iter()
                .find(|stack| *stack.item() == item)?;
            layout
                .slots
                .iter()
                .find(|(node, _, slot)| slot.0 == stack.id() && laid_out(node))
                .map(|(_, transform, _)| transform.translation)
        }
        Target::Square(square) => {
            let (camera, camera_transform) = layout.camera.iter().next()?;
            let world =
                square_center_at(square, layout.settings.tile_size, *layout.origin).extend(0.0);
            camera.world_to_viewport(camera_transform, world).ok()
        }
    }
}

/// Saves the screenshot with Bevy's `save_to_disk`, then either advances the batch driver or
/// exits (successfully only when the file exists).
fn save_then_advance_or_exit(
    output: PathBuf,
) -> impl FnMut(On<ScreenshotCaptured>, ResMut<Driver>, MessageWriter<AppExit>) {
    // `save_to_disk`: bevy_render-0.20.1/src/view/window/screenshot.rs:145.
    let mut save = save_to_disk(output.clone());
    move |captured, mut driver, mut exits| {
        save(captured);
        if !output.is_file() {
            eprintln!("could not save {}", output.display());
            exits.write(AppExit::error());
            return;
        }
        println!("saved {}", output.display());
        if driver.remaining_planet_scenes.is_empty() {
            exits.write(AppExit::Success);
        } else {
            driver.saved_current = true;
        }
    }
}

/// Runs in `First`, before input is read: performs the next cursor step once its target is
/// laid out, waits for the UI and streamed clutter to settle, then takes the screenshot.
#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "Bevy system parameters are passed by value"
)]
fn drive_scene(
    mut commands: Commands,
    mut driver: ResMut<Driver>,
    image: Res<ShotImage>,
    mut window: Single<(Entity, &mut Window), With<PrimaryWindow>>,
    layout: SceneLayout,
    focus: Option<Res<PlanetCameraFocus>>,
    readiness: TerrainReadiness,
    mut buttons: MessageWriter<MouseButtonInput>,
    mut exits: MessageWriter<AppExit>,
) {
    let (window_entity, window) = &mut *window;
    driver.frame += 1;
    let terrain_ready = readiness.poll_ready(&mut driver, focus.as_deref());
    if driver.frame > TIMEOUT_FRAMES {
        eprintln!(
            "timed out at step {} of {} (terrain_ready={terrain_ready})",
            driver.next_step,
            driver.steps.len()
        );
        exits.write(AppExit::error());
        return;
    }
    if let Some(button) = driver.release_pending.take() {
        // `MouseButtonInput`: bevy_input-0.20.1/src/mouse.rs:46.
        buttons.write(MouseButtonInput {
            button,
            state: ButtonState::Released,
            window: *window_entity,
        });
    }
    if driver.frames_to_wait > 0 {
        driver.frames_to_wait -= 1;
        return;
    }
    let Some(step) = driver.steps.get(driver.next_step).copied() else {
        if !terrain_ready {
            return;
        }
        if !driver.screenshot_taken {
            // `Screenshot::image`: bevy_render-0.20.1/src/view/window/screenshot.rs:107.
            commands
                .spawn(Screenshot::image(image.0.clone()))
                .observe(save_then_advance_or_exit(driver.output.clone()));
            driver.screenshot_taken = true;
        }
        return;
    };
    let (target, click) = match step {
        Step::Hover(target) => (target, None),
        Step::Click(target) => (target, Some(MouseButton::Left)),
        Step::RightClick(target) => (target, Some(MouseButton::Right)),
    };
    let Some(center) = target_center(target, &layout) else {
        return;
    };
    // `Window::set_physical_cursor_position`: bevy_window-0.20.1/src/window.rs:659. Without
    // the winit plugin nothing moves the real cursor or overwrites this position.
    window.set_physical_cursor_position(Some(center.as_dvec2()));
    if let Some(button) = click {
        buttons.write(MouseButtonInput {
            button,
            state: ButtonState::Pressed,
            window: *window_entity,
        });
        driver.release_pending = Some(button);
    }
    driver.next_step += 1;
    driver.frames_to_wait = STEP_FRAMES;
}

fn planet_caption_label(scene: Scene, focus: &PlanetCameraFocus) -> Option<String> {
    let tile = focus_tile(focus);
    match scene {
        Scene::PlanetCoast => Some(format!(
            "Sera Coastline  |  face: {}  chunk: ({}, {})  tile: ({}, {})",
            tile.face, focus.anchor.cx, focus.anchor.cy, tile.x, tile.y
        )),
        Scene::PlanetSeam => Some(format!(
            "Sera Cube Seam: neg_x <-> pos_y (90 deg)  |  chunk: ({}, {})  tile: ({}, {})",
            focus.anchor.cx, focus.anchor.cy, tile.x, tile.y
        )),
        // Tactical `biome_*` scenes render 100% unobstructed battlemap terrain so neither
        // human inspection nor `verify_biomes.py` metrics are polluted by HUD overlays.
        _ => None,
    }
}

fn sync_planet_caption(
    commands: &mut Commands,
    existing: &Query<Entity, With<PlanetCaptionNode>>,
    scene: Scene,
    focus: &PlanetCameraFocus,
) {
    for entity in existing {
        // `EntityCommands::despawn`: bevy_ecs-0.20.1/src/system/commands/mod.rs:1882.
        commands.entity(entity).despawn();
    }
    let Some(label) = planet_caption_label(scene, focus) else {
        return;
    };
    commands
        .spawn((
            PlanetCaptionNode,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(12.0),
                left: Val::Px(12.0),
                padding: UiRect::axes(Val::Px(12.0), Val::Px(8.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.06, 0.08, 0.11, 0.80)),
        ))
        .with_child((
            Text::new(label),
            TextFont {
                // `FontSize::Px`: bevy_text-0.20.1/src/text.rs:783.
                font_size: FontSize::Px(14.0),
                ..default()
            },
            TextColor(Color::srgb(0.95, 0.94, 0.90)),
        ));
}

/// Spawns a compact top-left caption pill for macro overview planet screenshots without
/// overlapping the coastline or face seam in the center of the viewport.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn spawn_planet_caption(
    mut commands: Commands,
    scene: Res<ActiveScene>,
    focus: Res<PlanetCameraFocus>,
    existing: Query<Entity, With<PlanetCaptionNode>>,
) {
    sync_planet_caption(&mut commands, &existing, scene.0, &focus);
}

/// Waits for streamed planet chunks and ground clutter to settle, captures the off-screen
/// screenshot, and advances through any remaining `--batch` planet/biome scenes.
#[expect(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "Bevy system parameters are passed by value"
)]
fn drive_planet_scene(
    mut commands: Commands,
    mut driver: ResMut<Driver>,
    mut active_scene: ResMut<ActiveScene>,
    mut focus: ResMut<PlanetCameraFocus>,
    captions: Query<Entity, With<PlanetCaptionNode>>,
    readiness: TerrainReadiness,
    image: Res<ShotImage>,
    mut exits: MessageWriter<AppExit>,
) {
    if driver.saved_current {
        let Some((next_scene, next_output)) = driver.remaining_planet_scenes.pop_front() else {
            exits.write(AppExit::Success);
            return;
        };
        let next_focus = next_scene
            .planet_focus()
            .expect("remaining_planet_scenes holds only planet/biome scenes");
        *active_scene = ActiveScene(next_scene);
        *focus = next_focus;
        sync_planet_caption(&mut commands, &captions, next_scene, &next_focus);
        driver.reset_for_planet_scene(next_scene, next_output);
        return;
    }

    driver.frame += 1;
    let terrain_ready = readiness.poll_ready(&mut driver, Some(&focus));
    if driver.frame > TIMEOUT_FRAMES {
        eprintln!(
            "planet scene {} timed out (terrain_ready={terrain_ready})",
            active_scene.0.name()
        );
        exits.write(AppExit::error());
        return;
    }
    if driver.frames_to_wait > 0 {
        driver.frames_to_wait -= 1;
        return;
    }
    if !terrain_ready {
        return;
    }
    if !driver.screenshot_taken {
        // `Screenshot::image`: bevy_render-0.20.1/src/view/window/screenshot.rs:107.
        commands
            .spawn(Screenshot::image(image.0.clone()))
            .observe(save_then_advance_or_exit(driver.output.clone()));
        driver.screenshot_taken = true;
    }
}

fn usage() -> String {
    let names: Vec<&str> = Scene::ALL.iter().map(|scene| scene.name()).collect();
    format!(
        "usage: ui_shot <scene> <out.png>, ui_shot {BATCH_FLAG} <out_dir> [scene...], or ui_shot {LIST_FLAG}\nscenes: {}",
        names.join(", ")
    )
}

fn prepare_output_file(output: &Path) -> Result<(), String> {
    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    if let Err(error) = std::fs::remove_file(output)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return Err(format!("cannot replace {}: {error}", output.display()));
    }
    Ok(())
}

fn build_base_app() -> App {
    let mut app = App::new();
    // `PluginGroupBuilder::disable`: bevy_app-0.20.1/src/plugin_group.rs:551. The window
    // entity stays, for the cursor and the UI systems that read it, but no real window opens.
    app.add_plugins(
        DefaultPlugins
            .build()
            .set(zeta_client::terrain::client_asset_plugin())
            .set(WindowPlugin {
                primary_window: Some(Window {
                    // `WindowResolution::new`: bevy_window-0.20.1/src/window.rs:923.
                    resolution: WindowResolution::new(IMAGE_WIDTH, IMAGE_HEIGHT),
                    ..default()
                }),
                ..default()
            })
            .disable::<WinitPlugin>()
            // `RenderPlugin::synchronous_pipeline_compilation`: bevy_render-0.20.1/src/lib.rs:164.
            // Shaders compile before the first frame draws, so the screenshot never catches a
            // frame drawn before its pipelines were ready.
            .set(RenderPlugin {
                synchronous_pipeline_compilation: true,
                ..default()
            }),
    )
    // `ScheduleRunnerPlugin::run_loop`: bevy_app-0.20.1/src/schedule_runner.rs:64.
    .add_plugins(ScheduleRunnerPlugin::run_loop(FRAME_WAIT));
    app
}

fn run_single_ui_scene(scene: Scene, output: PathBuf) -> AppExit {
    let mut app = build_base_app();
    let driver = Driver::new(scene, output, VecDeque::new());
    register_protocol(&mut OfflineProtocol(&mut app));
    app.add_plugins(ClientPlugins)
        .insert_resource(PlanetStreamSyncMode::Blocking)
        .insert_resource(ActiveScene(scene))
        .insert_resource(driver)
        .add_systems(Startup, spawn_fake_world)
        .add_systems(PostStartup, render_off_screen)
        .add_systems(First, drive_scene);
    app.run()
}

fn run_planet_scenes(mut queue: VecDeque<(Scene, PathBuf)>) -> AppExit {
    let Some((first_scene, first_output)) = queue.pop_front() else {
        return AppExit::Success;
    };
    let focus = first_scene
        .planet_focus()
        .expect("run_planet_scenes only receives planet/biome scenes");
    let mut app = build_base_app();
    let driver = Driver::new(first_scene, first_output, queue);
    app.insert_resource(ClearColor(Color::srgb(0.05, 0.09, 0.14)))
        .insert_resource(ActiveScene(first_scene))
        .insert_resource(focus)
        .insert_resource(PlanetStreamSyncMode::Blocking)
        .insert_resource(driver)
        .add_plugins((PlanetStreamPlugin, GroundClutterPlugin))
        .add_systems(Startup, spawn_planet_caption)
        .add_systems(PostStartup, render_off_screen)
        .add_systems(First, drive_planet_scene);
    app.run()
}

fn run_batch(out_dir: &Path, scene_names: &[String]) -> AppExit {
    let scenes: Vec<Scene> = if scene_names.is_empty() {
        Scene::ALL.to_vec()
    } else {
        let mut parsed = Vec::with_capacity(scene_names.len());
        for name in scene_names {
            let Some(scene) = Scene::from_name(name) else {
                eprintln!("unknown scene {name}\n{}", usage());
                return AppExit::error();
            };
            parsed.push(scene);
        }
        parsed
    };

    if let Err(error) = std::fs::create_dir_all(out_dir) {
        eprintln!("cannot create {}: {error}", out_dir.display());
        return AppExit::error();
    }

    let mut ui_jobs = Vec::new();
    let mut planet_jobs = VecDeque::new();
    for scene in scenes {
        let output = out_dir.join(format!("{}.png", scene.name()));
        if let Err(error) = prepare_output_file(&output) {
            eprintln!("{error}");
            return AppExit::error();
        }
        if scene.is_planet_scene() {
            planet_jobs.push_back((scene, output));
        } else {
            ui_jobs.push((scene, output));
        }
    }

    if !ui_jobs.is_empty() {
        let exe = match std::env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                eprintln!("cannot locate ui_shot binary: {error}");
                return AppExit::error();
            }
        };
        for (scene, output) in ui_jobs {
            let status = Command::new(&exe).arg(scene.name()).arg(&output).status();
            match status {
                Ok(code) if code.success() && output.is_file() => {}
                Ok(code) => {
                    eprintln!("scene {} failed ({code})", scene.name());
                    return AppExit::error();
                }
                Err(error) => {
                    eprintln!(
                        "could not launch {} for {}: {error}",
                        exe.display(),
                        scene.name()
                    );
                    return AppExit::error();
                }
            }
        }
    }

    run_planet_scenes(planet_jobs)
}

fn main() -> AppExit {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments == [LIST_FLAG] {
        for scene in Scene::ALL {
            println!("{}", scene.name());
        }
        return AppExit::Success;
    }
    if arguments.first().map(String::as_str) == Some(BATCH_FLAG) {
        let Some(out_dir) = arguments.get(1) else {
            eprintln!("{}", usage());
            return AppExit::error();
        };
        return run_batch(Path::new(out_dir), &arguments[2..]);
    }
    let [scene_name, output] = arguments.as_slice() else {
        eprintln!("{}", usage());
        return AppExit::error();
    };
    let Some(scene) = Scene::from_name(scene_name) else {
        eprintln!("unknown scene {scene_name}\n{}", usage());
        return AppExit::error();
    };
    let output = PathBuf::from(output);
    if let Err(error) = prepare_output_file(&output) {
        eprintln!("{error}");
        return AppExit::error();
    }
    if scene.is_planet_scene() {
        run_planet_scenes(VecDeque::from([(scene, output)]))
    } else {
        run_single_ui_scene(scene, output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scene_is_found_by_its_name_and_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for scene in Scene::ALL {
            assert!(
                seen.insert(scene.name()),
                "duplicate scene {}",
                scene.name()
            );
            assert_eq!(Scene::from_name(scene.name()), Some(scene));
        }
        assert_eq!(Scene::from_name("no_such_scene"), None);
    }

    #[test]
    fn biome_scenes_use_tactical_zoom_and_valid_cube_coordinates() {
        let biome_count = Scene::ALL
            .into_iter()
            .filter(|scene| scene.name().starts_with("biome_"))
            .map(|scene| {
                let focus = scene.planet_focus().expect("biome scene has planet focus");
                assert!(
                    (48.0..=64.0).contains(&focus.tile_pixel_size),
                    "{} tile_pixel_size {} outside tactical range",
                    scene.name(),
                    focus.tile_pixel_size
                );
                assert!(
                    ChunkCoord::new(focus.anchor.face, focus.anchor.cx, focus.anchor.cy).is_some(),
                    "{} chunk out of face bounds",
                    scene.name()
                );
                assert_eq!(planet_caption_label(scene, &focus), None);
            })
            .count();
        assert_eq!(biome_count, 11);
    }

    #[test]
    fn the_carried_inventory_has_every_item_and_an_equipped_longsword() {
        let inventory = carried_inventory();
        assert_eq!(inventory.stacks().len(), CARRIED.len());
        let first = &inventory.stacks()[0];
        assert_eq!(*first.item(), Item::Catalog(catalog_id("longsword")));
        assert!(first.equipped());
    }

    #[test]
    fn every_slot_target_is_carried() {
        let inventory = carried_inventory();
        for step in Scene::ALL.into_iter().flat_map(Scene::steps) {
            let (Step::Hover(Target::Slot(id)) | Step::Click(Target::Slot(id))) = step else {
                continue;
            };
            assert!(
                inventory.contains_catalog(&catalog_id(id)),
                "{id} is carried"
            );
        }
    }
}
