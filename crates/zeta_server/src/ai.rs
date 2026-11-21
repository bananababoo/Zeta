//! Real AI providers, which implement `zeta_core::ai::AiProvider` over the network.
//!
//! Rules: `docs/architecture/ai_orchestration.md` Section 6. Game code depends only on the
//! trait, so a provider can be swapped by changing which type the server constructs.

mod gemini;

pub use gemini::{GeminiProvider, GeminiSetupError};
