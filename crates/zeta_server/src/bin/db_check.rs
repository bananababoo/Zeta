//! Live check of the configured PostgreSQL + `PostGIS` database: connects, applies
//! migrations, queries the `PostGIS` version, and prints confirmation.
//!
//! Run with `cargo run -p zeta_server --bin db_check` after starting PostgreSQL with
//! `tools/dev_up.ps1` and filling in `.env`. It requires a running database, so it is
//! never part of `cargo test`.

use std::process::ExitCode;

use zeta_server::config::DatabaseConfig;
use zeta_server::db::EventLogStore;

fn main() -> ExitCode {
    let config = match DatabaseConfig::from_environment() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("configuration error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async move {
        let store = match EventLogStore::connect_and_migrate(&config).await {
            Ok(store) => store,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        };
        match store.postgis_version().await {
            Ok(version) => {
                println!(
                    "connected to {} on {}:{}; PostGIS {version}",
                    config.database, config.host, config.port
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        }
    })
}
