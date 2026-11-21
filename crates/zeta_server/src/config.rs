//! Server configuration read from environment variables, with a local `.env` file as a
//! fallback for development.
//!
//! Secrets and machine-specific values live here and never in code. The variable names are
//! listed in `.env.example` at the repository root. Switching AI provider or model is a
//! change to `.env`, never to code.

use std::env::{self, VarError};
use std::fmt;

use thiserror::Error;

/// Environment variable naming the AI provider, such as `gemini`.
pub const AI_PROVIDER_VARIABLE: &str = "ZETA_AI_PROVIDER";

/// Environment variable holding the AI model name.
pub const AI_MODEL_VARIABLE: &str = "ZETA_AI_MODEL";

/// Environment variable holding the Gemini API key.
pub const GEMINI_API_KEY_VARIABLE: &str = "GEMINI_API_KEY";

/// Provider names accepted in [`AI_PROVIDER_VARIABLE`]. Adding a provider adds its name here
/// and a variant to [`AiConfig`].
pub const KNOWN_PROVIDERS: &[&str] = &["gemini"];

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
    /// The provider name is not one the server supports.
    #[error("{AI_PROVIDER_VARIABLE} is {value:?}; supported providers: {KNOWN_PROVIDERS:?}")]
    UnknownProvider {
        /// The rejected value.
        value: String,
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
    /// Checks and wraps a model name, such as `gemini-4-hydrogen`.
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

/// The AI provider the server uses, with that provider's settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AiConfig {
    /// Google's Gemini API.
    Gemini(GeminiConfig),
}

impl AiConfig {
    /// Reads the configuration from the environment, loading `.env` first if it exists.
    /// Variables already set in the environment take precedence over `.env`.
    ///
    /// # Errors
    /// [`ConfigError`] if a variable is missing, not Unicode, or invalid.
    pub fn from_environment() -> Result<Self, ConfigError> {
        // A missing `.env` is normal on a deployed server, where real environment variables
        // are used instead, so the result is ignored.
        let _ = dotenvy::dotenv();
        Self::from_lookup(env::var)
    }

    /// Reads the configuration through `lookup`, which returns a variable's value. Tests
    /// pass a fixed table here, so they never change the real environment.
    ///
    /// # Errors
    /// [`ConfigError`] if a variable is missing, not Unicode, or invalid.
    pub fn from_lookup(
        lookup: impl Fn(&'static str) -> Result<String, VarError>,
    ) -> Result<Self, ConfigError> {
        let provider = required_variable(&lookup, AI_PROVIDER_VARIABLE)?;
        let model = ModelName::new(&required_variable(&lookup, AI_MODEL_VARIABLE)?)?;
        match provider.as_str() {
            "gemini" => Ok(Self::Gemini(GeminiConfig {
                api_key: ApiKey::new(required_variable(&lookup, GEMINI_API_KEY_VARIABLE)?),
                model,
            })),
            _ => Err(ConfigError::UnknownProvider { value: provider }),
        }
    }

    /// The configured model, whichever provider serves it.
    pub fn model(&self) -> &ModelName {
        match self {
            Self::Gemini(config) => &config.model,
        }
    }
}

fn required_variable(
    lookup: impl Fn(&'static str) -> Result<String, VarError>,
    variable: &'static str,
) -> Result<String, ConfigError> {
    match lookup(variable) {
        Ok(value) if !value.trim().is_empty() => Ok(value.trim().to_owned()),
        Ok(_) | Err(VarError::NotPresent) => Err(ConfigError::Missing { variable }),
        Err(VarError::NotUnicode(_)) => Err(ConfigError::NotUnicode { variable }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup_in(
        table: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&'static str) -> Result<String, VarError> {
        move |variable| {
            table
                .iter()
                .find(|(name, _)| *name == variable)
                .map(|(_, value)| (*value).to_owned())
                .ok_or(VarError::NotPresent)
        }
    }

    #[test]
    fn gemini_configuration_is_read_from_the_variables() {
        let config = AiConfig::from_lookup(lookup_in(&[
            (AI_PROVIDER_VARIABLE, "gemini"),
            (AI_MODEL_VARIABLE, " gemini-4-hydrogen "),
            (GEMINI_API_KEY_VARIABLE, "key"),
        ]))
        .expect("valid configuration");
        assert_eq!(
            config,
            AiConfig::Gemini(GeminiConfig {
                api_key: ApiKey::new("key"),
                model: ModelName::new("gemini-4-hydrogen").expect("valid name"),
            })
        );
        assert_eq!(config.model().as_str(), "gemini-4-hydrogen");
    }

    #[test]
    fn unknown_providers_are_rejected() {
        let error = AiConfig::from_lookup(lookup_in(&[
            (AI_PROVIDER_VARIABLE, "other"),
            (AI_MODEL_VARIABLE, "model-1"),
        ]))
        .expect_err("unknown provider");
        assert_eq!(
            error,
            ConfigError::UnknownProvider {
                value: "other".into()
            }
        );
    }

    #[test]
    fn missing_or_blank_variables_are_reported_by_name() {
        let error = AiConfig::from_lookup(lookup_in(&[
            (AI_PROVIDER_VARIABLE, "gemini"),
            (AI_MODEL_VARIABLE, "gemini-4-hydrogen"),
            (GEMINI_API_KEY_VARIABLE, "  "),
        ]))
        .expect_err("blank key");
        assert_eq!(
            error,
            ConfigError::Missing {
                variable: GEMINI_API_KEY_VARIABLE
            }
        );
    }

    #[test]
    fn api_key_debug_output_never_shows_the_key() {
        let key = ApiKey::new("secret-value");
        assert!(!format!("{key:?}").contains("secret-value"));
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
