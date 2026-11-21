//! The Gemini API provider (Google AI Studio), used by the prototype on the free tier.
//!
//! Rules: `docs/architecture/ai_orchestration.md` Section 6. Mapping between Zeta requests
//! and the Gemini wire format is kept in pure functions, so it is tested without a network.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeta_core::ai::{AiMessage, AiProvider, AiRequest, AiResponse, ProviderError};

use crate::config::{ApiKey, GeminiConfig};
use crate::settings::AI_REQUEST_TIMEOUT_SECONDS;

/// Base of every Gemini model path. The key is sent in a header, never in the path, so it
/// cannot appear in error messages that include the address.
const MODELS_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";

/// Header that carries the API key.
const API_KEY_HEADER: &str = "x-goog-api-key";

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
    endpoint: String,
    api_key: ApiKey,
}

impl GeminiProvider {
    /// Creates a provider for the configured model.
    ///
    /// # Errors
    /// [`GeminiSetupError::Client`] if the HTTP client cannot be built.
    pub fn new(config: GeminiConfig) -> Result<Self, GeminiSetupError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(AI_REQUEST_TIMEOUT_SECONDS))
            .build()?;
        Ok(Self {
            client,
            endpoint: format!("{MODELS_BASE}/{}:generateContent", config.model.as_str()),
            api_key: config.api_key,
        })
    }
}

impl AiProvider for GeminiProvider {
    async fn complete(&self, request: &AiRequest) -> Result<AiResponse, ProviderError> {
        let response = self
            .client
            .post(&self.endpoint)
            .header(API_KEY_HEADER, self.api_key.expose())
            .json(&request_body(request))
            .send()
            .await
            .map_err(|error| transport_failure(&error))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| transport_failure(&error))?;
        if status.is_success() {
            parse_success(&body)
        } else {
            Err(parse_failure(status.as_u16(), &body))
        }
    }
}

// Wire format: https://ai.google.dev/api/generate-content. Only the fields Zeta uses are
// modeled. Responses do not deny unknown fields, because Google adds fields over time.

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerateContentRequest<'a> {
    system_instruction: Content<'a>,
    contents: Vec<Content<'a>>,
    generation_config: GenerationConfig,
}

#[derive(Debug, PartialEq, Serialize)]
struct Content<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<&'static str>,
    parts: Vec<Part<'a>>,
}

#[derive(Debug, PartialEq, Serialize)]
struct Part<'a> {
    text: &'a str,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig {
    response_mime_type: &'static str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateContentResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    prompt_feedback: Option<PromptFeedback>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    content: Option<CandidateContent>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CandidateContent {
    #[serde(default)]
    parts: Vec<CandidatePart>,
}

#[derive(Debug, Deserialize)]
struct CandidatePart {
    text: Option<String>,
    /// True for a model's reasoning summary, which is not part of the answer.
    #[serde(default)]
    thought: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromptFeedback {
    block_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    message: String,
}

/// Maps a Zeta request to the Gemini request body.
///
/// Every AI DM request expects a JSON resolution, so JSON output is always requested. When a
/// request kind needs plain text, `AiRequest` gains a response format field.
fn request_body(request: &AiRequest) -> GenerateContentRequest<'_> {
    GenerateContentRequest {
        system_instruction: Content {
            role: None,
            parts: vec![Part {
                text: &request.system_prompt,
            }],
        },
        contents: request.messages.iter().map(message_content).collect(),
        generation_config: GenerationConfig {
            response_mime_type: "application/json",
        },
    }
}

fn message_content(message: &AiMessage) -> Content<'_> {
    let (role, text) = match message {
        AiMessage::User { text } => ("user", text),
        AiMessage::Assistant { text } => ("model", text),
    };
    Content {
        role: Some(role),
        parts: vec![Part { text }],
    }
}

