//! Tunable values whose final numbers are still open questions.
//!
//! Every value here is a placeholder chosen to make the code run. Each comment names the
//! open question that will replace it. Rules that are already decided live as constants
//! next to the code that applies them, not here.

/// Maximum number of entities in a Snapshot, including the acting character.
///
/// Placeholder. Open question: `docs/architecture/ai_orchestration.md`, Snapshot Maximum Count.
pub const SNAPSHOT_MAX_COUNT: usize = 32;

/// Hearing range in grid squares, used for the Snapshot and the Bubble Area.
///
/// Placeholder: 12 squares is 60 feet on the 5-foot grid. Not yet defined in any document.
pub const HEARING_RANGE_SQUARES: u32 = 12;

/// Retries after the first AI DM response fails validation.
///
/// Placeholder. `docs/architecture/ai_orchestration.md` Section 4 leaves the cap undefined.
pub const VALIDATION_RETRY_CAP: u32 = 2;

/// Inspect tool rounds allowed per action.
///
/// Placeholder. `docs/architecture/ai_orchestration.md` Section 2 leaves the cap undefined.
pub const INSPECT_ROUND_CAP: u32 = 3;

/// Start of Calendar Day 0, as milliseconds since the Unix epoch. Must be a Monday at 00:00 UTC.
///
/// Placeholder: 2026-01-05. The real value is the launch week, and the in-world date it maps
/// to depends on the calendar, which Lore has not defined yet.
pub const CALENDAR_EPOCH_UNIX_MILLISECONDS: i64 = 1_767_571_200_000;
