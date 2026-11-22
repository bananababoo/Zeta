//! The provider trait, the provider-agnostic request and response types, and recording.

use std::future::Future;
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 1 message in an AI DM conversation after the system prompt.
///
/// Stored in the Event Log. New kinds of message, such as inspect tool calls, are added as
/// new variants; existing variants never change shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum AiMessage {
    /// Sent by the server.
    User {
        /// Message text.
        text: String,
    },
    /// Returned by the AI DM.
    Assistant {
        /// Message text.
        text: String,
        /// Provider-specific turn steps preserved verbatim for multi-turn retries.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_turn: Option<serde_json::Value>,
    },
}

/// 1 request to an AI provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRequest {
    /// Instructions to the AI DM. Never contains player text.
    pub system_prompt: String,
    /// The conversation so far, oldest first.
    pub messages: Vec<AiMessage>,
    /// Optional JSON Schema enforcing the shape of the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<serde_json::Value>,
}

impl AiRequest {
    /// Creates an initial request expecting an [`AiResolution`](crate::resolution::AiResolution)
    /// JSON response.
    pub fn for_resolution(system_prompt: impl Into<String>, user_text: impl Into<String>) -> Self {
        Self {
            system_prompt: system_prompt.into(),
            messages: vec![AiMessage::User {
                text: user_text.into(),
            }],
            response_schema: Some(crate::resolution::AiResolution::json_schema()),
        }
    }
}

/// 1 response from an AI provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiResponse {
    /// The raw text the model returned.
    pub text: String,
    /// Provider-specific turn steps to resend verbatim on the next turn of this conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_turn: Option<serde_json::Value>,
}

/// A request and the response it received, as recorded for replay and the Event Log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiExchange {
    /// What was sent.
    pub request: AiRequest,
    /// What came back.
    pub response: AiResponse,
}

/// Why a provider returned no response.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ProviderError {
    /// The provider could not be reached or refused the request.
    #[error("AI provider failed: {message}")]
    Failed {
        /// The provider's description of the failure.
        message: String,
    },
    /// A replay was asked for more requests than were recorded.
    #[error("replay has no recorded exchange at position {position}")]
    ReplayExhausted {
        /// Position of the request, from 0.
        position: usize,
    },
    /// A replayed request differs from the recorded one.
    #[error("request at position {position} differs from the recording")]
    ReplayMismatch {
        /// Position of the request, from 0.
        position: usize,
    },
}

/// Something that answers AI DM requests: a real provider, a recording, or a replay.
///
/// The returned future is `Send` so the server can run calls on any thread.
pub trait AiProvider {
    /// Sends 1 request and waits for the response.
    fn complete(
        &self,
        request: &AiRequest,
    ) -> impl Future<Output = Result<AiResponse, ProviderError>> + Send;
}

/// Wraps a provider and keeps every successful exchange, so it can be saved and replayed.
#[derive(Debug)]
pub struct RecordingProvider<P> {
    inner: P,
    exchanges: Mutex<Vec<AiExchange>>,
}

impl<P> RecordingProvider<P> {
    /// Starts recording calls to `inner`.
    pub fn new(inner: P) -> Self {
        Self {
            inner,
            exchanges: Mutex::new(Vec::new()),
        }
    }

    /// Every exchange recorded so far, oldest first.
    pub fn exchanges(&self) -> Vec<AiExchange> {
        self.exchanges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Stops recording and returns the exchanges.
    pub fn into_exchanges(self) -> Vec<AiExchange> {
        self.exchanges
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl<P: AiProvider + Sync> AiProvider for RecordingProvider<P> {
    async fn complete(&self, request: &AiRequest) -> Result<AiResponse, ProviderError> {
        let response = self.inner.complete(request).await?;
        // A poisoned lock only means another recording call panicked mid-push; the list
        // itself is still usable.
        self.exchanges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(AiExchange {
                request: request.clone(),
                response: response.clone(),
            });
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::ReplayProvider;
    use crate::test_support::block_on;

    fn request(text: &str) -> AiRequest {
        AiRequest {
            system_prompt: "system".into(),
            messages: vec![AiMessage::User { text: text.into() }],
            response_schema: None,
        }
    }

    #[test]
    fn recording_keeps_exchanges_that_replay_answers_identically() {
        let original = vec![
            AiExchange {
                request: request("a"),
                response: AiResponse {
                    text: "1".into(),
                    provider_turn: None,
                },
            },
            AiExchange {
                request: request("b"),
                response: AiResponse {
                    text: "2".into(),
                    provider_turn: None,
                },
            },
        ];
        let recorder = RecordingProvider::new(ReplayProvider::new(original.clone()));
        block_on(recorder.complete(&request("a"))).expect("recorded");
        block_on(recorder.complete(&request("b"))).expect("recorded");
        assert_eq!(recorder.into_exchanges(), original);
    }

    #[test]
    fn messages_serialize_with_a_role_tag() {
        let json = serde_json::to_value(AiMessage::Assistant {
            text: "hi".into(),
            provider_turn: None,
        })
        .expect("serializes");
        assert_eq!(json, serde_json::json!({"role": "assistant", "text": "hi"}));
    }

    #[test]
    fn legacy_json_without_schema_or_provider_turn_deserializes_and_omits_none() {
        let legacy_request = serde_json::json!({
            "system_prompt": "sys",
            "messages": [
                {"role": "user", "text": "hi"},
                {"role": "assistant", "text": "hello"}
            ]
        });
        let parsed_request: AiRequest =
            serde_json::from_value(legacy_request.clone()).expect("parses request");
        assert_eq!(parsed_request.response_schema, None);
        assert_eq!(
            serde_json::to_value(&parsed_request).expect("serializes"),
            legacy_request
        );

        let legacy_response = serde_json::json!({"text": "ok"});
        let parsed_response: AiResponse =
            serde_json::from_value(legacy_response.clone()).expect("parses response");
        assert_eq!(parsed_response.provider_turn, None);
        assert_eq!(
            serde_json::to_value(&parsed_response).expect("serializes"),
            legacy_response
        );
    }

    #[test]
    fn for_resolution_populates_resolution_json_schema() {
        let request = AiRequest::for_resolution("sys", "act");
        assert_eq!(request.system_prompt, "sys");
        assert_eq!(
            request.messages,
            vec![AiMessage::User { text: "act".into() }]
        );
        assert_eq!(
            request.response_schema,
            Some(crate::resolution::AiResolution::json_schema())
        );
    }
}
