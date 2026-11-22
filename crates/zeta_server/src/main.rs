//! Headless authoritative Zeta server: the test scene, player characters, and the AI DM
//! loop, served to clients over the network.
//!
//! Architecture: `docs/architecture/technical_architecture.md` Sections 1, 2 and 5, and
//! `docs/prototypes/realism_prototype.md`. This binary only reads configuration and wires
//! the plugins of the `zeta_server` library together.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use bevy_app::{App, AppExit, ScheduleRunnerPlugin, TaskPoolPlugin};
use bevy_state::app::StatesPlugin;
use bevy_time::TimePlugin;
use tokio::runtime::{Builder, Runtime};
use zeta_core::event_log::MapPartitionId;
use zeta_server::ai::ConfiguredProvider;
use zeta_server::bridge::{AiBridge, AiBridgePlugin};
use zeta_server::config::{AiConfig, DatabaseConfig, listen_address_from_environment};
use zeta_server::db::EventLogStore;
use zeta_server::net::ServerNetPlugin;
use zeta_server::settings::{SERVER_TICKS_PER_SECOND, TEST_SCENE_MAP_PARTITION_ID};
use zeta_server::world::{EventLogWriter, WorldPlugin};

fn main() -> ExitCode {
    let listen_address = match listen_address_from_environment() {
        Ok(address) => address,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let provider = match AiConfig::from_environment().map(ConfiguredProvider::new) {
        Ok(Ok(provider)) => Arc::new(provider),
        Ok(Err(error)) => {
            eprintln!("could not set up the AI provider: {error}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let event_log = event_log_writer(&runtime);
    let bridge = AiBridge::new(runtime.handle().clone(), provider);

    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        TimePlugin,
        StatesPlugin,
        ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(
            1.0 / f64::from(SERVER_TICKS_PER_SECOND),
        )),
    ))
    .add_plugins(ServerNetPlugin { listen_address })
    .add_plugins(AiBridgePlugin::<ConfiguredProvider>::new())
    .add_plugins(WorldPlugin::<ConfiguredProvider>::new())
    .insert_resource(bridge)
    .insert_resource(event_log);

    eprintln!("Zeta server listening on {listen_address}");
    match app.run() {
        AppExit::Success => ExitCode::SUCCESS,
        AppExit::Error(_) => ExitCode::FAILURE,
    }
}

/// The Event Log writer: PostgreSQL when the database is configured and reachable, else an
/// in-memory log that is lost on exit.
fn event_log_writer(runtime: &Runtime) -> EventLogWriter {
    let config = match DatabaseConfig::from_environment() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("warning: {error}; the Event Log is kept in memory only and is lost on exit");
            return EventLogWriter::memory();
        }
    };
    match runtime.block_on(EventLogStore::connect_and_migrate(&config)) {
        Ok(store) => EventLogWriter::database(
            store,
            runtime.handle(),
            MapPartitionId(TEST_SCENE_MAP_PARTITION_ID),
        ),
        Err(error) => {
            eprintln!(
                "warning: could not open the Event Log database ({error}); the Event Log is \
                 kept in memory only and is lost on exit"
            );
            EventLogWriter::memory()
        }
    }
}
