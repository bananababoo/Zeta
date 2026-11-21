//! Real AI providers, which implement `zeta_core::ai::AiProvider` over the network.
//!
//! Rules: `docs/architecture/ai_orchestration.md` Section 6. Game code depends only on the
//! trait. The server builds a [`ConfiguredProvider`] from configuration, so switching
//! provider or model is a change to `.env`, never to game code.
//!
//! Adding a provider: write its module here, add a variant to [`ConfiguredProvider`] and to
//! `crate::config::AiConfig`, and the compiler lists every `match` that must handle it.

mod gemini;

use thiserror::Error;
use zeta_core::ai::{AiProvider, AiRequest, AiResponse, ProviderError};

use crate::config::AiConfig;

pub use gemini::{GeminiProvider, GeminiSetupError};

/// Whichever provider the configuration selects.
#[derive(Debug)]
pub enum ConfiguredProvider {
    /// Google's Gemini API.
    Gemini(GeminiProvider),
}

/// Why the configured provider could not be created.
#[derive(Debug, Error)]
pub enum ProviderSetupError {
    /// The Gemini provider failed to start.
    #[error(transparent)]
    Gemini(#[from] GeminiSetupError),
}

impl ConfiguredProvider {
    /// Creates the provider named in `config`.
    ///
    /// # Errors
    /// [`ProviderSetupError`] if that provider cannot start.
    pub fn new(config: AiConfig) -> Result<Self, ProviderSetupError> {
        match config {
            AiConfig::Gemini(config) => Ok(Self::Gemini(GeminiProvider::new(config)?)),
        }
    }
}

impl AiProvider for ConfiguredProvider {
    async fn complete(&self, request: &AiRequest) -> Result<AiResponse, ProviderError> {
        match self {
            Self::Gemini(provider) => provider.complete(request).await,
        }
    }
}
