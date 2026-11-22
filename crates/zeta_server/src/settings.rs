//! Tunable server values whose final numbers are still open questions.
//!
//! Every value here is a placeholder chosen to make the code run. Each comment names the
//! open question or document that will replace it.

/// Seconds the server waits for 1 AI provider response before treating the call as failed.
///
/// Placeholder. `docs/architecture/ai_orchestration.md` does not define a provider timeout.
pub const AI_REQUEST_TIMEOUT_SECONDS: u64 = 60;

/// Maximum number of connections in the PostgreSQL connection pool.
///
/// Placeholder. `docs/architecture/persistence.md` Section 1 does not define a pool size.
pub const DATABASE_MAX_CONNECTIONS: u32 = 5;

/// Seconds the server waits when opening a PostgreSQL connection before failing.
///
/// Placeholder. `docs/architecture/persistence.md` Section 1 does not define a connect timeout.
pub const DATABASE_CONNECT_TIMEOUT_SECONDS: u64 = 10;
