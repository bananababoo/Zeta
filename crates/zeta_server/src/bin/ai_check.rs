//! Live check of the configured AI provider: sends 1 small request and prints the reply.
//!
//! Run with `cargo run -p zeta_server --bin ai_check` after filling in `.env`. It calls the
//! real provider, so it is never part of `cargo test`.

use std::process::ExitCode;

use zeta_core::ai::{AiMessage, AiProvider, AiRequest};
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
    let request = AiRequest {
        system_prompt: "Reply with a JSON object of the form {\"ok\": true, \"word\": <any word>}."
            .to_owned(),
        messages: vec![AiMessage::User {
            text: "Connection check.".to_owned(),
        }],
    };
    match runtime.block_on(provider.complete(&request)) {
        Ok(response) => {
            println!("{model} replied: {}", response.text);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