/// Extracts the answer text from a successful response body.
fn parse_success(body: &str) -> Result<AiResponse, ProviderError> {
    let response: GenerateContentResponse =
        serde_json::from_str(body).map_err(|error| ProviderError::Failed {
            message: format!("unreadable Gemini response: {error}"),
        })?;
    if let Some(reason) = response
        .prompt_feedback
        .and_then(|feedback| feedback.block_reason)
    {
        return Err(ProviderError::Failed {
            message: format!("Gemini blocked the prompt: {reason}"),
        });
    }
    let Some(candidate) = response.candidates.into_iter().next() else {
        return Err(ProviderError::Failed {
            message: "Gemini returned no candidates".to_owned(),
        });
    };
    let text: String = candidate
        .content
        .into_iter()
        .flat_map(|content| content.parts)
        .filter(|part| !part.thought)
        .filter_map(|part| part.text)
        .collect();
    if text.is_empty() {
        let reason = candidate
            .finish_reason
            .unwrap_or_else(|| "unknown".to_owned());
        return Err(ProviderError::Failed {
            message: format!("Gemini returned no text, finish reason {reason}"),
        });
    }
    Ok(AiResponse { text })
}

/// Describes a non-success response, using Google's error message when the body has one.
fn parse_failure(status: u16, body: &str) -> ProviderError {
    let message = match serde_json::from_str::<ErrorEnvelope>(body) {
        Ok(envelope) => format!("Gemini status {status}: {}", envelope.error.message),
        Err(_) => format!("Gemini status {status} with an unreadable body"),
    };
    ProviderError::Failed { message }
}

fn transport_failure(error: &reqwest::Error) -> ProviderError {
    ProviderError::Failed {
        message: format!("Gemini request failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelName;
    use serde_json::json;

    #[test]
    fn request_body_maps_roles_and_asks_for_json() {
        let request = AiRequest {
            system_prompt: "You are the DM.".into(),
            messages: vec![
                AiMessage::User {
                    text: "I push the cart.".into(),
                },
                AiMessage::Assistant {
                    text: "{\"ok\":true}".into(),
                },
            ],
        };
        let body = serde_json::to_value(request_body(&request)).expect("serializes");
        assert_eq!(
            body,
            json!({
                "systemInstruction": {"parts": [{"text": "You are the DM."}]},
                "contents": [
                    {"role": "user", "parts": [{"text": "I push the cart."}]},
                    {"role": "model", "parts": [{"text": "{\"ok\":true}"}]}
                ],
                "generationConfig": {"responseMimeType": "application/json"}
            })
        );
    }

    #[test]
    fn success_joins_answer_parts_and_skips_thoughts() {
        let body = json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"text": "thinking...", "thought": true},
                    {"text": "{\"ok\":"},
                    {"text": "true}"}
                ]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"totalTokenCount": 10}
        })
        .to_string();
        let response = parse_success(&body).expect("has text");
        assert_eq!(response.text, "{\"ok\":true}");
    }

    #[test]
    fn blocked_prompts_are_failures() {
        let body = json!({"promptFeedback": {"blockReason": "SAFETY"}}).to_string();
        let error = parse_success(&body).expect_err("blocked");
        assert!(error.to_string().contains("SAFETY"));
    }

    #[test]
    fn candidates_without_text_report_the_finish_reason() {
        let body = json!({"candidates": [{"finishReason": "MAX_TOKENS"}]}).to_string();
        let error = parse_success(&body).expect_err("no text");
        assert!(error.to_string().contains("MAX_TOKENS"));
    }

    #[test]
    fn failures_use_the_provider_error_message() {
        let body = json!({"error": {"code": 429, "message": "Quota exceeded", "status": "RESOURCE_EXHAUSTED"}})
            .to_string();
        let error = parse_failure(429, &body);
        assert_eq!(
            error,
            ProviderError::Failed {
                message: "Gemini status 429: Quota exceeded".into()
            }
        );
    }

    #[test]
    fn endpoint_names_the_model_and_never_contains_the_key() {
        let provider = GeminiProvider::new(GeminiConfig {
            api_key: ApiKey::new("secret-value"),
            model: ModelName::new("gemini-4-hydrogen").expect("valid name"),
        })
        .expect("client builds");
        assert_eq!(
            provider.endpoint,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-4-hydrogen:generateContent"
        );
        assert!(!format!("{provider:?}").contains("secret-value"));
    }
}
