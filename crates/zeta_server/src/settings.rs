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

/// Game loop updates per second on the headless server.
///
/// Placeholder. `docs/architecture/technical_architecture.md` does not define a tick rate.
pub const SERVER_TICKS_PER_SECOND: u32 = 60;

/// Most squares 1 movement request may cover, inside and outside Turn Bubbles.
///
/// Placeholder: 6 squares is 30 feet, a common 5e walking speed. `docs/prototypes/realism_prototype.md`
/// Section 2 gives each turn 1 movement but no distance.
pub const MOVEMENT_SQUARES_PER_REQUEST: u32 = 6;

/// Most characters 1 Freeform Action Prompt may hold, counted after trimming.
///
/// Placeholder. `docs/architecture/ai_orchestration.md` Section 5 does not define a length limit.
pub const MAX_PROMPT_CHARACTERS: usize = 500;

/// Map Partition of the prototype test scene, used when storing Event Log records.
///
/// Placeholder until maps are partitioned (`docs/architecture/persistence.md` Section 3).
pub const TEST_SCENE_MAP_PARTITION_ID: u64 = 0;

/// Largest message the networking library should build before splitting component changes,
/// in bytes. About 1 internet packet, the size the library is designed around.
///
/// Placeholder. Replaced when hosting is set up (`docs/architecture/technical_architecture.md` Section 1).
pub const SPLIT_MESSAGE_BYTES: usize = 1200;

/// Bytes read from 1 connection per update at most, so 1 flooding peer cannot stall the server.
///
/// Placeholder. Replaced when hosting is set up (`docs/architecture/technical_architecture.md` Section 1).
pub const MAX_RECEIVE_BYTES_PER_UPDATE: usize = 4 << 20;
