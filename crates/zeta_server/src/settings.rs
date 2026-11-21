//! Tunable server values whose final numbers are still open questions.
//!
//! Every value here is a placeholder chosen to make the code run. Each comment names the
//! open question or document that will replace it.

/// Seconds the server waits for 1 AI provider response before treating the call as failed.
///
/// Placeholder. `docs/architecture/ai_orchestration.md` does not define a provider timeout.
pub const AI_REQUEST_TIMEOUT_SECONDS: u64 = 60;
