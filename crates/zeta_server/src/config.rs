//! Server configuration read from environment variables, with a local `.env` file as a
//! fallback for development.
//!
//! Secrets and machine-specific values live here and never in code. The variable names are
//! listed in `.env.example` at the repository root. Switching AI provider or model is a
//! change to `.env`, never to code.

use std::collections::BTreeMap;
use std::env::{self, VarError};
use std::fmt;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr};

use thiserror::Error;
use zeta_core::settings::DEFAULT_SERVER_PORT;

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

/// Environment variable holding the IP address and port the game server listens on.
pub const LISTEN_ADDRESS_VARIABLE: &str = "ZETA_LISTEN_ADDRESS";

/// Environment variable that explicitly opts into volatile in-memory Event Log mode when
/// PostgreSQL is unavailable (`docs/architecture/persistence.md` Section 1).
pub const ALLOW_IN_MEMORY_LOG_VARIABLE: &str = "ZETA_ALLOW_IN_MEMORY_LOG";

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
    /// An address variable is not an IP address and port such as `127.0.0.1:7777`.
    #[error("{variable} is {value:?}; expected an IP address and port such as 127.0.0.1:7777")]
    InvalidAddress {
        /// Name of the variable.
        variable: &'static str,
        /// The rejected value.
        value: String,
    },
    /// A boolean flag variable is not one of `1`, `0`, `true`, `false`, `yes`, or `no`.
    #[error("{variable} is {value:?}; expected 1, 0, true, or false")]
    InvalidBoolean {
        /// Name of the variable.
        variable: &'static str,
        /// The rejected value.
        value: String,
    },
}

/// Reads whether [`ALLOW_IN_MEMORY_LOG_VARIABLE`] opts into volatile in-memory Event Log
/// mode, defaulting to `false` (fail fast when PostgreSQL is unavailable).
///
/// # Errors
/// [`ConfigError`] if the variable is set but not Unicode or not a valid boolean flag.
pub fn allow_in_memory_log_from_environment() -> Result<bool, ConfigError> {
    let dotenv = load_dotenv_file();
    allow_in_memory_log_from_lookup(|variable| lookup_with_dotenv(&dotenv, variable))
}

/// Reads [`ALLOW_IN_MEMORY_LOG_VARIABLE`] through `lookup`.
///
/// # Errors
/// [`ConfigError`] if the variable is set but not Unicode or not a valid boolean flag.
pub fn allow_in_memory_log_from_lookup(
    lookup: impl Fn(&'static str) -> Result<String, VarError>,
) -> Result<bool, ConfigError> {
    match optional_variable(&lookup, ALLOW_IN_MEMORY_LOG_VARIABLE)? {
        None => Ok(false),
        Some(value) => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => Ok(true),
            "0" | "false" | "no" => Ok(false),
            _ => Err(ConfigError::InvalidBoolean {
                variable: ALLOW_IN_MEMORY_LOG_VARIABLE,
                value,
            }),
        },
    }
}

/// Reads the address the game server listens on from [`LISTEN_ADDRESS_VARIABLE`], loading
/// `.env` first if it exists, and defaults to `127.0.0.1` on [`DEFAULT_SERVER_PORT`].
///
/// # Errors
/// [`ConfigError`] if the variable is set but not Unicode or not a socket address.
pub fn listen_address_from_environment() -> Result<SocketAddr, ConfigError> {
    let dotenv = load_dotenv_file();
    listen_address_from_lookup(|variable| lookup_with_dotenv(&dotenv, variable))
}

