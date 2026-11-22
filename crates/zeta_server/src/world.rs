//! The authoritative server world: scene, players, movement, Snapshots, AI DM effects, and Turn Bubbles.
//!
//! Rules: `docs/prototypes/realism_prototype.md` Sections 1 and 2,
//! `docs/architecture/technical_architecture.md` Sections 1, 2 and 5, and
//! `docs/architecture/ai_orchestration.md`. The world never imports the networking library:
//! it reads and writes only the messages in [`crate::net::messages`], and marks replicated
//! entities with [`Networked`].

mod effects;
mod event_log;
mod inventory;
mod movement;
mod players;
mod prompts;
mod scene;
mod snapshot;
mod turn_bubble;

use std::collections::BTreeMap;
use std::marker::PhantomData;

use bevy_app::{App, Plugin, Startup, Update};
use bevy_ecs::prelude::{Entity, IntoScheduleConfigs, Resource};
use zeta_core::ai::AiProvider;
use zeta_core::entity::EntityId;
use zeta_core::protocol::{LogEntryKind, NarrationLogEntry};

use crate::bridge::{CompletedAiResolutions, ResolutionLocksResource};
use crate::dice::{OsDice, RollD20};
use crate::net::messages::{PlayerConnection, Recipients, ToPlayers};

pub use event_log::EventLogWriter;
pub use scene::{TEST_SCENE_BOUNDS, TEST_SCENE_PLACE_NAME};

#[cfg(doc)]
use crate::net::messages::Networked;

/// Adds the server world: the test scene, player characters, movement, Freeform Action
/// Prompts resolved through the AI DM bridge for provider `P`, and Turn Bubbles.
///
/// Needs `crate::bridge::AiBridgePlugin::<P>` and an `AiBridge<P>` resource, and the
/// messages of `crate::net::messages`. Rolls with [`OsDice`] and keeps the Event Log in
/// memory unless the app inserts its own [`DiceRoller`] or [`EventLogWriter`].
pub struct WorldPlugin<P>(PhantomData<fn() -> P>);

impl<P> WorldPlugin<P> {
    /// Creates the plugin.
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<P> Default for WorldPlugin<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P> std::fmt::Debug for WorldPlugin<P> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_tuple("WorldPlugin").finish()
    }
}

impl<P: AiProvider + Send + Sync + 'static> Plugin for WorldPlugin<P> {
    fn build(&self, app: &mut App) {
        app.init_resource::<ResolutionLocksResource>()
            .init_resource::<CompletedAiResolutions>()
            .init_resource::<EntityIndex>()
            .init_resource::<NextEntityId>()
            .init_resource::<LogSequence>()
            .init_resource::<PlayerCharacters>()
            .init_resource::<prompts::PromptRegistry>()
            .init_resource::<prompts::IncomingPrompts>()
            .init_resource::<DiceRoller>()
            .init_resource::<EventLogWriter>()
            .init_resource::<inventory::IncomingItemVerbs>()
            .add_systems(Startup, scene::spawn_test_scene)
            .add_systems(
                Update,
                (
                    players::connect_players,
                    players::disconnect_players,
                    movement::handle_move_requests,
                    inventory::collect_item_verbs,
                    inventory::run_item_verbs,
                    prompts::collect_prompts,
                    turn_bubble::handle_end_turn_requests,
                    prompts::run_resolutions::<P>,
                )
                    .chain(),
            );
    }
}

/// The dice the server rolls for checks.
#[derive(Resource)]
pub struct DiceRoller(Box<dyn RollD20>);

impl DiceRoller {
    /// Rolls with `dice`.
    pub fn new(dice: impl RollD20 + 'static) -> Self {
        Self(Box::new(dice))
    }
}

impl Default for DiceRoller {
    fn default() -> Self {
        Self::new(OsDice)
    }
}

impl std::fmt::Debug for DiceRoller {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_tuple("DiceRoller").finish_non_exhaustive()
    }
}

/// Maps every world entity's permanent identifier to its engine entity.
#[derive(Resource, Default, Debug)]
pub(crate) struct EntityIndex(pub(crate) BTreeMap<EntityId, Entity>);

/// Hands out permanent entity identifiers, never reusing one.
#[derive(Resource, Default, Debug)]
pub(crate) struct NextEntityId(u64);

impl NextEntityId {
    /// The next unused identifier.
    pub(crate) fn allocate(&mut self) -> EntityId {
        let id = EntityId(self.0);
        // 1 identifier per spawned entity; a u64 cannot run out within any server's life.
        self.0 = self
            .0
            .checked_add(1)
            .expect("entity identifiers never run out");
        id
    }
}

/// The 1 sequence shared by narration log lines and dice rolls, so clients can order them.
#[derive(Resource, Default, Debug)]
pub(crate) struct LogSequence(u64);

impl LogSequence {
    /// The next sequence number.
    pub(crate) fn next(&mut self) -> u64 {
        let sequence = self.0;
        // 1 number per log line; a u64 cannot run out within any server's life.
        self.0 = self
            .0
            .checked_add(1)
            .expect("log sequence numbers never run out");
        sequence
    }
}

/// Which character each connected player controls.
#[derive(Resource, Default, Debug)]
pub(crate) struct PlayerCharacters {
    /// Character of each connection.
    pub(crate) by_connection: BTreeMap<PlayerConnection, EntityId>,
    /// Players who ever joined, for numbering new characters.
    pub(crate) joined: u32,
}

