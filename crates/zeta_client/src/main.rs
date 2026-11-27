//! Zeta game client binary. Stays thin; the plugins live in the library.
//!
//! Rules: `docs/architecture/technical_architecture.md` Sections 1 and 5.

use bevy::prelude::*;
use zeta_client::ClientPlugins;
use zeta_client::net::{ClientNetPlugin, parse_server_address};
use zeta_core::settings::DEFAULT_SERVER_PORT;

/// Environment variable naming the server to connect to, as `host:port`.
const SERVER_ADDRESS_VARIABLE: &str = "ZETA_SERVER_ADDRESS";

fn main() -> AppExit {
    let variable = std::env::var(SERVER_ADDRESS_VARIABLE).ok();
    let server_address = match parse_server_address(variable.as_deref()) {
        Ok(address) => address,
        Err(error) => {
            eprintln!(
                "{SERVER_ADDRESS_VARIABLE} must be an address such as 127.0.0.1:{DEFAULT_SERVER_PORT}: {error}"
            );
            return AppExit::error();
        }
    };
    App::new()
        .add_plugins(
            DefaultPlugins
                .set(zeta_client::terrain::client_asset_plugin())
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "Zeta".to_owned(),
                        ..default()
                    }),
                    ..default()
                }),
        )
        .add_plugins(ClientNetPlugin { server_address })
        .add_plugins(ClientPlugins)
        .run()
}
