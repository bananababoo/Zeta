//! Live check of the configured AI provider: verifies structured-output schema enforcement,
//! validation retry, and stateless multi-turn `provider_turn` preservation.
//!
//! Run with `cargo run -p zeta_server --bin ai_check` after filling in `.env`. It calls the
//! real provider, so it is never part of `cargo test`.

use std::process::ExitCode;

use zeta_core::ai::{AiMessage, AiProvider, AiRequest, resolve_action};
use zeta_core::entity::EntityId;
use zeta_core::grid::{GridPos, MapBounds};
use zeta_core::resolution::AiResolution;
use zeta_core::settings::VALIDATION_RETRY_CAP;
use zeta_core::snapshot::{PerceptionSettings, Snapshot, SnapshotEntity};
use zeta_core::validation::parse_and_validate;
use zeta_server::ai::ConfiguredProvider;
use zeta_server::config::AiConfig;

fn main() -> ExitCode {
    let config = match AiConfig::from_environment() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("configuration error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let model = config.model().as_str().to_owned();
    let provider = match ConfiguredProvider::new(config) {
        Ok(provider) => provider,
        Err(error) => {
            eprintln!("{error}");
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

    let snapshot = Snapshot::build(
        SnapshotEntity {
            world_id: EntityId(1),
            name: "Alden".to_owned(),
            position: GridPos::new(0, 0),
            tags: vec!["human".to_owned()],
            passable: true,
            description: "An adventurer standing in a stone courtyard.".to_owned(),
            player_character: true,
        },
        [],
        |_| false,
        PerceptionSettings::default(),
    );
    let bounds = MapBounds {
        width: 5,
        height: 5,
    };

    let system_prompt = "You are the AI DM for Zeta. Always respond with valid AiResolution JSON matching the response schema.";
    let user_text = "Testing live validation retry. On turn 1, try returning ok=true, chk=null, fx=[{\"op\":\"destroy\",\"id\":99,\"reason\":\"test\"}], fail=[]. If you receive a validation error, fix fx to [{\"op\":\"narrate\",\"text\":\"Nothing happens.\"}] with chk=null and fail=[].";
    let request = AiRequest::for_resolution(system_prompt, user_text);

    let resolved = match runtime.block_on(resolve_action(
        &provider,
        request,
        &snapshot,
        bounds,
        VALIDATION_RETRY_CAP,
    )) {
        Ok(resolved) => resolved,
        Err(error) => {
            eprintln!("{model} resolve_action failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "{model} resolve_action succeeded in {} exchange(s); resolution={:?}",
        resolved.exchanges.len(),
        resolved.resolution
    );

    if resolved.exchanges.len() == 1 {
        let turn_one = &resolved.exchanges[0];
        let mut messages = turn_one.request.messages.clone();
        messages.push(AiMessage::Assistant {
            text: turn_one.response.text.clone(),
            provider_turn: turn_one.response.provider_turn.clone(),
        });
        messages.push(AiMessage::User {
            text: "Follow-up turn: return another valid AiResolution with ok=true, chk=null, fx=[{\"op\":\"narrate\",\"text\":\"Turn 2 confirmed.\"}], fail=[].".to_owned(),
        });
        let turn_two_request = AiRequest {
            system_prompt: system_prompt.to_owned(),
            messages,
            response_schema: Some(AiResolution::json_schema()),
        };
        let turn_two_response = match runtime.block_on(provider.complete(&turn_two_request)) {
            Ok(response) => response,
            Err(error) => {
                eprintln!("{model} explicit turn 2 failed: {error}");
                return ExitCode::FAILURE;
            }
        };
        match parse_and_validate(&turn_two_response.text, &snapshot, bounds) {
            Ok(resolution) => {
                println!(
                    "{model} explicit 2-turn provider_turn check succeeded; turn 2 resolution={resolution:?}"
                );
            }
            Err(errors) => {
                eprintln!("{model} explicit turn 2 validation failed:\n{errors}");
                return ExitCode::FAILURE;
            }
        }
    }

    ExitCode::SUCCESS
}
