//! The Gemini API provider (Google AI Studio), used by the prototype on the free tier.
//!
//! Rules: `docs/architecture/ai_orchestration.md` Section 6. Uses the Interactions API in
//! stateless mode: the server keeps the conversation, sends it whole each time, and asks
//! Google not to store it. Mapping between Zeta requests and the wire format is kept in pure
//! functions, so it is tested without a network.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeta_core::ai::{AiMessage, AiProvider, AiRequest, AiResponse, ProviderError};

use crate::config::{ApiKey, GeminiConfig};
use crate::settings::AI_REQUEST_TIMEOUT_SECONDS;

/// Interactions API endpoint. The model is named in the body, and the key is sent in a
/// header, so neither appears in the address or in errors that include it.
const INTERACTIONS_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/interactions";

/// Header that carries the API key.
const API_KEY_HEADER: &str = "x-goog-api-key";

/// Pins the wire format this module was written against, so a later Google revision cannot
/// change response shapes without a deliberate update here.
const API_REVISION_HEADER: &str = "Api-Revision";
const API_REVISION: &str = "2026-05-20";

/// Why a [`GeminiProvider`] could not be created.
#[derive(Debug, Error)]
pub enum GeminiSetupError {
    /// The HTTP client could not be built, usually because TLS failed to initialize.
    #[error("could not build the HTTP client: {0}")]
    Client(#[from] reqwest::Error),
}

/// Sends AI DM requests to 1 Gemini model.
#[derive(Debug)]
pub struct GeminiProvider {
    client: reqwest::Client,
    config: GeminiConfig,
}

impl GeminiProvider {
    /// Creates a provider for the configured model.
    ///
    /// # Errors
    /// [`GeminiSetupError::Client`] if the HTTP client cannot be built.
    pub fn new(config: GeminiConfig) -> Result<Self, GeminiSetupError> {
        // reqwest is built without a crypto provider (`rustls-no-provider`), so `ring` is
        // installed as the process default before the first client is built (reqwest
        // async_impl/client.rs:721, rustls crypto/mod.rs:227). An error only means a provider
        // is already installed, which is the goal.
        let _already_installed = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(AI_REQUEST_TIMEOUT_SECONDS))
            .build()?;
        Ok(Self { client, config })
    }

    fn api_key(&self) -> &ApiKey {
        &self.config.api_key
    }
}

impl AiProvider for GeminiProvider {
    async fn complete(&self, request: &AiRequest) -> Result<AiResponse, ProviderError> {
        let response = self
            .client
            .post(INTERACTIONS_ENDPOINT)
            .header(API_KEY_HEADER, self.api_key().expose())
            .header(API_REVISION_HEADER, API_REVISION)
            .json(&request_body(self.config.model.as_str(), request))
            .send()
            .await
            .map_err(transport_failure)?;
        let status = response.status();
        let body = response.text().await.map_err(transport_failure)?;
        if status.is_success() {
            parse_success(&body)
        } else {
            Err(parse_failure(status.as_u16(), &body))
        }
    }
}

// Wire format: https://aistudio.google.com/api-reference/interactions-api.md. Only the
// fields Zeta uses are modeled. Responses do not deny unknown fields or step types, because
// Google adds them over time.

#[derive(Debug, PartialEq, Serialize)]
struct InteractionRequest<'a> {
    model: &'a str,
    system_instruction: &'a str,
    input: Vec<InputStep<'a>>,
    response_format: ResponseFormat<'a>,
    /// The server keeps the conversation itself, so Google does not need to store it.
    store: bool,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(untagged)]
