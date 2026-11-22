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

/// TCP port the server listens on and clients connect to by default.
///
/// Placeholder. Replaced when hosting is set up (`docs/architecture/technical_architecture.md` Section 1).
pub const DEFAULT_SERVER_PORT: u16 = 7777;

/// Largest payload 1 network frame may carry, in bytes.
///
/// Placeholder: 1,048,576 bytes, far above any Milestone 1 message. Replaced when hosting is set up
/// (`docs/architecture/technical_architecture.md` Section 1).
pub const MAX_PAYLOAD_BYTES: u32 = 1 << 20;

/// Largest number of encoded bytes waiting in 1 `FrameOutbox` before the peer counts as stuck.
///
/// Placeholder: 8 times [`MAX_PAYLOAD_BYTES`]. Replaced when hosting is set up
/// (`docs/architecture/technical_architecture.md` Section 1).
pub const MAX_OUTBOX_BYTES: usize = 8 << 20;

/// Longest name, in characters, of a custom narrative item the AI DM creates.
///
/// Placeholder. `docs/design/game_design.md` Section 5 limits custom items but sets no length.
pub const CUSTOM_ITEM_NAME_MAX_CHARACTERS: usize = 48;

/// Longest 1-sentence description, in characters, of a custom narrative item.
///
/// Placeholder. `docs/design/game_design.md` Section 5 limits custom items but sets no length.
pub const CUSTOM_ITEM_DESCRIPTION_MAX_CHARACTERS: usize = 200;

/// Most free-form tags on 1 custom narrative item.
///
/// Placeholder. `docs/design/game_design.md` Section 5 limits custom items but sets no count.
pub const CUSTOM_ITEM_TAG_MAX_COUNT: usize = 8;

/// Longest free-form tag, in characters, on a custom narrative item.
///
/// Placeholder. `docs/design/game_design.md` Section 5 limits custom items but sets no length.
pub const CUSTOM_ITEM_TAG_MAX_CHARACTERS: usize = 32;

/// Heaviest custom narrative item, in thousandths of a pound (50 lb).
///
/// Placeholder. `docs/design/game_design.md` Section 5 limits custom items but sets no weight.
pub const CUSTOM_ITEM_MAX_WEIGHT_THOUSANDTHS_OF_POUND: u32 = 50_000;

/// Most items 1 `award` or `take` effect may move at once.
///
/// Placeholder. Not yet defined in any document.
pub const ITEM_EFFECT_MAX_QUANTITY: u32 = 100;

/// Most catalog entries 1 `inspect` catalog query returns, so 1 vague word cannot fill the
/// context window.
///
/// Placeholder. `docs/architecture/ai_orchestration.md` Section 2 sets no limit.
pub const CATALOG_QUERY_MAX_RESULTS: usize = 10;
