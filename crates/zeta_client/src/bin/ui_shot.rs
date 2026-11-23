//! Dev tool: renders the real client UI in 1 scene to a PNG file, then exits.
//!
//! Usage: `cargo run -p zeta_client --bin ui_shot --offline -- <scene> <out.png>`. Run it
//! without arguments to list the scenes; `tools/ui_shots.ps1` renders all of them.
//!
//! It builds the game's own `ClientPlugins` and fakes only 2 things: the network (protocol
//! messages are plain engine messages, and the scene spawns the entities the server would
//! replicate) and the mouse cursor. No server runs and no window opens: the camera renders to
//! an off-screen image of the window's size, and Bevy's screenshot API saves that image. The
//! game binary never uses this file.
//!
//! Adding a scene: add a `Scene` variant, give it a name in `Scene::name` and `Scene::ALL`,
//! and list its cursor steps in `Scene::steps`.

use std::path::PathBuf;
use std::time::Duration;

use bevy::app::ScheduleRunnerPlugin;
use bevy::camera::RenderTarget;
use bevy::ecs::component::Mutable;
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
use zeta_client::inventory::{InventorySlot, InventoryToggleButton};
use zeta_core::catalog::CatalogId;
use zeta_core::entity::EntityId;
use zeta_core::grid::{GridPos, MapBounds};
use zeta_core::protocol::{
    Collider, Delivery, EntityName, Inventory, Item, MapArea, PlayerAssigned, PlayerCharacter,
    ProtocolRegistrar, register_protocol,
};

/// Width of the rendered image in pixels, at a scale factor of 1.
const IMAGE_WIDTH: u32 = 1280;
/// Height of the rendered image in pixels.
const IMAGE_HEIGHT: u32 = 720;
/// Frames before the first cursor step, so the fake world replicates and lays out.
const SETTLE_FRAMES: u32 = 10;
/// Frames after each cursor step, so the UI reacts and lays out again.
const STEP_FRAMES: u32 = 6;
/// Frames after which a scene that never finishes fails.
const TIMEOUT_FRAMES: u32 = 600;
/// Pause between frames; the tool runs without a window, so nothing else paces it.
const FRAME_WAIT: Duration = Duration::from_millis(5);
/// Argument that prints the scene names, 1 per line, for `tools/ui_shots.ps1`.
const LIST_FLAG: &str = "--list";

/// The local character's id in every scene.
const LOCAL_CHARACTER: EntityId = EntityId(1);
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

/// 1 state of the UI the tool can render.
#[allow(
    clippy::enum_variant_names,
    reason = "each name starts with the panel it shows; scenes of other panels come later"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scene {
    /// The inventory panel open, cursor away.
    InventoryOpen,
    /// The cursor over the Painter's Supplies slot, so its tooltip shows.
    InventoryHover,
    /// The longsword slot clicked, so its action menu shows.
    InventoryMenu,
}

/// What the fake cursor points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// The button that opens the inventory panel.
    InventoryToggle,
    /// The slot of the local character's stack of this catalog id.
    Slot(&'static str),
}

/// 1 action of the fake cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// Move the cursor over the target and stay there.
    Hover(Target),
    /// Move the cursor over the target and click the left button.
    Click(Target),
}

impl Scene {
    /// Every scene, in the order `tools/ui_shots.ps1` lists them.
    const ALL: [Self; 3] = [
        Self::InventoryOpen,
        Self::InventoryHover,
        Self::InventoryMenu,
    ];