/// A notice line for 1 player, such as why a request was refused.
pub(crate) fn notice(
    sequence: &mut LogSequence,
    connection: PlayerConnection,
    text: impl Into<String>,
) -> ToPlayers<NarrationLogEntry> {
    ToPlayers {
        recipients: Recipients::Only(connection),
        message: NarrationLogEntry {
            sequence: sequence.next(),
            kind: LogEntryKind::Notice,
            speaker: None,
            speaker_name: None,
            text: text.into(),
            vol: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use bevy_app::Last;
    use bevy_ecs::prelude::{MessageReader, ResMut, World};
    use zeta_core::ai::{AiExchange, AiResponse, ReplayProvider};
    use zeta_core::catalog::CatalogId;
    use zeta_core::event_log::{ItemChange, ItemChangeKind};
    use zeta_core::grid::{GridPos, MapBounds};
    use zeta_core::protocol::{
        Collider, DiceRollShown, EndTurnRequest, EntityName, FreeformActionPrompt, Inventory, Item,
        ItemStack, ItemVerb, ItemVerbRequest, MapArea, MoveRequest, PlayerAssigned,
        PlayerCharacter, PromptState, PromptStatusUpdate, StackId, Tags, TurnBubble, Visual,
    };
    use zeta_core::resolution::{D20, Volume};
    use zeta_core::snapshot::Snapshot;

    use super::*;
    use crate::bridge::{AiBridge, AiBridgePlugin};
    use crate::dice::FixedDice;
    use crate::net::messages::{
        FromPlayer, NetMessagesPlugin, Networked, PlayerConnected, PlayerDisconnected,
    };

    /// Every message the world sent to players, in order.
    #[derive(Resource, Default)]
    struct Sent {
        assigned: Vec<ToPlayers<PlayerAssigned>>,
        lines: Vec<ToPlayers<NarrationLogEntry>>,
        rolls: Vec<ToPlayers<DiceRollShown>>,
        statuses: Vec<ToPlayers<PromptStatusUpdate>>,
    }

    impl Sent {
        fn states_for(&self, connection: PlayerConnection) -> Vec<PromptState> {
            self.statuses
                .iter()
                .filter(|status| status.recipients == Recipients::Only(connection))
                .map(|status| status.message.state)
                .collect()
        }

        fn has_line(&self, kind: LogEntryKind, text: &str) -> bool {
            self.lines
                .iter()
                .any(|line| line.message.kind == kind && line.message.text == text)
        }
    }

    fn collect_sent(
        mut sent: ResMut<Sent>,
        mut assigned: MessageReader<ToPlayers<PlayerAssigned>>,
        mut lines: MessageReader<ToPlayers<NarrationLogEntry>>,
        mut rolls: MessageReader<ToPlayers<DiceRollShown>>,
        mut statuses: MessageReader<ToPlayers<PromptStatusUpdate>>,
    ) {
        sent.assigned.extend(assigned.read().cloned());
        sent.lines.extend(lines.read().cloned());
        sent.rolls.extend(rolls.read().cloned());
        sent.statuses.extend(statuses.read().cloned());
    }

    fn face(value: u8) -> D20 {
        D20::new(value).expect("a d20 face")
    }

    /// A world with the test scene, replaying `exchanges` and rolling `faces` in turn.
    fn test_app(faces: &[u8]) -> App {
        let mut app = App::new();
        app.add_plugins(NetMessagesPlugin)
            .add_plugins(AiBridgePlugin::<ReplayProvider>::new())
            .add_plugins(WorldPlugin::<ReplayProvider>::new())
            .insert_resource(DiceRoller::new(FixedDice::new(
                faces.iter().map(|&value| face(value)).collect(),
            )))
            .init_resource::<Sent>()
            .add_systems(Last, collect_sent);
        replay(&mut app, Vec::new());
        app.update();
        app
    }

    /// Replaces the AI DM with a replay of `exchanges`.
    fn replay(app: &mut App, exchanges: Vec<AiExchange>) -> Arc<ReplayProvider> {
        let provider = Arc::new(ReplayProvider::new(exchanges));
        let bridge = AiBridge::with_owned_runtime(Arc::clone(&provider)).expect("tokio runtime");
        app.insert_resource(bridge);
        provider
    }

    fn snapshot_of(app: &mut App, actor: EntityId) -> Snapshot {
        snapshot::build_snapshot(app.world_mut(), actor).expect("the actor exists")
    }

    /// The exchange the world will send for `actor` declaring `text` now.
    fn exchange(app: &mut App, actor: EntityId, text: &str, response: &str) -> AiExchange {
        let snapshot = snapshot_of(app, actor);
        let active = turn_bubble::bubble_of(app.world_mut(), actor).is_some();
        let items = prompts::item_context(app.world(), actor, None);
        AiExchange {
            request: prompts::resolution_request(&snapshot, text, active, &items),
            response: AiResponse {
                text: response.to_owned(),
                provider_turn: None,
            },
        }
    }

    /// The Snapshot local identifier of `id`.
    fn local_id(snapshot: &Snapshot, id: EntityId) -> u32 {
        snapshot
            .ai_view()
            .iter()
            .zip(snapshot.entities())
            .find(|(_, entity)| entity.world_id == id)
            .map(|(view, _)| view.id)
            .expect("the entity is in the Snapshot")
    }

    fn named(app: &mut App, name: &str) -> EntityId {
        let mut query = app.world_mut().query::<(&EntityId, &EntityName)>();
        query
            .iter(app.world())
            .find(|(_, entity_name)| entity_name.0 == name)
            .map(|(id, _)| *id)
            .expect("the named entity exists")
    }

    fn engine(app: &App, id: EntityId) -> Option<Entity> {
        app.world().resource::<EntityIndex>().0.get(&id).copied()
    }

    fn position(app: &mut App, id: EntityId) -> GridPos {
        snapshot::live_position(app.world_mut(), id).expect("the entity exists")
    }

    fn connect(app: &mut App) -> (PlayerConnection, EntityId) {
        let connection = PlayerConnection(app.world_mut().spawn_empty().id());
        app.world_mut()
            .write_message(PlayerConnected { connection });
        app.update();
        let character = app
            .world()
            .resource::<PlayerCharacters>()
            .by_connection
            .get(&connection)
            .copied()
            .expect("the player has a character");
        (connection, character)
    }

    fn send<M: Send + Sync + 'static>(app: &mut App, connection: PlayerConnection, message: M) {
        app.world_mut().write_message(FromPlayer {
            connection,
            message,
        });
    }

    fn prompt(app: &mut App, connection: PlayerConnection, text: &str) {
        send(
            app,
            connection,
            FreeformActionPrompt {
                text: text.to_owned(),
                utilize: None,
            },
        );
    }

    fn run_until(app: &mut App, done: impl Fn(&World) -> bool) {
        let start = Instant::now();
        app.update();
        while !done(app.world()) {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "the world did not settle"
            );
            std::thread::sleep(Duration::from_millis(2));
            app.update();
        }
    }

    fn finished_count(world: &World, connection: PlayerConnection) -> usize {
        world
            .resource::<Sent>()
            .states_for(connection)
            .iter()
            .filter(|state| matches!(state, PromptState::Applied | PromptState::Rejected))
            .count()
    }

    fn records(app: &App) -> Vec<zeta_core::event_log::EventRecord> {
        app.world()
            .resource::<EventLogWriter>()
            .memory_log()
            .expect("tests keep the Event Log in memory")
            .records()
            .to_vec()
    }

    fn nothing_locked(app: &App) -> bool {
        let locks = &app.world().resource::<ResolutionLocksResource>().0;
        locks.queued_count() == 0
            && app
                .world()
                .resource::<EntityIndex>()
                .0
                .keys()
                .all(|&id| !locks.is_locked(id))
    }

    #[test]
    fn the_test_scene_spawns_with_1_networked_map_area() {
        let mut app = test_app(&[10]);
        let mut areas = app.world_mut().query::<(&MapArea, &Networked)>();
        let areas: Vec<MapBounds> = areas.iter(app.world()).map(|(area, _)| area.0).collect();
        assert_eq!(areas, vec![TEST_SCENE_BOUNDS]);
        assert_eq!(
            app.world().get_resource::<MapBounds>(),
            Some(&TEST_SCENE_BOUNDS)
        );
        for name in ["Wooden Bridge", "Oil Barrel", "Goblin"] {
            let id = named(&mut app, name);
            let entity = engine(&app, id).expect("indexed");
            assert!(app.world().get::<Networked>(entity).is_some());
        }
    }

    #[test]
    fn a_connecting_player_gets_a_character_and_is_told_which() {
        let mut app = test_app(&[10]);
        let (connection, character) = connect(&mut app);
        let sent = app.world().resource::<Sent>();
        assert_eq!(
            sent.assigned,
            vec![ToPlayers {
                recipients: Recipients::Only(connection),
                message: PlayerAssigned { character },
            }]
        );
        let entity = engine(&app, character).expect("indexed");
        assert!(app.world().get::<PlayerCharacter>(entity).is_some());
        assert!(app.world().get::<Networked>(entity).is_some());
        assert_eq!(position(&mut app, character), GridPos::new(4, 10));

        let (_, second) = connect(&mut app);
        assert_ne!(position(&mut app, second), GridPos::new(4, 10));
    }

    #[test]
    fn movement_follows_free_paths_and_refuses_the_rest() {
        let mut app = test_app(&[10]);
        let (connection, character) = connect(&mut app);

        send(
            &mut app,
            connection,
            MoveRequest {
                target: GridPos::new(6, 10),
            },
        );
        app.update();
        assert_eq!(position(&mut app, character), GridPos::new(6, 10));

        for target in [GridPos::new(8, 9), GridPos::new(10, 5), GridPos::new(19, 0)] {
            let notices_before = app.world().resource::<Sent>().lines.len();
            send(&mut app, connection, MoveRequest { target });
            app.update();
            assert_eq!(position(&mut app, character), GridPos::new(6, 10));
            let sent = app.world().resource::<Sent>();
            assert_eq!(sent.lines.len(), notices_before + 1);
            assert_eq!(
                sent.lines.last().map(|line| line.message.kind),
                Some(LogEntryKind::Notice)
            );
        }
    }

    #[test]
    fn a_checked_prompt_rolls_applies_its_branch_and_is_recorded() {
        let mut app = test_app(&[15, 3]);
        let (connection, character) = connect(&mut app);
        let barrel = named(&mut app, "Oil Barrel");
        let snapshot = snapshot_of(&mut app, character);
        let barrel_local = local_id(&snapshot, barrel);
        let response = format!(
            r#"{{"ok": true, "chk": {{"dc": 10, "reason": "The barrel is heavy."}},
            "fx": [{{"op": "modify", "id": {barrel_local}, "add": ["tipped"]}}],
            "fail": [{{"op": "narrate", "text": "The barrel does not budge."}}]}}"#
        );
        let first = exchange(&mut app, character, "I tip the barrel.", &response);
        let first_provider = replay(&mut app, vec![first]);

        prompt(&mut app, connection, "I tip the barrel.");
        run_until(&mut app, |world| finished_count(world, connection) == 1);
        // The first prompt tagged the barrel, so the second Snapshot differs.
        let second = exchange(&mut app, character, "I tip the barrel again.", &response);
        let provider = replay(&mut app, vec![second]);
        prompt(&mut app, connection, "  I tip the barrel again.  ");
        run_until(&mut app, |world| finished_count(world, connection) == 2);

        assert!(first_provider.is_finished());
        assert!(provider.is_finished());
        let barrel_entity = engine(&app, barrel).expect("indexed");
        let tags = app.world().get::<Tags>(barrel_entity).expect("tags");
        assert!(tags.0.contains("tipped"));

        let sent = app.world().resource::<Sent>();
        let rolls: Vec<(u8, bool)> = sent
            .rolls
            .iter()
            .map(|roll| (roll.message.natural.face(), roll.message.success))
            .collect();
        assert_eq!(rolls, vec![(15, true), (3, false)]);
        assert!(
            sent.rolls
                .iter()
                .all(|roll| roll.recipients == Recipients::All
                    && roll.message.dc == 10
                    && roll.message.actor == character)
        );
        assert!(sent.has_line(LogEntryKind::Action, "I tip the barrel."));
        assert!(sent.has_line(LogEntryKind::Action, "I tip the barrel again."));
        assert!(sent.has_line(LogEntryKind::Narration, "The barrel does not budge."));
        assert_eq!(
            sent.states_for(connection),
            vec![
                PromptState::Resolving,
                PromptState::Applied,
                PromptState::Resolving,
                PromptState::Applied
            ]
        );
        let mut sequences: Vec<u64> = sent
            .lines
            .iter()
            .map(|line| line.message.sequence)
            .chain(sent.rolls.iter().map(|roll| roll.message.sequence))
            .collect();
        let count = sequences.len();
        sequences.sort_unstable();
        sequences.dedup();
        assert_eq!(sequences.len(), count, "1 shared sequence, never reused");

        let records = records(&app);
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|record| {
            let zeta_core::event_log::EventCause::FreeformAction(cause) = &record.cause else {
                return false;
            };
            cause.actor == character && cause.roll.is_some() && cause.exchanges.len() == 1
        }));
        assert!(nothing_locked(&app));
    }

    #[test]
    fn all_8_effects_change_the_world() {
        let mut app = test_app(&[10]);
        let (connection, character) = connect(&mut app);
        let barrel = named(&mut app, "Oil Barrel");
        let goblin = named(&mut app, "Goblin");
        let bridge = named(&mut app, "Wooden Bridge");
        let snapshot = snapshot_of(&mut app, character);
        let [barrel_local, goblin_local, bridge_local] =
            [barrel, goblin, bridge].map(|id| local_id(&snapshot, id));
        let response = format!(
            r#"{{"ok": true, "chk": null, "fail": [], "fx": [
            {{"op": "narrate", "text": "Flames leap up."}},
            {{"op": "speak", "id": {goblin_local}, "text": "Fire!", "vol": "shout"}},
            {{"op": "move", "id": {goblin_local}, "x": 14, "y": 10, "style": "runs"}},
            {{"op": "modify", "id": {barrel_local}, "add": ["burning"], "passable": true,
              "visual": "A burning barrel.",
              "settle": {{"minutes": 5, "add": ["charred"], "remove": ["burning"]}}}},
            {{"op": "spawn", "name": "Smoke", "x": 7, "y": 9, "passable": true,
              "tags": ["obscuring"], "visual": "Thick grey smoke."}},
            {{"op": "destroy", "id": {bridge_local}, "reason": "burned away"}},
            {{"op": "history", "ids": [{barrel_local}], "summary": "The barrel burned.",
              "preserved": false}},
            {{"op": "bubble", "active": true, "ids": [0, {goblin_local}]}}
            ]}}"#
        );
        let first = exchange(&mut app, character, "I light the barrel.", &response);
        replay(&mut app, vec![first]);

        prompt(&mut app, connection, "I light the barrel.");
        run_until(&mut app, |world| finished_count(world, connection) == 1);

        let sent = app.world().resource::<Sent>();
        assert!(sent.has_line(LogEntryKind::Narration, "Flames leap up."));
        let speech = sent
            .lines
            .iter()
            .find(|line| line.message.kind == LogEntryKind::Speech)
            .expect("the goblin spoke");
        assert_eq!(speech.message.speaker, Some(goblin));
        assert_eq!(speech.message.speaker_name.as_deref(), Some("Goblin"));
        assert_eq!(speech.message.vol, Some(Volume::Shout));
        assert_eq!(
            sent.states_for(connection).last(),
            Some(&PromptState::Applied)
        );

        assert_eq!(position(&mut app, goblin), GridPos::new(14, 10));
        let barrel_entity = engine(&app, barrel).expect("indexed");
        let world = app.world();
        assert!(
            world
                .get::<Tags>(barrel_entity)
                .expect("tags")
                .0
                .contains("burning")
        );
        assert!(
            world
                .get::<Collider>(barrel_entity)
                .expect("collider")
                .passable
        );
        assert_eq!(
            world
                .get::<Visual>(barrel_entity)
                .map(|visual| visual.0.as_str()),
            Some("A burning barrel.")
        );
        assert_eq!(
            world
                .get::<effects::Settling>(barrel_entity)
                .map(|settling| settling.0.minutes),
            Some(5)
        );
        assert!(engine(&app, bridge).is_none());

        let records = records(&app);
        let spawned = records[0].change.spawned.clone();
        assert_eq!(spawned.len(), 1);
        let smoke = engine(&app, spawned[0]).expect("the spawned entity is indexed");
        assert_eq!(
            app.world()
                .get::<EntityName>(smoke)
                .map(|name| name.0.as_str()),
            Some("Smoke")
        );
        assert!(app.world().get::<Networked>(smoke).is_some());

        let bubble = turn_bubble::bubble_of(app.world_mut(), character).expect("a bubble");
        assert_eq!(bubble.order, vec![character, goblin]);
        assert!(nothing_locked(&app));
    }

    #[test]
    fn a_provider_failure_rejects_the_prompt_and_asks_to_rephrase() {
        let mut app = test_app(&[10]);
        let (connection, _) = connect(&mut app);
        prompt(&mut app, connection, "I juggle.");
        run_until(&mut app, |world| finished_count(world, connection) == 1);

        let sent = app.world().resource::<Sent>();
        assert_eq!(
            sent.states_for(connection),
            vec![PromptState::Resolving, PromptState::Rejected]
        );
        assert!(sent.lines.iter().any(|line| {
            line.recipients == Recipients::Only(connection)
                && line.message.kind == LogEntryKind::Notice
        }));
        assert_eq!(records(&app), Vec::new());
        assert!(nothing_locked(&app));
    }

    #[test]
    fn overlapping_prompts_wait_in_submission_order() {
        let mut app = test_app(&[10]);
        let (first_connection, first) = connect(&mut app);
        let (second_connection, second) = connect(&mut app);
        let narrate =
            r#"{"ok": true, "chk": null, "fx": [{"op": "narrate", "text": "Done."}], "fail": []}"#;
        let first_exchange = exchange(&mut app, first, "I whistle.", narrate);
        let second_exchange = exchange(&mut app, second, "I clap.", narrate);
        let provider = replay(&mut app, vec![first_exchange, second_exchange]);

        prompt(&mut app, first_connection, "I whistle.");
        prompt(&mut app, second_connection, "I clap.");
        run_until(&mut app, |world| {
            finished_count(world, first_connection) == 1
                && finished_count(world, second_connection) == 1
        });

        assert!(provider.is_finished());
        let sent = app.world().resource::<Sent>();
        assert_eq!(
            sent.states_for(second_connection),
            vec![
                PromptState::Queued,
                PromptState::Resolving,
                PromptState::Applied
            ]
        );
        assert_eq!(records(&app).len(), 2);
        assert!(nothing_locked(&app));
    }

    #[test]
    fn a_referenced_character_who_moved_is_resolved_once_more() {
        let mut app = test_app(&[10]);
        let (first_connection, first) = connect(&mut app);
        let (second_connection, second) = connect(&mut app);
        let snapshot = snapshot_of(&mut app, first);
        let second_local = local_id(&snapshot, second);
        let speak = format!(
            r#"{{"ok": true, "chk": null, "fail": [],
            "fx": [{{"op": "speak", "id": {second_local}, "text": "Hello.", "vol": "normal"}}]}}"#
        );
        let narrate = r#"{"ok": true, "chk": null, "fx": [{"op": "narrate", "text": "Too late."}], "fail": []}"#;
        let stale = exchange(&mut app, first, "I greet my friend.", &speak);

        let second_entity = engine(&app, second).expect("indexed");
        let start = position(&mut app, second);
        let target = GridPos::new(start.x, start.y.saturating_sub(2));
        let set_position = |app: &mut App, square: GridPos| {
            *app.world_mut()
                .get_mut::<GridPos>(second_entity)
                .expect("position") = square;
        };
        set_position(&mut app, target);
        let fresh = exchange(&mut app, first, "I greet my friend.", narrate);
        set_position(&mut app, start);
        let provider = replay(&mut app, vec![stale, fresh]);

        prompt(&mut app, first_connection, "I greet my friend.");
        app.update();
        send(&mut app, second_connection, MoveRequest { target });
        run_until(&mut app, |world| {
            finished_count(world, first_connection) == 1
        });

        assert!(provider.is_finished());
        assert_eq!(position(&mut app, second), target);
        let sent = app.world().resource::<Sent>();
        assert!(sent.has_line(LogEntryKind::Narration, "Too late."));
        assert!(!sent.has_line(LogEntryKind::Speech, "Hello."));
        let records = records(&app);
        assert_eq!(records.len(), 1);
        let zeta_core::event_log::EventCause::FreeformAction(cause) = &records[0].cause else {
            panic!(
                "expected a Freeform Action record, got {:?}",
                records[0].cause
            );
        };
        assert_eq!(cause.exchanges.len(), 2);
        assert!(nothing_locked(&app));
    }

    #[test]
    fn turn_bubbles_limit_turns_and_give_npc_turns_to_the_ai_dm() {
        let mut app = test_app(&[10]);
        let (connection, character) = connect(&mut app);
        let goblin = named(&mut app, "Goblin");
        turn_bubble::start_or_extend(app.world_mut(), &[character, goblin]);

        send(
            &mut app,
            connection,
            MoveRequest {
                target: GridPos::new(5, 10),
            },
        );
        app.update();
        assert_eq!(position(&mut app, character), GridPos::new(5, 10));
        send(
            &mut app,
            connection,
            MoveRequest {
                target: GridPos::new(6, 10),
            },
        );
        app.update();
        assert_eq!(position(&mut app, character), GridPos::new(5, 10));
        assert_eq!(
            app.world()
                .resource::<Sent>()
                .lines
                .last()
                .map(|line| line.message.text.as_str()),
            Some("You already used that this turn.")
        );

        let npc_turn = exchange(
            &mut app,
            goblin,
            prompts::NPC_TURN_TEXT,
            r#"{"ok": true, "chk": null, "fx": [{"op": "narrate", "text": "The goblin snarls."}], "fail": []}"#,
        );
        let provider = replay(&mut app, vec![npc_turn]);
        send(&mut app, connection, EndTurnRequest);
        run_until(&mut app, |world| {
            world
                .resource::<Sent>()
                .has_line(LogEntryKind::Narration, "The goblin snarls.")
        });
        app.update();

        assert!(provider.is_finished());
        let bubble: TurnBubble =
            turn_bubble::bubble_of(app.world_mut(), character).expect("a bubble");
        assert_eq!(bubble.current, 0);
        assert_eq!(bubble.completed_rounds, 1);
        assert!(!bubble.movement_used);

        send(
            &mut app,
            connection,
            MoveRequest {
                target: GridPos::new(6, 10),
            },
        );
        app.update();
        assert_eq!(position(&mut app, character), GridPos::new(6, 10));
    }

    #[test]
    fn empty_overlong_and_out_of_turn_prompts_are_rejected() {
        let mut app = test_app(&[10]);
        let (connection, character) = connect(&mut app);
        let goblin = named(&mut app, "Goblin");
        prompt(&mut app, connection, "   ");
        app.update();
        prompt(
            &mut app,
            connection,
            &"a".repeat(crate::settings::MAX_PROMPT_CHARACTERS + 1),
        );
        app.update();
        turn_bubble::start_or_extend(app.world_mut(), &[goblin, character]);
        prompt(&mut app, connection, "I attack.");
        app.update();

        let sent = app.world().resource::<Sent>();
        assert_eq!(sent.states_for(connection), vec![PromptState::Rejected; 3]);
        assert!(
            sent.statuses
                .iter()
                .all(|status| status.message.reason.is_some())
        );
        assert!(
            !sent
                .lines
                .iter()
                .any(|line| line.message.kind == LogEntryKind::Action)
        );
    }

    #[test]
    fn a_departing_player_loses_their_character() {
        let mut app = test_app(&[10]);
        let (connection, character) = connect(&mut app);
        let goblin = named(&mut app, "Goblin");
        turn_bubble::start_or_extend(app.world_mut(), &[character, goblin]);
        app.world_mut()
            .write_message(PlayerDisconnected { connection });
        app.update();

        assert!(engine(&app, character).is_none());
        assert!(
            !app.world()
                .resource::<PlayerCharacters>()
                .by_connection
                .contains_key(&connection)
        );
        let mut bubbles = app.world_mut().query::<&TurnBubble>();
        assert_eq!(bubbles.iter(app.world()).count(), 0);
    }

    fn inventory_of(app: &App, id: EntityId) -> Inventory {
        let entity = engine(app, id).expect("indexed");
        app.world()
            .get::<Inventory>(entity)
            .cloned()
            .expect("the entity carries items")
    }

    fn catalog_id(text: &str) -> CatalogId {
        CatalogId::new(text).expect("valid id")
    }

    /// The stack of `id` holding catalog item `item`.
    fn stack_of(app: &App, id: EntityId, item: &str) -> StackId {
        inventory_of(app, id)
            .stacks()
            .iter()
            .find(|stack| stack.item().catalog_id() == Some(&catalog_id(item)))
            .map(ItemStack::id)
            .expect("the item is carried")
    }

    fn held(app: &App, id: EntityId, item: &str) -> u32 {
        inventory_of(app, id)
            .stacks()
            .iter()
            .filter(|stack| stack.item().catalog_id() == Some(&catalog_id(item)))
            .map(ItemStack::quantity)
            .sum()
    }

    fn verb(app: &mut App, connection: PlayerConnection, request: ItemVerbRequest) {
        send(app, connection, request);
        app.update();
    }

    fn request(verb: ItemVerb, stack: StackId) -> ItemVerbRequest {
        ItemVerbRequest {
            verb,
            stack,
            quantity: 1,
            recipient: None,
            craft_target: None,
        }
    }

    fn item_changes(app: &App) -> Vec<ItemChange> {
        records(app)
            .into_iter()
            .flat_map(|record| record.change.items)
            .collect()
    }

    #[test]
    fn characters_start_with_items_and_the_goblin_carries_a_scimitar() {
        let mut app = test_app(&[10]);
        let (_, character) = connect(&mut app);
        let goblin = named(&mut app, "Goblin");
        assert_eq!(held(&app, character, "painters_supplies"), 1);
        assert_eq!(held(&app, character, "rations"), 2);
        assert_eq!(held(&app, goblin, "scimitar"), 1);
        let entity = engine(&app, character).expect("indexed");
        assert!(app.world().get::<Networked>(entity).is_some());
    }

    #[test]
    fn item_verbs_change_the_inventory_and_are_recorded() {
        let mut app = test_app(&[10]);
        let (connection, character) = connect(&mut app);
        let start = position(&mut app, character);

        let sword = stack_of(&app, character, "longsword");
        verb(&mut app, connection, request(ItemVerb::Equip, sword));
        let equipped = inventory_of(&app, character);
        assert!(equipped.stack(sword).expect("held").equipped());

        let rations = stack_of(&app, character, "rations");
        verb(&mut app, connection, request(ItemVerb::Consume, rations));
        assert_eq!(held(&app, character, "rations"), 1);

        let rope = stack_of(&app, character, "rope");
        verb(&mut app, connection, request(ItemVerb::Drop, rope));
        assert_eq!(held(&app, character, "rope"), 0);

        let tool = stack_of(&app, character, "painters_supplies");
        verb(
            &mut app,
            connection,
            ItemVerbRequest {
                craft_target: Some(catalog_id("wooden_staff")),
                ..request(ItemVerb::Craft, tool)
            },
        );
        assert_eq!(held(&app, character, "wooden_staff"), 1);
        assert_eq!(held(&app, character, "painters_supplies"), 1);

        let records = records(&app);
        assert_eq!(records.len(), 4);
        assert!(records.iter().all(|record| matches!(
            &record.cause,
            zeta_core::event_log::EventCause::ItemVerb(cause) if cause.actor == character
        )));
        let dropped = records[2].change.spawned.clone();
        assert_eq!(dropped.len(), 1);
        assert_eq!(position(&mut app, dropped[0]), start);
        assert_eq!(held(&app, dropped[0], "rope"), 1);
        let changes = item_changes(&app);
        assert_eq!(changes.len(), 5, "{changes:?}");
        assert_eq!(changes[0].kind, ItemChangeKind::Equipped);
        assert!(
            app.world()
                .resource::<Sent>()
                .has_line(LogEntryKind::Action, "Equips the Longsword.")
        );
    }

    #[test]
    fn refused_verbs_change_nothing_and_say_why() {
        let mut app = test_app(&[10]);
        let (connection, character) = connect(&mut app);
        let goblin = named(&mut app, "Goblin");
        let sword = stack_of(&app, character, "longsword");
        let tool = stack_of(&app, character, "painters_supplies");
        verb(
            &mut app,
            connection,
            ItemVerbRequest {
                recipient: Some(goblin),
                ..request(ItemVerb::Give, sword)
            },
        );
        verb(&mut app, connection, request(ItemVerb::Consume, sword));
        verb(
            &mut app,
            connection,
            ItemVerbRequest {
                craft_target: Some(catalog_id("longsword")),
                ..request(ItemVerb::Craft, tool)
            },
        );
        verb(&mut app, connection, request(ItemVerb::Equip, StackId(999)));

        assert_eq!(held(&app, character, "longsword"), 1);
        assert_eq!(held(&app, goblin, "longsword"), 0);
        assert_eq!(records(&app).len(), 0);
        let notices = app
            .world()
            .resource::<Sent>()
            .lines
            .iter()
            .filter(|line| {
                line.message.kind == LogEntryKind::Notice
                    && line.recipients == Recipients::Only(connection)
            })
            .count();
        assert_eq!(notices, 4);
        assert!(
            app.world()
                .resource::<Sent>()
                .has_line(LogEntryKind::Notice, "Move next to Goblin first.")
        );
    }

    #[test]
    fn give_moves_items_to_an_adjacent_holder() {
        let mut app = test_app(&[10]);
        let (connection, character) = connect(&mut app);
        let goblin = named(&mut app, "Goblin");
        let here = position(&mut app, character);
        let goblin_entity = engine(&app, goblin).expect("indexed");
        *app.world_mut()
            .get_mut::<GridPos>(goblin_entity)
            .expect("position") = GridPos::new(here.x + 1, here.y);
        let rations = stack_of(&app, character, "rations");
        verb(
            &mut app,
            connection,
            ItemVerbRequest {
                quantity: 2,
                recipient: Some(goblin),
                ..request(ItemVerb::Give, rations)
            },
        );
        assert_eq!(held(&app, character, "rations"), 0);
        assert_eq!(held(&app, goblin, "rations"), 2);
        let changes = item_changes(&app);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[1].holder, goblin);
        assert_eq!(changes[1].kind, ItemChangeKind::Gained);
    }

    #[test]
    fn award_and_take_effects_move_items() {
        let mut app = test_app(&[10]);
        let (connection, character) = connect(&mut app);
        let goblin = named(&mut app, "Goblin");
        let snapshot = snapshot_of(&mut app, character);
        let goblin_local = local_id(&snapshot, goblin);
        let response = format!(
            r#"{{"ok": true, "chk": null, "fail": [], "fx": [
            {{"op": "take", "id": {goblin_local}, "item": "scimitar", "custom_name": null,
              "qty": 1}},
            {{"op": "award", "id": 0, "item": "scimitar", "custom": null, "qty": 1}},
            {{"op": "award", "id": 0, "item": null, "qty": 1, "custom": {{
              "name": "Goblin Map", "weight_thousandths_of_pound": 10,
              "description": "A crude map scratched on hide.", "tags": ["map"]}}}},
            {{"op": "take", "id": 0, "item": "rations", "custom_name": null, "qty": 1}}
            ]}}"#
        );
        let first = exchange(&mut app, character, "I take the goblin's blade.", &response);
        replay(&mut app, vec![first]);
        prompt(&mut app, connection, "I take the goblin's blade.");
        run_until(&mut app, |world| finished_count(world, connection) == 1);

        assert_eq!(held(&app, goblin, "scimitar"), 0);
        assert_eq!(held(&app, character, "scimitar"), 1);
        assert_eq!(held(&app, character, "rations"), 1);
        assert!(
            inventory_of(&app, character)
                .stacks()
                .iter()
                .any(|stack| matches!(
                    stack.item(),
                    Item::Custom(custom) if custom.name == "Goblin Map"
                ))
        );
        let changes = item_changes(&app);
        assert_eq!(changes.len(), 4, "{changes:?}");
        assert_eq!(changes[0].holder, goblin);
        assert_eq!(changes[0].kind, ItemChangeKind::Lost);
    }

    #[test]
    fn an_attached_item_reaches_the_ai_request() {
        let mut app = test_app(&[10]);
        let (_, character) = connect(&mut app);
        let rope = stack_of(&app, character, "rope");
        let sword = stack_of(&app, character, "longsword");
        let context = prompts::item_context(app.world(), character, Some(rope));
        let snapshot = snapshot_of(&mut app, character);
        let request = prompts::resolution_request(&snapshot, "I tie a knot.", false, &context);
        let [zeta_core::ai::AiMessage::User { text }] = request.messages.as_slice() else {
            panic!("expected 1 user message");
        };
        let body: serde_json::Value = serde_json::from_str(text).expect("JSON");
        assert_eq!(body["utilize"]["item"], "Rope");
        assert_eq!(body["utilize"]["checks"][0]["dc"], 10);
        assert_eq!(body["character"]["items"][0]["name"], "Longsword");
        let no_checks = prompts::item_context(app.world(), character, Some(sword));
        assert_eq!(no_checks.utilize, None);
        assert_eq!(
            prompts::utilize_refusal(app.world(), character, Some(rope)),
            None
        );
        assert_eq!(
            prompts::utilize_refusal(app.world(), character, Some(sword)),
            Some("That item has no use to attach to an action.")
        );
        assert_eq!(
            prompts::utilize_refusal(app.world(), character, Some(StackId(999))),
            Some("You no longer carry that item.")
        );
    }

    /// Moves `mover` to the square east of `target`.
    fn place_next_to(app: &mut App, mover: EntityId, target: EntityId) {
        let here = position(app, target);
        let entity = engine(app, mover).expect("indexed");
        *app.world_mut()
            .get_mut::<GridPos>(entity)
            .expect("position") = GridPos::new(here.x + 1, here.y);
    }

    fn has_notice(app: &App, text: &str) -> bool {
        app.world()
            .resource::<Sent>()
            .has_line(LogEntryKind::Notice, text)
    }

    #[test]
    fn locked_actors_and_recipients_refuse_item_verbs() {
        let mut app = test_app(&[10]);
        let (connection, character) = connect(&mut app);
        let goblin = named(&mut app, "Goblin");
        place_next_to(&mut app, goblin, character);
        let rations = stack_of(&app, character, "rations");
        let give = ItemVerbRequest {
            recipient: Some(goblin),
            ..request(ItemVerb::Give, rations)
        };

        // Only the goblin is locked: a Snapshot holding just the goblin.
        let snapshot = snapshot_of(&mut app, character);
        let goblin_record = snapshot
            .entities()
            .iter()
            .find(|entity| entity.world_id == goblin)
            .expect("the goblin is perceived")
            .clone();
        let goblin_only = zeta_core::snapshot::Snapshot::build(
            goblin_record,
            Vec::new(),
            |_| false,
            zeta_core::snapshot::PerceptionSettings::default(),
        );
        let (goblin_ticket, _) = app
            .world_mut()
            .resource_mut::<ResolutionLocksResource>()
            .0
            .submit(&goblin_only);
        verb(&mut app, connection, give.clone());
        assert!(has_notice(
            &app,
            "Goblin is part of an action that is still resolving."
        ));
        app.world_mut()
            .resource_mut::<ResolutionLocksResource>()
            .0
            .release(goblin_ticket)
            .expect("active");

        // The actor is locked.
        app.world_mut()
            .resource_mut::<ResolutionLocksResource>()
            .0
            .submit(&snapshot);
        verb(&mut app, connection, give);
        assert!(has_notice(
            &app,
            "Your items cannot change while an action involving your character resolves."
        ));

        assert_eq!(held(&app, character, "rations"), 2);
        assert_eq!(held(&app, goblin, "rations"), 0);
        assert_eq!(records(&app).len(), 0);
    }

    #[test]
    fn utilize_as_an_item_verb_points_to_the_action_prompt() {
        let mut app = test_app(&[10]);
        let (connection, character) = connect(&mut app);
        let rope = stack_of(&app, character, "rope");
        verb(&mut app, connection, request(ItemVerb::Utilize, rope));
        assert!(has_notice(
            &app,
            "Describe how you use the Rope in the action prompt."
        ));
        assert_eq!(held(&app, character, "rope"), 1);
        assert_eq!(records(&app).len(), 0);
    }

    #[test]
    fn quantities_outside_the_stack_change_nothing() {
        let mut app = test_app(&[10]);
        let (connection, character) = connect(&mut app);
        let goblin = named(&mut app, "Goblin");
        place_next_to(&mut app, goblin, character);
        let rope = stack_of(&app, character, "rope");
        let rations = stack_of(&app, character, "rations");
        for quantity in [0, 5] {
            verb(
                &mut app,
                connection,
                ItemVerbRequest {
                    quantity,
                    ..request(ItemVerb::Drop, rope)
                },
            );
            verb(
                &mut app,
                connection,
                ItemVerbRequest {
                    quantity,
                    recipient: Some(goblin),
                    ..request(ItemVerb::Give, rations)
                },
            );
        }
        assert!(has_notice(&app, "Choose at least 1."));
        assert!(has_notice(&app, "You carry only 1."));
        assert!(has_notice(&app, "You carry only 2."));
        assert_eq!(held(&app, character, "rope"), 1);
        assert_eq!(held(&app, character, "rations"), 2);
        assert_eq!(held(&app, goblin, "rations"), 0);
        assert_eq!(records(&app).len(), 0);
    }

    #[test]
    fn a_full_recipient_stack_keeps_the_items_with_the_giver() {
        let mut app = test_app(&[10]);
        let (connection, character) = connect(&mut app);
        let goblin = named(&mut app, "Goblin");
        place_next_to(&mut app, goblin, character);
        let goblin_entity = engine(&app, goblin).expect("indexed");
        app.world_mut()
            .get_mut::<Inventory>(goblin_entity)
            .expect("the goblin carries items")
            .add(Item::Catalog(catalog_id("rations")), u32::MAX)
            .expect("adds");
        let rations = stack_of(&app, character, "rations");
        verb(
            &mut app,
            connection,
            ItemVerbRequest {
                recipient: Some(goblin),
                ..request(ItemVerb::Give, rations)
            },
        );
        assert!(has_notice(&app, "That stack cannot hold any more."));
        assert_eq!(held(&app, character, "rations"), 2);
        assert_eq!(held(&app, goblin, "rations"), u32::MAX);
        assert_eq!(records(&app).len(), 0);
    }

    #[test]
    fn give_needs_a_creature_that_carries_items() {
        let mut app = test_app(&[10]);
        let (connection, character) = connect(&mut app);
        let barrel = named(&mut app, "Oil Barrel");
        place_next_to(&mut app, barrel, character);
        let sword = stack_of(&app, character, "longsword");
        verb(
            &mut app,
            connection,
            ItemVerbRequest {
                recipient: Some(barrel),
                ..request(ItemVerb::Give, sword)
            },
        );
        assert!(has_notice(&app, "Oil Barrel cannot carry items."));

        // A dropped pile holds items but is no creature.
        let rope = stack_of(&app, character, "rope");
        verb(&mut app, connection, request(ItemVerb::Drop, rope));
        let pile = records(&app)[0].change.spawned[0];
        verb(
            &mut app,
            connection,
            ItemVerbRequest {
                recipient: Some(pile),
                ..request(ItemVerb::Give, sword)
            },
        );
        assert!(has_notice(&app, "Rope cannot carry items."));
        assert_eq!(held(&app, character, "longsword"), 1);
        assert_eq!(held(&app, pile, "longsword"), 0);
        assert_eq!(records(&app).len(), 1);
    }
}
