//! The validation retry loop for 1 Freeform Action Prompt.
//!
//! Rules: `docs/architecture/ai_orchestration.md` Section 4. On a structural failure the
//! exact validation errors go back to the AI DM, up to the retry cap. Nothing from a failed
//! response is ever returned, so a chain reaction never applies halfway.

use crate::ai::{AiExchange, AiMessage, AiProvider, AiRequest, ProviderError};
use crate::grid::MapBounds;
use crate::resolution::AiResolution;
use crate::snapshot::Snapshot;
use crate::validation::{ValidationErrors, parse_and_validate};

/// A resolution that passed validation, with every exchange it took to get there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAction {
    /// The valid resolution.
    pub resolution: AiResolution,
    /// Every request and response, including failed attempts, for the Event Log.
    pub exchanges: Vec<AiExchange>,
}

/// Why no valid resolution was produced. The caller applies nothing, releases the
/// `PendingAiResolution` lock, and asks the player to rephrase.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    /// The provider returned no response.
    #[error("the AI provider returned no response")]
    Provider {
        /// The provider's failure.
        source: ProviderError,
        /// Exchanges completed before the failure.
        exchanges: Vec<AiExchange>,
    },
    /// Every attempt failed validation.
    #[error("every attempt failed validation; last errors:\n{last_errors}")]
    RetriesExhausted {
        /// Errors in the final attempt.
        last_errors: ValidationErrors,
        /// Every attempt.
        exchanges: Vec<AiExchange>,
    },
}

/// Asks the AI DM to resolve an action, retrying with the validation errors on failure.
///
/// Makes at most `retry_cap + 1` provider calls.
///
/// # Errors
///
/// Returns [`ResolveError`] if the provider fails or every attempt fails validation.
pub async fn resolve_action<P: AiProvider>(
    provider: &P,
    request: AiRequest,
    snapshot: &Snapshot,
    bounds: MapBounds,
    retry_cap: u32,
) -> Result<ResolvedAction, ResolveError> {
    let mut request = request;
    let mut exchanges = Vec::new();
    let mut retries_left = retry_cap;
    loop {
        let response = match provider.complete(&request).await {
            Ok(response) => response,
            Err(source) => return Err(ResolveError::Provider { source, exchanges }),
        };
        let checked = parse_and_validate(&response.text, snapshot, bounds);
        exchanges.push(AiExchange {
            request: request.clone(),
            response: response.clone(),
        });
        let errors = match checked {
            Ok(resolution) => {
                return Ok(ResolvedAction {
                    resolution,
                    exchanges,
                });
            }
            Err(errors) => errors,
        };
        if retries_left == 0 {
            return Err(ResolveError::RetriesExhausted {
                last_errors: errors,
                exchanges,
            });
        }
        retries_left -= 1;
        request.messages.push(AiMessage::Assistant {
            text: response.text,
        });
        request.messages.push(AiMessage::User {
            text: retry_message(&errors),
        });
    }
}

/// The message that returns validation errors to the AI DM.
fn retry_message(errors: &ValidationErrors) -> String {
    format!(
        "Your response failed validation. Fix these errors and return the complete corrected response:\n{errors}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{AiResponse, ReplayProvider};
    use crate::snapshot::PerceptionSettings;
    use crate::snapshot::tests::entity;
    use crate::test_support::block_on;

    const BOUNDS: MapBounds = MapBounds {
        width: 10,
        height: 10,
    };
    const INVALID: &str = r#"{"ok": true, "chk": null, "fx": [{"op": "destroy", "id": 9, "reason": "x"}], "fail": []}"#;
    const VALID: &str =
        r#"{"ok": true, "chk": null, "fx": [{"op": "narrate", "text": "Done."}], "fail": []}"#;

    fn snapshot() -> Snapshot {
        Snapshot::build(
            entity(1, 0, 0),
            [],
            |_| false,
            PerceptionSettings::default(),
        )
    }

    fn first_request() -> AiRequest {
        AiRequest {
            system_prompt: "system".into(),
            messages: vec![AiMessage::User {
                text: "I wave.".into(),
            }],
        }
    }

    /// Builds the exchanges a well-behaved loop produces for these responses.
    fn script(responses: &[&str]) -> Vec<AiExchange> {
        let mut request = first_request();
        let mut exchanges = Vec::new();
        for text in responses {
            exchanges.push(AiExchange {
                request: request.clone(),
                response: AiResponse {
                    text: (*text).into(),
                },
            });
            if let Err(errors) = parse_and_validate(text, &snapshot(), BOUNDS) {
                request.messages.push(AiMessage::Assistant {
                    text: (*text).into(),
                });
                request.messages.push(AiMessage::User {
                    text: retry_message(&errors),
                });
            }
        }
        exchanges
    }

    #[test]
    fn valid_first_response_needs_no_retry() {
        let replay = ReplayProvider::new(script(&[VALID]));
        let resolved = block_on(resolve_action(
            &replay,
            first_request(),
            &snapshot(),
            BOUNDS,
            2,
        ))
        .expect("valid response");
        assert_eq!(resolved.exchanges.len(), 1);
    }

    #[test]
    fn invalid_response_is_retried_with_its_errors() {
        let exchanges = script(&[INVALID, VALID]);
        let retry_text = match exchanges[1].request.messages.last() {
            Some(AiMessage::User { text }) => text.clone(),
            other => panic!("expected a retry message, got {other:?}"),
        };
        assert!(retry_text.contains("fx[0] (destroy): id 9 is not in the Snapshot"));

        let replay = ReplayProvider::new(exchanges);
        let resolved = block_on(resolve_action(
            &replay,
            first_request(),
            &snapshot(),
            BOUNDS,
            2,
        ))
        .expect("second response is valid");
        assert_eq!(resolved.exchanges.len(), 2);
        assert!(replay.is_finished());
    }

    #[test]
    fn exhausted_retries_return_no_resolution() {
        let replay = ReplayProvider::new(script(&[INVALID, INVALID]));
        let result = block_on(resolve_action(
            &replay,
            first_request(),
            &snapshot(),
            BOUNDS,
            1,
        ));
        assert!(matches!(
            result,
            Err(ResolveError::RetriesExhausted { ref exchanges, .. }) if exchanges.len() == 2
        ));
    }

    #[test]
    fn provider_failure_stops_the_loop() {
        let replay = ReplayProvider::new(vec![]);
        let result = block_on(resolve_action(
            &replay,
            first_request(),
            &snapshot(),
            BOUNDS,
            2,
        ));
        assert!(matches!(
            result,
            Err(ResolveError::Provider {
                source: ProviderError::ReplayExhausted { position: 0 },
                ..
            })
        ));
    }
}
