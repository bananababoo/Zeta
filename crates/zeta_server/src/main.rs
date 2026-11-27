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
use zeta_server::config::{
    ALLOW_IN_MEMORY_LOG_VARIABLE, AiConfig, DatabaseConfig, allow_in_memory_log_from_environment,
    listen_address_from_environment,
};
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
    let allow_in_memory = match allow_in_memory_log_from_environment() {
        Ok(allow) => allow,
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
    let event_log = match event_log_writer(&runtime, allow_in_memory) {
        Ok(writer) => writer,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
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

/// Opens the PostgreSQL Event Log store, failing fast on startup if PostgreSQL is
/// misconfigured or unreachable unless `allow_in_memory` (`ZETA_ALLOW_IN_MEMORY_LOG=1`) is set.
fn event_log_writer(runtime: &Runtime, allow_in_memory: bool) -> Result<EventLogWriter, String> {
    let config = match DatabaseConfig::from_environment() {
        Ok(config) => config,
        Err(error) => {
            if allow_in_memory {
                let notice = format!(
                    "Persistence is running in memory-only mode ({ALLOW_IN_MEMORY_LOG_VARIABLE}=1; \
                     {error}). World changes will not be saved."
                );
                eprintln!("warning: {notice}");
                return Ok(EventLogWriter::memory_with_notice(notice));
            }
            return Err(format!(
                "startup aborted: database configuration failed ({error}). \
                 Fill in PostgreSQL settings in .env and start Docker/PostgreSQL with \
                 `powershell -ExecutionPolicy Bypass -File tools/dev_up.ps1`, or set \
                 {ALLOW_IN_MEMORY_LOG_VARIABLE}=1 to intentionally run without persistence."
            ));
        }
    };
    match runtime.block_on(EventLogStore::connect_and_migrate(&config)) {
        Ok(store) => Ok(EventLogWriter::database(
            store,
            runtime.handle(),
            MapPartitionId(TEST_SCENE_MAP_PARTITION_ID),
        )),
        Err(error) => {
            if allow_in_memory {
                let notice = format!(
                    "Persistence is running in memory-only mode ({ALLOW_IN_MEMORY_LOG_VARIABLE}=1; \
                     could not reach PostgreSQL at {}:{}: {error}). World changes will not be saved.",
                    config.host, config.port
                );
                eprintln!("warning: {notice}");
                return Ok(EventLogWriter::memory_with_notice(notice));
            }
            Err(format!(
                "startup aborted: could not connect to PostgreSQL at {}:{} ({error}). \
                 Is Docker Desktop / PostgreSQL running? Start it with \
                 `powershell -ExecutionPolicy Bypass -File tools/dev_up.ps1`, or set \
                 {ALLOW_IN_MEMORY_LOG_VARIABLE}=1 to intentionally run without persistence.",
                config.host, config.port
            ))
        }
    }
}
