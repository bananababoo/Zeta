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

/// Environment variable holding the PostgreSQL host name or address.
pub const POSTGRES_HOST_VARIABLE: &str = "POSTGRES_HOST";

/// Environment variable holding the PostgreSQL port number.
pub const POSTGRES_PORT_VARIABLE: &str = "POSTGRES_PORT";

/// Environment variable holding the PostgreSQL user name.
pub const POSTGRES_USER_VARIABLE: &str = "POSTGRES_USER";

/// Environment variable holding the PostgreSQL password.
pub const POSTGRES_PASSWORD_VARIABLE: &str = "POSTGRES_PASSWORD";

/// Environment variable holding the PostgreSQL database name.
pub const POSTGRES_DB_VARIABLE: &str = "POSTGRES_DB";

const DEFAULT_POSTGRES_HOST: &str = "127.0.0.1";
const DEFAULT_POSTGRES_PORT: u16 = 5432;

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
    /// A port variable is not a whole number from 1 to 65535.
    #[error("{variable} is {value:?}; expected a port number from 1 to 65535")]
    InvalidPort {
        /// Name of the variable.
        variable: &'static str,
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

/// A PostgreSQL password. Its `Debug` output is redacted so the password never reaches a log.
#[derive(Clone, PartialEq, Eq)]
pub struct DatabasePassword(String);

impl DatabasePassword {
    /// Wraps a password.
    pub fn new(password: impl Into<String>) -> Self {
        Self(password.into())
    }

    /// The password itself, for opening a database connection.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DatabasePassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DatabasePassword(<redacted>)")
    }
}

/// Everything needed to connect to PostgreSQL (`docs/architecture/persistence.md` Section 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseConfig {
    /// Database server host name or address.
    pub host: String,
    /// Database server port.
    pub port: u16,
    /// PostgreSQL user name.
    pub user: String,
    /// PostgreSQL password.
    pub password: DatabasePassword,
    /// PostgreSQL database name.
    pub database: String,
}

impl DatabaseConfig {
    /// Reads the database configuration from the environment, loading `.env` first if it
    /// exists. Variables already set in the environment take precedence over `.env`.
    ///
    /// # Errors
    /// [`ConfigError`] if a variable is missing, not Unicode, or invalid.
    pub fn from_environment() -> Result<Self, ConfigError> {
        let _ = dotenvy::dotenv();
        Self::from_lookup(env::var)
    }

    /// Reads the database configuration through `lookup`, which returns a variable's value.
    ///
    /// # Errors
    /// [`ConfigError`] if a variable is missing, not Unicode, or invalid.
    pub fn from_lookup(
        lookup: impl Fn(&'static str) -> Result<String, VarError>,
    ) -> Result<Self, ConfigError> {
        let host = optional_variable(&lookup, POSTGRES_HOST_VARIABLE)?
            .unwrap_or_else(|| DEFAULT_POSTGRES_HOST.to_owned());
        let port = match optional_variable(&lookup, POSTGRES_PORT_VARIABLE)? {
            Some(value) => parse_port(POSTGRES_PORT_VARIABLE, &value)?,
            None => DEFAULT_POSTGRES_PORT,
        };
        let user = required_variable(&lookup, POSTGRES_USER_VARIABLE)?;
        let password =
            DatabasePassword::new(required_variable(&lookup, POSTGRES_PASSWORD_VARIABLE)?);
        let database = required_variable(&lookup, POSTGRES_DB_VARIABLE)?;
        Ok(Self {
            host,
            port,
            user,
            password,
            database,
        })
    }
}

fn parse_port(variable: &'static str, raw: &str) -> Result<u16, ConfigError> {
    match raw.parse::<u16>() {
        Ok(port) if port != 0 => Ok(port),
        _ => Err(ConfigError::InvalidPort {
            variable,
            value: raw.to_owned(),
        }),
    }
}

fn optional_variable(
    lookup: impl Fn(&'static str) -> Result<String, VarError>,
    variable: &'static str,
) -> Result<Option<String>, ConfigError> {
    match lookup(variable) {
        Ok(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_owned()))
            }
        }
        Err(VarError::NotPresent) => Ok(None),
        Err(VarError::NotUnicode(_)) => Err(ConfigError::NotUnicode { variable }),
    }
}