    /// The name given on the command line, which is also the PNG file name.
    fn name(self) -> &'static str {
        match self {
            Self::InventoryOpen => "inventory_open",
            Self::InventoryHover => "inventory_hover",
            Self::InventoryMenu => "inventory_menu",
        }
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

/// Progress through the scene's cursor steps.
#[derive(Resource, Debug)]
struct Driver {
    steps: Vec<Step>,
    next_step: usize,
    frames_to_wait: u32,
    frame: u32,
    release_pending: bool,
    screenshot_taken: bool,
    output: PathBuf,
}

/// The off-screen image the camera renders to.
#[derive(Resource, Debug)]
struct ShotImage(Handle<Image>);

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

/// Spawns what the server would replicate: the map area, the local character, and a second
/// Player Character beside it, so `Give` has a recipient. Then assigns the local character.
fn spawn_fake_world(mut commands: Commands, mut assignments: MessageWriter<PlayerAssigned>) {
    commands.spawn(MapArea(MapBounds {
        width: 12,
        height: 8,
    }));
    commands.spawn((
        LOCAL_CHARACTER,
        GridPos::new(5, 4),
        EntityName("Aria".to_owned()),
        PlayerCharacter,
        Collider { passable: false },
        carried_inventory(),
    ));
    commands.spawn((
        EntityId(2),
        GridPos::new(6, 4),
        EntityName("Bram".to_owned()),
        PlayerCharacter,
        Collider { passable: false },
        Inventory::default(),
    ));
    assignments.write(PlayerAssigned {
        character: LOCAL_CHARACTER,
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
    }
}

/// Saves the screenshot with Bevy's `save_to_disk`, then exits: successfully only when the
/// file exists.
fn save_then_exit(output: PathBuf) -> impl FnMut(On<ScreenshotCaptured>, MessageWriter<AppExit>) {
    // `save_to_disk`: bevy_render-0.20.1/src/view/window/screenshot.rs:145.
    let mut save = save_to_disk(output.clone());
    move |captured, mut exits| {
        save(captured);
        let exit = if output.is_file() {
            println!("saved {}", output.display());
            AppExit::Success
        } else {
            eprintln!("could not save {}", output.display());
            AppExit::error()
        };
        exits.write(exit);
    }
}

/// Runs in `First`, before input is read: performs the next cursor step once its target is
/// laid out, waits for the UI to react, then takes the screenshot.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn drive_scene(
    mut commands: Commands,
    mut driver: ResMut<Driver>,
    image: Res<ShotImage>,
    mut window: Single<(Entity, &mut Window), With<PrimaryWindow>>,
    layout: SceneLayout,
    mut buttons: MessageWriter<MouseButtonInput>,
    mut exits: MessageWriter<AppExit>,
) {
    let (window_entity, window) = &mut *window;
    driver.frame += 1;
    if driver.frame > TIMEOUT_FRAMES {
        eprintln!(
            "timed out at step {} of {}",
            driver.next_step,
            driver.steps.len()
        );
        exits.write(AppExit::error());
        return;
    }
    if driver.release_pending {
        // `MouseButtonInput`: bevy_input-0.20.1/src/mouse.rs:46.
        buttons.write(MouseButtonInput {
            button: MouseButton::Left,
            state: ButtonState::Released,
            window: *window_entity,
        });
        driver.release_pending = false;
    }
    if driver.frames_to_wait > 0 {
        driver.frames_to_wait -= 1;
        return;
    }
    let Some(step) = driver.steps.get(driver.next_step).copied() else {
        if !driver.screenshot_taken {
            // `Screenshot::image`: bevy_render-0.20.1/src/view/window/screenshot.rs:107.
            commands
                .spawn(Screenshot::image(image.0.clone()))
                .observe(save_then_exit(driver.output.clone()));
            driver.screenshot_taken = true;
        }
        return;
    };
    let (target, click) = match step {
        Step::Hover(target) => (target, false),
        Step::Click(target) => (target, true),
    };
    let Some(center) = target_center(target, &layout) else {
        return;
    };
    // `Window::set_physical_cursor_position`: bevy_window-0.20.1/src/window.rs:659. Without
    // the winit plugin nothing moves the real cursor or overwrites this position.
    window.set_physical_cursor_position(Some(center.as_dvec2()));
    if click {
        buttons.write(MouseButtonInput {
            button: MouseButton::Left,
            state: ButtonState::Pressed,
            window: *window_entity,
        });
        driver.release_pending = true;
    }
    driver.next_step += 1;
    driver.frames_to_wait = STEP_FRAMES;
}

fn usage() -> String {
    let names: Vec<&str> = Scene::ALL.iter().map(|scene| scene.name()).collect();
    format!(
        "usage: ui_shot <scene> <out.png>, or ui_shot {LIST_FLAG}\nscenes: {}",
        names.join(", ")
    )
}

fn main() -> AppExit {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments == [LIST_FLAG] {
        for scene in Scene::ALL {
            println!("{}", scene.name());
        }
        return AppExit::Success;
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
    if let Err(error) = std::fs::remove_file(&output)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("cannot replace {}: {error}", output.display());
        return AppExit::error();
    }
    let mut app = App::new();
    // `PluginGroupBuilder::disable`: bevy_app-0.20.1/src/plugin_group.rs:551. The window
    // entity stays, for the cursor and the UI systems that read it, but no real window opens.
    app.add_plugins(
        DefaultPlugins
            .build()
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
    register_protocol(&mut OfflineProtocol(&mut app));
    app.add_plugins(ClientPlugins)
        .insert_resource(Driver {
            steps: scene.steps(),
            next_step: 0,
            frames_to_wait: SETTLE_FRAMES,
            frame: 0,
            release_pending: false,
            screenshot_taken: false,
            output,
        })
        .add_systems(Startup, spawn_fake_world)
        .add_systems(PostStartup, render_off_screen)
        .add_systems(First, drive_scene);
    app.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scene_is_found_by_its_name() {
        for scene in Scene::ALL {
            assert_eq!(Scene::from_name(scene.name()), Some(scene));
        }
        assert_eq!(Scene::from_name("no_such_scene"), None);
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
