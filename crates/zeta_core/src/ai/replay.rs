//! Answers AI DM requests from a recording, without calling or paying a provider.

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::ai::{AiExchange, AiProvider, AiRequest, AiResponse, ProviderError};

/// Replays recorded exchanges in order.
///
/// Each request must equal the recorded request at the same position, so any change to how
/// requests are built shows up as a [`ProviderError::ReplayMismatch`] in tests.
#[derive(Debug)]
pub struct ReplayProvider {
    exchanges: Vec<AiExchange>,
    position: AtomicUsize,
}

impl ReplayProvider {
    /// Replays `exchanges` from the first.
    pub fn new(exchanges: Vec<AiExchange>) -> Self {
        Self {
            exchanges,
            position: AtomicUsize::new(0),
        }
    }

    /// Whether every recorded exchange has been replayed.
    pub fn is_finished(&self) -> bool {
        self.position.load(Ordering::Relaxed) >= self.exchanges.len()
    }
}

impl AiProvider for ReplayProvider {
    fn complete(
        &self,
        request: &AiRequest,
    ) -> impl Future<Output = Result<AiResponse, ProviderError>> + Send {
        let position = self.position.fetch_add(1, Ordering::Relaxed);
        let result = match self.exchanges.get(position) {
            None => Err(ProviderError::ReplayExhausted { position }),
            Some(recorded) if recorded.request == *request => Ok(recorded.response.clone()),
            Some(_) => Err(ProviderError::ReplayMismatch { position }),
        };
        // Answers come from memory, so the future is ready at once.
        std::future::ready(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::AiMessage;
    use crate::test_support::block_on;

    fn exchange(question: &str, answer: &str) -> AiExchange {
        AiExchange {
            request: AiRequest {
                system_prompt: String::new(),
                messages: vec![AiMessage::User {
                    text: question.into(),
                }],
            },
            response: AiResponse {
                text: answer.into(),
            },
        }
    }

    #[test]
    fn replays_responses_in_order_then_reports_exhaustion() {
        let first = exchange("q", "a");
        let replay = ReplayProvider::new(vec![first.clone()]);
        assert_eq!(
            block_on(replay.complete(&first.request)),
            Ok(first.response)
        );
        assert!(replay.is_finished());
        assert_eq!(
            block_on(replay.complete(&first.request)),
            Err(ProviderError::ReplayExhausted { position: 1 })
        );
    }

    #[test]
    fn a_changed_request_is_a_mismatch() {
        let replay = ReplayProvider::new(vec![exchange("q", "a")]);
        let changed = exchange("different", "a").request;
        assert_eq!(
            block_on(replay.complete(&changed)),
            Err(ProviderError::ReplayMismatch { position: 0 })
        );
    }
}