/// Reads the listen address through `lookup`, which returns a variable's value.
///
/// # Errors
/// [`ConfigError`] if the variable is set but not Unicode or not a socket address.
pub fn listen_address_from_lookup(
    lookup: impl Fn(&'static str) -> Result<String, VarError>,
) -> Result<SocketAddr, ConfigError> {
    match optional_variable(&lookup, LISTEN_ADDRESS_VARIABLE)? {
        Some(value) => value
            .parse::<SocketAddr>()
            .map_err(|_| ConfigError::InvalidAddress {
                variable: LISTEN_ADDRESS_VARIABLE,
                value,
            }),
        None => Ok(SocketAddr::from((Ipv4Addr::LOCALHOST, DEFAULT_SERVER_PORT))),
    }
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
        let dotenv = load_dotenv_file();
        Self::from_lookup(|variable| lookup_with_dotenv(&dotenv, variable))
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
        let dotenv = load_dotenv_file();
        Self::from_lookup(|variable| lookup_with_dotenv(&dotenv, variable))
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

fn lookup_with_dotenv(
    dotenv: &BTreeMap<String, String>,
    variable: &'static str,
) -> Result<String, VarError> {
    match env::var(variable) {
        Ok(value) => Ok(value),
        Err(VarError::NotPresent) => dotenv.get(variable).cloned().ok_or(VarError::NotPresent),
        Err(not_unicode) => Err(not_unicode),
    }
}

/// Reads the nearest `.env` file walking up from the current working directory.
///
/// Never mutates process-global environment variables, avoiding `unsafe` under Rust 2024.
fn load_dotenv_file() -> BTreeMap<String, String> {
    let Ok(directory) = env::current_dir() else {
        return BTreeMap::new();
    };
    for ancestor in directory.ancestors() {
        let candidate = ancestor.join(".env");
        if let Ok(contents) = fs::read_to_string(candidate) {
            return parse_dotenv(&contents);
        }
    }
    BTreeMap::new()
}

fn parse_dotenv(contents: &str) -> BTreeMap<String, String> {
    let mut entries = BTreeMap::new();
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((raw_key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let key = raw_key.trim();
        if key.is_empty() {
            continue;
        }
        let value = raw_value.trim();
        let unquoted = if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            &value[1..value.len() - 1]
        } else {
            value.split(" #").next().unwrap_or(value).trim()
        };
        entries.insert(key.to_owned(), unquoted.to_owned());
    }
    entries
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

    #[test]
    fn dotenv_parser_handles_comments_quotes_and_whitespace() {
        let contents = "\
# leading comment

ZETA_AI_PROVIDER=gemini
export ZETA_AI_MODEL = \"gemini-3.5-flash-lite\"
POSTGRES_USER = 'zeta'
POSTGRES_PASSWORD=secret # inline comment
EMPTY_VALUE=
";
        let parsed = parse_dotenv(contents);
        assert_eq!(parsed.get("ZETA_AI_PROVIDER"), Some(&"gemini".to_owned()));
        assert_eq!(
            parsed.get("ZETA_AI_MODEL"),
            Some(&"gemini-3.5-flash-lite".to_owned())
        );
        assert_eq!(parsed.get("POSTGRES_USER"), Some(&"zeta".to_owned()));
        assert_eq!(parsed.get("POSTGRES_PASSWORD"), Some(&"secret".to_owned()));
        assert_eq!(parsed.get("EMPTY_VALUE"), Some(&String::new()));
    }

    #[test]
    fn listen_address_defaults_to_the_local_machine_on_the_default_port() {
        let address = listen_address_from_lookup(lookup_in(&[])).expect("default address");
        assert_eq!(
            address,
            SocketAddr::from((Ipv4Addr::LOCALHOST, DEFAULT_SERVER_PORT))
        );
    }

    #[test]
    fn listen_address_reads_the_variable_and_rejects_bad_values() {
        let address =
            listen_address_from_lookup(lookup_in(&[(LISTEN_ADDRESS_VARIABLE, " 0.0.0.0:9000 ")]))
                .expect("valid address");
        assert_eq!(address, SocketAddr::from((Ipv4Addr::UNSPECIFIED, 9000)));
        assert_eq!(
            listen_address_from_lookup(lookup_in(&[(LISTEN_ADDRESS_VARIABLE, "localhost")])),
            Err(ConfigError::InvalidAddress {
                variable: LISTEN_ADDRESS_VARIABLE,
                value: "localhost".into(),
            })
        );
    }

    #[test]
    fn allow_in_memory_log_defaults_to_false_and_validates_flags() {
        assert_eq!(allow_in_memory_log_from_lookup(lookup_in(&[])), Ok(false));
        assert_eq!(
            allow_in_memory_log_from_lookup(lookup_in(&[(ALLOW_IN_MEMORY_LOG_VARIABLE, "1")])),
            Ok(true)
        );
        assert_eq!(
            allow_in_memory_log_from_lookup(lookup_in(&[(ALLOW_IN_MEMORY_LOG_VARIABLE, "TRUE")])),
            Ok(true)
        );
        assert_eq!(
            allow_in_memory_log_from_lookup(lookup_in(&[(ALLOW_IN_MEMORY_LOG_VARIABLE, "0")])),
            Ok(false)
        );
        assert_eq!(
            allow_in_memory_log_from_lookup(lookup_in(&[(ALLOW_IN_MEMORY_LOG_VARIABLE, "maybe")])),
            Err(ConfigError::InvalidBoolean {
                variable: ALLOW_IN_MEMORY_LOG_VARIABLE,
                value: "maybe".into(),
            })
        );
    }
}