enum InputStep<'a> {
    Structured(Step<'a>),
    Raw(&'a serde_json::Value),
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Step<'a> {
    UserInput { content: Vec<TextContent<'a>> },
    ModelOutput { content: Vec<TextContent<'a>> },
}

#[derive(Debug, PartialEq, Serialize)]
struct TextContent<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
}

#[derive(Debug, PartialEq, Serialize)]
struct ResponseFormat<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    mime_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema: Option<&'a serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct Interaction {
    status: String,
    #[serde(default)]
    steps: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseStep {
    ModelOutput {
        #[serde(default)]
        content: Vec<ResponseContent>,
    },
    /// Thoughts, tool calls, echoed input, and any step type added later.
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseContent {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    message: String,
}

/// Maps a Zeta request to the Interactions request body.
///
/// Every AI DM request expects a JSON resolution, so JSON output is always requested, with an
/// optional JSON Schema attached when `request.response_schema` is set.
fn request_body<'a>(model: &'a str, request: &'a AiRequest) -> InteractionRequest<'a> {
    InteractionRequest {
        model,
        system_instruction: &request.system_prompt,
        input: request.messages.iter().flat_map(message_steps).collect(),
        response_format: ResponseFormat {
            kind: "text",
            mime_type: "application/json",
            schema: request.response_schema.as_ref(),
        },
        store: false,
    }
}

fn message_steps(message: &AiMessage) -> Vec<InputStep<'_>> {
    match message {
        AiMessage::User { text } => vec![InputStep::Structured(Step::UserInput {
            content: vec![text_content(text)],
        })],
        AiMessage::Assistant {
            text,
            provider_turn,
        } => match provider_turn {
            Some(serde_json::Value::Array(steps)) if !steps.is_empty() => {
                steps.iter().map(InputStep::Raw).collect()
            }
            _ => vec![InputStep::Structured(Step::ModelOutput {
                content: vec![text_content(text)],
            })],
        },
    }
}

fn text_content(text: &str) -> TextContent<'_> {
    TextContent { kind: "text", text }
}

/// Extracts the answer from a successful response body: the text of the trailing run of
/// model output steps, which is the model's final answer, plus all raw model-generated steps
/// in `provider_turn`.
fn parse_success(body: &str) -> Result<AiResponse, ProviderError> {
    let interaction: Interaction =
        serde_json::from_str(body).map_err(|error| ProviderError::InvalidResponse {
            detail: format!("unreadable Gemini JSON: {error}"),
        })?;
    if interaction.status != "completed" {
        return Err(ProviderError::InvalidResponse {
            detail: format!(
                "Gemini interaction ended with status {}",
                interaction.status
            ),
        });
    }
    let parsed_steps: Vec<ResponseStep> = interaction
        .steps
        .iter()
        .map(ResponseStep::deserialize)
        .collect::<Result<_, _>>()
        .map_err(|error| ProviderError::InvalidResponse {
            detail: format!("unreadable Gemini response step: {error}"),
        })?;
    let trailing_outputs = parsed_steps
        .iter()
        .rev()
        .take_while(|step| matches!(step, ResponseStep::ModelOutput { .. }))
        .count();
    let text: String = parsed_steps[parsed_steps.len() - trailing_outputs..]
        .iter()
        .flat_map(|step| match step {
            ResponseStep::ModelOutput { content } => content.as_slice(),
            ResponseStep::Other => &[],
        })
        .filter_map(|content| match content {
            ResponseContent::Text { text } => Some(text.as_str()),
            ResponseContent::Other => None,
        })
        .collect();
    if text.is_empty() {
        return Err(ProviderError::InvalidResponse {
            detail: "Gemini returned no text".to_owned(),
        });
    }
    let model_steps: Vec<serde_json::Value> = interaction
        .steps
        .into_iter()
        .filter(|step| step.get("type").and_then(serde_json::Value::as_str) != Some("user_input"))
        .collect();
    let provider_turn = (!model_steps.is_empty()).then_some(serde_json::Value::Array(model_steps));
    Ok(AiResponse {
        text,
        provider_turn,
    })
}

/// Maximum non-JSON error body characters preserved in diagnostics.
const MAX_ERROR_SNIPPET_CHARS: usize = 160;

/// Classifies a non-success HTTP response into a typed [`ProviderError`], using Google's
/// error message when present or a concise trimmed body snippet otherwise.
fn parse_failure(status: u16, body: &str) -> ProviderError {
    let detail = match serde_json::from_str::<ErrorEnvelope>(body) {
        Ok(envelope) if !envelope.error.message.trim().is_empty() => {
            envelope.error.message.trim().to_owned()
        }
        _ => {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                "empty error body".to_owned()
            } else {
                trimmed.chars().take(MAX_ERROR_SNIPPET_CHARS).collect()
            }
        }
    };
    match status {
        401 | 403 => ProviderError::Unauthorized { status, detail },
        429 => ProviderError::RateLimited { status, detail },
        400 => ProviderError::BadRequest { status, detail },
        404 => ProviderError::ModelNotFound { status, detail },
        500..=599 => ProviderError::ServerUnavailable { status, detail },
        _ => ProviderError::HttpStatus { status, detail },
    }
}

