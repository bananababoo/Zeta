//! Zeta game client binary. Stays thin; the plugins live in the library.
//!
//! Rules: `docs/architecture/technical_architecture.md` Sections 1 and 5.

use std::net::{AddrParseError, Ipv4Addr, SocketAddr};

use bevy::prelude::*;
use zeta_client::ClientPlugins;
use zeta_client::net::ClientNetPlugin;
use zeta_core::settings::DEFAULT_SERVER_PORT;

/// Environment variable naming the server to connect to, as `host:port`.
const SERVER_ADDRESS_VARIABLE: &str = "ZETA_SERVER_ADDRESS";

/// The server address from the environment value, or the local server on the default port.
fn server_address(value: Option<&str>) -> Result<SocketAddr, AddrParseError> {
    value.map_or_else(
        || Ok(SocketAddr::from((Ipv4Addr::LOCALHOST, DEFAULT_SERVER_PORT))),
        str::parse,
    )
}

fn main() -> AppExit {
    let variable = std::env::var(SERVER_ADDRESS_VARIABLE).ok();
    let server_address = match server_address(variable.as_deref()) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_server_is_local_on_the_default_port() {
        let address = server_address(None).expect("the default parses");
        assert_eq!(
            address,
            SocketAddr::from((Ipv4Addr::LOCALHOST, DEFAULT_SERVER_PORT))
        );
    }

    #[test]
    fn the_environment_value_overrides_the_default() {
        let address = server_address(Some("10.0.0.5:9000")).expect("a valid address");
        assert_eq!(address, SocketAddr::from(([10, 0, 0, 5], 9000)));
    }

    #[test]
    fn an_invalid_environment_value_is_an_error() {
        assert!(server_address(Some("not an address")).is_err());
    }
}
