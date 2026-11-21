//! Provider-agnostic AI DM calls: the provider trait, recording and replay, and the
//! validation retry loop.
//!
//! Rules: `docs/architecture/ai_orchestration.md` Sections 4 and 6. Real providers live in
//! the server, because they perform network calls.

mod provider;
mod replay;
mod retry;

pub use provider::{
    AiExchange, AiMessage, AiProvider, AiRequest, AiResponse, ProviderError, RecordingProvider,
};
pub use replay::ReplayProvider;
pub use retry::{ResolveError, ResolvedAction, resolve_action};