fn transport_failure(error: reqwest::Error) -> ProviderError {
    let is_timeout = error.is_timeout();
    let detail = error.without_url().to_string();
    if is_timeout {
        ProviderError::Timeout { detail }
    } else {
        ProviderError::Unreachable { detail }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelName;
    use serde_json::json;

    #[test]
    fn request_body_maps_roles_and_asks_for_unstored_json() {
        let request = AiRequest {
            system_prompt: "You are the DM.".into(),
            messages: vec![
                AiMessage::User {
                    text: "I push the cart.".into(),
                },
                AiMessage::Assistant {
                    text: "{\"ok\":true}".into(),
                    provider_turn: None,
                },
            ],
            response_schema: None,
        };
        let body = serde_json::to_value(request_body("gemini-3.5-flash-lite", &request))
            .expect("serializes");
        assert_eq!(
            body,
            json!({
                "model": "gemini-3.5-flash-lite",
                "system_instruction": "You are the DM.",
                "input": [
                    {"type": "user_input", "content": [{"type": "text", "text": "I push the cart."}]},
                    {"type": "model_output", "content": [{"type": "text", "text": "{\"ok\":true}"}]}
                ],
                "response_format": {"type": "text", "mime_type": "application/json"},
                "store": false
            })
        );
    }

    #[test]
    fn request_body_includes_response_schema_when_present() {
        let request = AiRequest::for_resolution("You are the DM.", "I look around.");
        let body = serde_json::to_value(request_body("gemini-3.5-flash-lite", &request))
            .expect("serializes");
        assert_eq!(
            body["response_format"],
            json!({
                "type": "text",
                "mime_type": "application/json",
                "schema": zeta_core::resolution::AiResolution::json_schema()
            })
        );
    }

    #[test]
    fn success_joins_the_final_output_and_skips_thoughts_and_unknown_steps() {
        let body = json!({
            "id": "abc",
            "status": "completed",
            "steps": [
                {"type": "user_input", "content": [{"type": "text", "text": "echo"}]},
                {"type": "model_output", "content": [{"type": "text", "text": "draft"}]},
                {"type": "thought", "signature": "s", "summary": []},
                {"type": "model_output", "content": [{"type": "text", "text": "{\"ok\":"}]},
                {"type": "model_output", "content": [
                    {"type": "text", "text": "true}"},
                    {"type": "image", "data": "..."}
                ]}
            ],
            "usage": {"total_tokens": 10}
        })
        .to_string();
        let response = parse_success(&body).expect("has text");
        assert_eq!(response.text, "{\"ok\":true}");
        assert_eq!(
            response.provider_turn,
            Some(json!([
                {"type": "model_output", "content": [{"type": "text", "text": "draft"}]},
                {"type": "thought", "signature": "s", "summary": []},
                {"type": "model_output", "content": [{"type": "text", "text": "{\"ok\":"}]},
                {"type": "model_output", "content": [
                    {"type": "text", "text": "true}"},
                    {"type": "image", "data": "..."}
                ]}
            ]))
        );
    }

    #[test]
    fn thought_and_model_output_round_trip_verbatim_into_next_request_body() {
        let thought = json!({"type": "thought", "signature": "sig-abc", "summary": [{"type": "text", "text": "thinking"}]});
        let output =
            json!({"type": "model_output", "content": [{"type": "text", "text": "{\"ok\":true}"}]});
        let body = json!({
            "status": "completed",
            "steps": [
                {"type": "user_input", "content": [{"type": "text", "text": "ignored"}]},
                thought,
                output
            ]
        })
        .to_string();
        let response = parse_success(&body).expect("parses");
        let next_request = AiRequest {
            system_prompt: "You are the DM.".into(),
            messages: vec![
                AiMessage::User {
                    text: "First turn".into(),
                },
                AiMessage::Assistant {
                    text: response.text,
                    provider_turn: response.provider_turn,
                },
                AiMessage::User {
                    text: "Retry turn".into(),
                },
            ],
            response_schema: None,
        };
        let wire = serde_json::to_value(request_body("gemini-3.5-flash-lite", &next_request))
            .expect("serializes");
        assert_eq!(
            wire["input"],
            json!([
                {"type": "user_input", "content": [{"type": "text", "text": "First turn"}]},
                thought,
                output,
                {"type": "user_input", "content": [{"type": "text", "text": "Retry turn"}]}
            ])
        );
    }

    #[test]
    fn unfinished_interactions_report_their_status() {
        let body = json!({"status": "incomplete", "steps": []}).to_string();
        let error = parse_success(&body).expect_err("incomplete");
        assert!(error.to_string().contains("incomplete"));
    }

    #[test]
    fn completed_interactions_without_text_are_failures() {
        let body = json!({"status": "completed", "steps": [{"type": "thought"}]}).to_string();
        assert!(parse_success(&body).is_err());
    }

    #[test]
    fn failures_classify_by_http_status_and_keep_provider_message() {
        let rate_limited = json!({"error": {"code": 429, "message": "Quota exceeded"}}).to_string();
        assert_eq!(
            parse_failure(429, &rate_limited),
            ProviderError::RateLimited {
                status: 429,
                detail: "Quota exceeded".into(),
            }
        );
        let auth = json!({"error": {"code": 403, "message": "API key invalid"}}).to_string();
        assert_eq!(
            parse_failure(403, &auth),
            ProviderError::Unauthorized {
                status: 403,
                detail: "API key invalid".into(),
            }
        );
        let bad_req = json!({"error": {"code": 400, "message": "Invalid schema"}}).to_string();
        assert_eq!(
            parse_failure(400, &bad_req),
            ProviderError::BadRequest {
                status: 400,
                detail: "Invalid schema".into(),
            }
        );
        let missing = json!({"error": {"code": 404, "message": "Model not found"}}).to_string();
        assert_eq!(
            parse_failure(404, &missing),
            ProviderError::ModelNotFound {
                status: 404,
                detail: "Model not found".into(),
            }
        );
        assert_eq!(
            parse_failure(503, "upstream overloaded"),
            ProviderError::ServerUnavailable {
                status: 503,
                detail: "upstream overloaded".into(),
            }
        );
        assert_eq!(
            parse_failure(418, ""),
            ProviderError::HttpStatus {
                status: 418,
                detail: "empty error body".into(),
            }
        );
    }

    #[test]
    fn debug_output_never_contains_the_key() {
        let provider = GeminiProvider::new(GeminiConfig {
            api_key: ApiKey::new("secret-value"),
            model: ModelName::new("gemini-3.5-flash-lite").expect("valid name"),
        })
        .expect("client builds");
        assert!(!format!("{provider:?}").contains("secret-value"));
    }
}