fn required_variable(
    lookup: impl Fn(&'static str) -> Result<String, VarError>,
    variable: &'static str,
) -> Result<String, ConfigError> {
    optional_variable(lookup, variable)?.ok_or(ConfigError::Missing { variable })
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

    #[test]
    fn database_configuration_uses_defaults_when_host_and_port_are_unset_or_blank() {
        let config = DatabaseConfig::from_lookup(lookup_in(&[
            (POSTGRES_HOST_VARIABLE, "   "),
            (POSTGRES_USER_VARIABLE, "zeta"),
            (POSTGRES_PASSWORD_VARIABLE, "secret"),
            (POSTGRES_DB_VARIABLE, "zeta_db"),
        ]))
        .expect("valid database configuration");
        assert_eq!(
            config,
            DatabaseConfig {
                host: "127.0.0.1".to_owned(),
                port: 5432,
                user: "zeta".to_owned(),
                password: DatabasePassword::new("secret"),
                database: "zeta_db".to_owned(),
            }
        );
    }

    #[test]
    fn database_configuration_reads_explicit_host_and_port() {
        let config = DatabaseConfig::from_lookup(lookup_in(&[
            (POSTGRES_HOST_VARIABLE, " db.internal "),
            (POSTGRES_PORT_VARIABLE, " 5543 "),
            (POSTGRES_USER_VARIABLE, "zeta"),
            (POSTGRES_PASSWORD_VARIABLE, "secret"),
            (POSTGRES_DB_VARIABLE, "zeta_db"),
        ]))
        .expect("valid database configuration");
        assert_eq!(config.host, "db.internal");
        assert_eq!(config.port, 5543);
    }

    #[test]
    fn database_configuration_rejects_invalid_ports() {
        for bad_port in ["0", "65536", "-1", "postgres"] {
            let table: [(&'static str, &'static str); 4] = [
                (POSTGRES_PORT_VARIABLE, bad_port),
                (POSTGRES_USER_VARIABLE, "zeta"),
                (POSTGRES_PASSWORD_VARIABLE, "secret"),
                (POSTGRES_DB_VARIABLE, "zeta_db"),
            ];
            let error = DatabaseConfig::from_lookup(|variable| {
                table
                    .iter()
                    .find(|(name, _)| *name == variable)
                    .map(|(_, value)| (*value).to_owned())
                    .ok_or(VarError::NotPresent)
            })
            .expect_err("invalid port");
            assert_eq!(
                error,
                ConfigError::InvalidPort {
                    variable: POSTGRES_PORT_VARIABLE,
                    value: bad_port.to_owned(),
                }
            );
        }
    }

    #[test]
    fn database_configuration_reports_missing_required_variable() {
        let error = DatabaseConfig::from_lookup(lookup_in(&[
            (POSTGRES_USER_VARIABLE, "zeta"),
            (POSTGRES_PASSWORD_VARIABLE, "secret"),
        ]))
        .expect_err("missing db");
        assert_eq!(
            error,
            ConfigError::Missing {
                variable: POSTGRES_DB_VARIABLE,
            }
        );
    }

    #[test]
    fn database_password_debug_output_never_shows_the_password() {
        let password = DatabasePassword::new("super-secret");
        assert_eq!(format!("{password:?}"), "DatabasePassword(<redacted>)");
        let config = DatabaseConfig {
            host: "127.0.0.1".to_owned(),
            port: 5432,
            user: "zeta".to_owned(),
            password,
            database: "zeta".to_owned(),
        };
        assert!(!format!("{config:?}").contains("super-secret"));
    }
}
