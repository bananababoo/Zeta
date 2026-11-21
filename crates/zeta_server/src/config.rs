//! Server configuration read from environment variables, with a local `.env` file as a
//! fallback for development.
//!
//! Secrets and machine-specific values live here and never in code. The variable names are
//! listed in `.env.example` at the repository root.

use std::env::{self, VarError};
use std::fmt;

use thiserror::Error;

/// Environment variable holding the Gemini API key.
pub const GEMINI_API_KEY_VARIABLE: &str = "GEMINI_API_KEY";

/// Environment variable holding the AI model name.
pub const AI_MODEL_VARIABLE: &str = "ZETA_AI_MODEL";

/// A configuration value that is missing or unusable.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ConfigError {
    /// The variable is unset or empty.
    #[error("{variable} is not set; copy .env.example to .env and fill it in")]
    Missing {
        /// Name of the variable.
        variable: &'static str,
    },
    /// The variable is set but is not valid Unicode.
    #[error("{variable} is not valid Unicode")]
    NotUnicode {
        /// Name of the variable.
        variable: &'static str,
    },
    /// The model name contains characters a provider model name never has.
    #[error(
        "{AI_MODEL_VARIABLE} is {value:?}; a model name uses only lowercase letters, digits, '.', and '-'"
    )]
    InvalidModelName {
        /// The rejected value.
        value: String,
    },
}

/// An AI provider key. Its `Debug` output is redacted so the key never reaches a log.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// Wraps a key.
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// The key itself, for the single place that sends it to the provider.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApiKey(<redacted>)")
    }
}

/// A provider model name, checked so it is safe to place in a request path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelName(String);

impl ModelName {
    /// Checks and wraps a model name, such as `gemini-2.5-flash`.
    ///
    /// # Errors
    /// [`ConfigError::InvalidModelName`] if the name is empty or contains anything other
    /// than lowercase ASCII letters, digits, `.`, and `-`.
    pub fn new(name: &str) -> Result<Self, ConfigError> {
        let is_valid = !name.is_empty()
            && name
                .chars()
                .all(|character| matches!(character, 'a'..='z' | '0'..='9' | '.' | '-'));
        if is_valid {
            Ok(Self(name.to_owned()))
        } else {
            Err(ConfigError::InvalidModelName {
                value: name.to_owned(),
            })
        }
    }

    /// The model name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Everything needed to call the Gemini API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeminiConfig {
    /// The API key from Google AI Studio.
    pub api_key: ApiKey,
    /// The model every AI DM request uses.
    pub model: ModelName,
}

impl GeminiConfig {
    /// Reads the configuration from the environment, loading `.env` first if it exists.
    /// Variables already set in the environment take precedence over `.env`.
    ///
    /// # Errors
    /// [`ConfigError`] if a variable is missing, not Unicode, or invalid.
    pub fn from_environment() -> Result<Self, ConfigError> {
        // A missing `.env` is normal on a deployed server, where real environment variables
        // are used instead, so the result is ignored.
        let _ = dotenvy::dotenv();
        Ok(Self {
            api_key: ApiKey::new(required_variable(GEMINI_API_KEY_VARIABLE)?),
            model: ModelName::new(&required_variable(AI_MODEL_VARIABLE)?)?,
        })
    }
}

fn required_variable(variable: &'static str) -> Result<String, ConfigError> {
    match env::var(variable) {
        Ok(value) if !value.trim().is_empty() => Ok(value.trim().to_owned()),
        Ok(_) | Err(VarError::NotPresent) => Err(ConfigError::Missing { variable }),
        Err(VarError::NotUnicode(_)) => Err(ConfigError::NotUnicode { variable }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_debug_output_never_shows_the_key() {
        let key = ApiKey::new("secret-value");
        assert!(!format!("{key:?}").contains("secret-value"));
    }

    #[test]
    fn model_names_accept_provider_style_names() {
        let model = ModelName::new("gemini-2.5-flash").expect("valid name");
        assert_eq!(model.as_str(), "gemini-2.5-flash");
    }

    #[test]
    fn model_names_reject_path_characters_and_empty_text() {
        for name in [
            "",
            "gemini/../x",
            "Gemini",
            "gemini flash",
            "gemini:generate",
        ] {
            assert!(
                matches!(
                    ModelName::new(name),
                    Err(ConfigError::InvalidModelName { .. })
                ),
                "{name:?} should be rejected"
            );
        }
    }
}
