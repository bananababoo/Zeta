//! World Clock, Day Cycle, Dawn, Calendar Day, and Area Clock, all calculated from UTC.
//!
//! Rules: `docs/design/time_model.md` Sections 1, 2, and 5. Calculation approach:
//! `docs/architecture/technical_architecture.md` Section 4. Nothing here is stored or
//! ticked; every value is a pure function of a UTC timestamp, except the round count
//! of an Area Clock.
//!
//! Month and year names are deliberately absent: the calendar's shape depends on Lore.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

// Units up to 1 day fit in u32, which is what `TimeOfDay` stores. The i64 forms below are
// for real-world timestamps; widening u32 to i64 or u64 never loses a value.
const MILLISECONDS_PER_SECOND: u32 = 1_000;
const MILLISECONDS_PER_MINUTE: u32 = 60 * MILLISECONDS_PER_SECOND;
const MILLISECONDS_PER_HOUR: u32 = 60 * MILLISECONDS_PER_MINUTE;
const MILLISECONDS_PER_DAY: u32 = 24 * MILLISECONDS_PER_HOUR;
const REAL_MILLISECONDS_PER_HOUR: i64 = MILLISECONDS_PER_HOUR as i64;
const REAL_MILLISECONDS_PER_DAY: i64 = MILLISECONDS_PER_DAY as i64;
const REAL_MILLISECONDS_PER_WEEK: i64 = 7 * REAL_MILLISECONDS_PER_DAY;

/// The Unix epoch fell on a Thursday, so Monday 00:00 UTC sits 4 days into each Unix week.
const MONDAY_OFFSET_IN_UNIX_WEEK: i64 = 4 * REAL_MILLISECONDS_PER_DAY;

/// Real-world length of 1 Day Cycle.
pub const DAY_CYCLE_REAL_MILLISECONDS: i64 = 4 * REAL_MILLISECONDS_PER_HOUR;

/// In-game time passes this many times faster than real-world time on the World Clock.
pub const WORLD_CLOCK_RATE: i64 = REAL_MILLISECONDS_PER_DAY / DAY_CYCLE_REAL_MILLISECONDS;

/// In-game time of day at Dawn, the start of every Day Cycle.
pub const DAWN: TimeOfDay = TimeOfDay::from_hours_minutes(6, 0);

/// In-game length of 1 completed 5e round on an Area Clock.
pub const ROUND: InGameDuration = InGameDuration::from_seconds(6);

/// A Catch-Up of at least this long counts as a Short Rest.
pub const SHORT_REST: InGameDuration = InGameDuration::from_hours(1);

/// A real-world instant, as milliseconds since the Unix epoch in UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UtcTimestamp(i64);

impl UtcTimestamp {
    /// Creates a timestamp from milliseconds since the Unix epoch.
    pub const fn from_unix_milliseconds(milliseconds: i64) -> Self {
        Self(milliseconds)
    }

    /// Milliseconds since the Unix epoch.
    pub const fn unix_milliseconds(self) -> i64 {
        self.0
    }

    /// This instant moved by a signed number of real-world milliseconds, clamped at the
    /// representable range.
    #[must_use]
    pub const fn saturating_add_milliseconds(self, milliseconds: i64) -> Self {
        Self(self.0.saturating_add(milliseconds))
    }
}

/// An in-game time of day, from 00:00 up to but excluding 24:00.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TimeOfDay(u32);

impl TimeOfDay {
    /// Creates a time of day. Values past 24:00 wrap around midnight.
    pub const fn from_hours_minutes(hours: u32, minutes: u32) -> Self {
        // Reducing each part first keeps the sum far below u32::MAX.
        let hours_part = (hours % 24) * MILLISECONDS_PER_HOUR;
        let minutes_part = (minutes % (24 * 60)) * MILLISECONDS_PER_MINUTE;
        Self((hours_part + minutes_part) % MILLISECONDS_PER_DAY)
    }

    /// Wraps any count of in-game milliseconds into a time of day.
    fn wrapping_from_milliseconds(milliseconds: i64) -> Self {
        let wrapped = milliseconds.rem_euclid(REAL_MILLISECONDS_PER_DAY);
        Self(u32::try_from(wrapped).expect("a value below 1 day in milliseconds fits in u32"))
    }

    /// In-game milliseconds since midnight.
    pub const fn milliseconds_since_midnight(self) -> u32 {
        self.0
    }

    /// Hour of the day, from 0 to 23.
    pub const fn hour(self) -> u32 {
        self.0 / MILLISECONDS_PER_HOUR
    }

    /// Minute of the hour, from 0 to 59.
    pub const fn minute(self) -> u32 {
        self.0 / MILLISECONDS_PER_MINUTE % 60
    }

    /// Second of the minute, from 0 to 59.
    pub const fn second(self) -> u32 {
        self.0 / MILLISECONDS_PER_SECOND % 60
    }
}

impl fmt::Display for TimeOfDay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:02}:{:02}", self.hour(), self.minute())
    }
}

/// A non-negative span of in-game time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InGameDuration(u64);

impl InGameDuration {
    /// A span of whole in-game seconds, clamped at the representable range.
    pub const fn from_seconds(seconds: u64) -> Self {
        Self(seconds.saturating_mul(MILLISECONDS_PER_SECOND as u64))
    }

    /// A span of whole in-game hours, clamped at the representable range.
    pub const fn from_hours(hours: u64) -> Self {
        Self(hours.saturating_mul(MILLISECONDS_PER_HOUR as u64))
    }

    /// Length in in-game milliseconds.
    pub const fn milliseconds(self) -> u64 {
        self.0
    }
}

/// The World Clock reading at a real-world instant.
pub fn world_clock(now: UtcTimestamp) -> TimeOfDay {
    let into_cycle = now.0.rem_euclid(DAY_CYCLE_REAL_MILLISECONDS);
    TimeOfDay::wrapping_from_milliseconds(i64::from(DAWN.0) + into_cycle * WORLD_CLOCK_RATE)
}

/// 1 Day Cycle, numbered from the cycle that began at the Unix epoch.
///
/// Day Cycles begin at 00:00, 04:00, 08:00, 12:00, 16:00, and 20:00 UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DayCycle(i64);

impl DayCycle {
    /// The Day Cycle running at a real-world instant.
    pub const fn containing(now: UtcTimestamp) -> Self {
        Self(now.0.div_euclid(DAY_CYCLE_REAL_MILLISECONDS))
    }

    /// Number of Day Cycles since the Unix epoch.
    pub const fn index(self) -> i64 {
        self.0
    }

    /// The real-world instant of this Day Cycle's Dawn.
    pub const fn dawn(self) -> UtcTimestamp {
        UtcTimestamp(self.0.saturating_mul(DAY_CYCLE_REAL_MILLISECONDS))
    }

    /// The Day Cycle after this one.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Errors from building time values out of untrusted input.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TimeError {
    /// Calendar Days roll over on Mondays at 00:00 UTC, so the epoch must be one.
    #[error("calendar epoch {0:?} is not a Monday at 00:00 UTC")]
    EpochNotMondayMidnight(UtcTimestamp),
}

/// The Monday at 00:00 UTC on which Calendar Day 0 begins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "UtcTimestamp", into = "UtcTimestamp")]
pub struct CalendarEpoch(UtcTimestamp);

impl CalendarEpoch {
    /// Creates an epoch.
    ///
    /// # Errors
    ///
    /// Returns [`TimeError::EpochNotMondayMidnight`] unless `start` is a Monday at 00:00 UTC.
    pub const fn new(start: UtcTimestamp) -> Result<Self, TimeError> {
        if start.0.rem_euclid(REAL_MILLISECONDS_PER_WEEK) == MONDAY_OFFSET_IN_UNIX_WEEK {
            Ok(Self(start))
        } else {
            Err(TimeError::EpochNotMondayMidnight(start))
        }
    }

    /// The instant Calendar Day 0 begins.
    pub const fn start(self) -> UtcTimestamp {
        self.0
    }
}

impl TryFrom<UtcTimestamp> for CalendarEpoch {
    type Error = TimeError;

    fn try_from(start: UtcTimestamp) -> Result<Self, Self::Error> {
        Self::new(start)
    }
}

impl From<CalendarEpoch> for UtcTimestamp {
    fn from(epoch: CalendarEpoch) -> Self {
        epoch.0
    }
}

/// The in-game date, which advances by 1 day every real-world week.
///
/// Numbered from the epoch; days before it are negative. Mapping the number to an in-world
/// month and year belongs to the calendar, which is not defined yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CalendarDay(i64);

impl CalendarDay {
    /// The Calendar Day at a real-world instant.
    pub const fn containing(now: UtcTimestamp, epoch: CalendarEpoch) -> Self {
        Self(
            now.0
                .saturating_sub(epoch.0.0)
                .div_euclid(REAL_MILLISECONDS_PER_WEEK),
        )
    }

    /// Number of Calendar Days since the epoch.
    pub const fn index(self) -> i64 {
        self.0
    }

    /// The real-world instant this Calendar Day begins.
    pub const fn start(self, epoch: CalendarEpoch) -> UtcTimestamp {
        UtcTimestamp(
            epoch
                .0
                .0
                .saturating_add(self.0.saturating_mul(REAL_MILLISECONDS_PER_WEEK)),
        )
    }
}

/// The time of day inside a Bubble Area while a Turn Bubble is active.
///
/// Stored as the instant the Turn Bubble started plus the completed round count. Each
/// round adds 6 in-game seconds, which equals 1 real-world second at the World Clock rate,
/// so the Area Clock always reads as the World Clock at some earlier instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AreaClock {
    started_at: UtcTimestamp,
    completed_rounds: u32,
}

impl AreaClock {
    /// An Area Clock for a Turn Bubble starting at `started_at`.
    pub const fn start(started_at: UtcTimestamp) -> Self {
        Self {
            started_at,
            completed_rounds: 0,
        }
    }

    /// Records 1 completed 5e round.
    pub const fn complete_round(&mut self) {
        self.completed_rounds = self.completed_rounds.saturating_add(1);
    }

    /// When the Turn Bubble started.
    pub const fn started_at(self) -> UtcTimestamp {
        self.started_at
    }

    /// Number of completed 5e rounds.
    pub const fn completed_rounds(self) -> u32 {
        self.completed_rounds
    }

    /// The instant whose World Clock reading equals this Area Clock.
    ///
    /// Clamped at `now`, because an Area Clock never runs ahead of the World Clock.
    fn equivalent_instant(self, now: UtcTimestamp) -> UtcTimestamp {
        let real_milliseconds_per_round = ROUND.0.cast_signed() / WORLD_CLOCK_RATE;
        let elapsed = i64::from(self.completed_rounds) * real_milliseconds_per_round;
        let instant = self.started_at.saturating_add_milliseconds(elapsed);
        if instant < now { instant } else { now }
    }

    /// The Area Clock reading at a real-world instant.
    pub fn time_of_day(self, now: UtcTimestamp) -> TimeOfDay {
        world_clock(self.equivalent_instant(now))
    }

    /// How far this Area Clock trails the World Clock, which is the length of Catch-Up if
    /// the Turn Bubble ended now.
    pub fn behind_world_clock(self, now: UtcTimestamp) -> InGameDuration {
        let behind_real = now.0.saturating_sub(self.equivalent_instant(now).0);
        let behind_in_game = behind_real.saturating_mul(WORLD_CLOCK_RATE);
        InGameDuration(u64::try_from(behind_in_game).unwrap_or(0))
    }

    /// Whether Catch-Up at `now` would count as a Short Rest for the participants.
    pub fn catch_up_is_short_rest(self, now: UtcTimestamp) -> bool {
        self.behind_world_clock(now) >= SHORT_REST
    }

    /// Of 2 overlapping Bubble Areas' clocks, the one showing the earlier time, which they
    /// then share.
    #[must_use]
    pub fn earliest(self, other: Self, now: UtcTimestamp) -> Self {
        if other.equivalent_instant(now) < self.equivalent_instant(now) {
            other
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::CALENDAR_EPOCH_UNIX_MILLISECONDS;

    // Signed forms of the unit sizes, for building timestamps in tests.
    const MILLISECONDS_PER_SECOND: i64 = super::MILLISECONDS_PER_SECOND as i64;
    const MILLISECONDS_PER_MINUTE: i64 = super::MILLISECONDS_PER_MINUTE as i64;
    const MILLISECONDS_PER_HOUR: i64 = REAL_MILLISECONDS_PER_HOUR;
    const MILLISECONDS_PER_DAY: i64 = REAL_MILLISECONDS_PER_DAY;
    const MILLISECONDS_PER_WEEK: i64 = REAL_MILLISECONDS_PER_WEEK;

    fn utc_hours(hours: i64) -> UtcTimestamp {
        UtcTimestamp(hours * MILLISECONDS_PER_HOUR)
    }

    fn epoch() -> CalendarEpoch {
        CalendarEpoch::new(UtcTimestamp(CALENDAR_EPOCH_UNIX_MILLISECONDS))
            .expect("the settings epoch is a Monday")
    }

    #[test]
    fn world_clock_reads_dawn_at_every_day_cycle_boundary() {
        for hour in [0, 4, 8, 12, 16, 20, 24, -4] {
            assert_eq!(world_clock(utc_hours(hour)), DAWN, "UTC hour {hour}");
        }
    }

    #[test]
    fn one_real_minute_is_six_in_game_minutes() {
        let one_minute_after_dawn = UtcTimestamp(MILLISECONDS_PER_MINUTE);
        assert_eq!(
            world_clock(one_minute_after_dawn),
            TimeOfDay::from_hours_minutes(6, 6)
        );
    }

    #[test]
    fn world_clock_wraps_past_midnight_within_a_cycle() {
        // 3 real hours after Dawn is 18 in-game hours after 06:00.
        assert_eq!(
            world_clock(utc_hours(3)),
            TimeOfDay::from_hours_minutes(0, 0)
        );
        assert_eq!(world_clock(utc_hours(3)).to_string(), "00:00");
    }

    #[test]
    fn day_cycle_dawn_round_trips() {
        let now = UtcTimestamp(5 * MILLISECONDS_PER_HOUR + 17);
        let cycle = DayCycle::containing(now);
        assert_eq!(cycle.dawn(), utc_hours(4));
        assert_eq!(cycle.next().dawn(), utc_hours(8));
    }

    #[test]
    fn day_cycles_before_the_unix_epoch_round_down() {
        assert_eq!(DayCycle::containing(UtcTimestamp(-1)).index(), -1);
    }

    #[test]
    fn settings_epoch_is_a_monday_at_midnight() {
        assert!(CalendarEpoch::new(UtcTimestamp(CALENDAR_EPOCH_UNIX_MILLISECONDS)).is_ok());
    }

    #[test]
    fn epoch_on_a_non_monday_is_rejected() {
        let tuesday = UtcTimestamp(CALENDAR_EPOCH_UNIX_MILLISECONDS + MILLISECONDS_PER_DAY);
        assert_eq!(
            CalendarEpoch::new(tuesday),
            Err(TimeError::EpochNotMondayMidnight(tuesday))
        );
    }

    #[test]
    fn calendar_day_rolls_over_exactly_at_monday_midnight() {
        let epoch = epoch();
        let next_monday = epoch
            .start()
            .saturating_add_milliseconds(MILLISECONDS_PER_WEEK);
        assert_eq!(
            CalendarDay::containing(next_monday.saturating_add_milliseconds(-1), epoch).index(),
            0
        );
        assert_eq!(CalendarDay::containing(next_monday, epoch).index(), 1);
        assert_eq!(CalendarDay(1).start(epoch), next_monday);
    }

    #[test]
    fn calendar_days_before_the_epoch_are_negative() {
        let epoch = epoch();
        let before = epoch.start().saturating_add_milliseconds(-1);
        assert_eq!(CalendarDay::containing(before, epoch).index(), -1);
    }

    #[test]
    fn area_clock_advances_six_seconds_per_round() {
        let start = utc_hours(0);
        let mut clock = AreaClock::start(start);
        clock.complete_round();
        clock.complete_round();
        let much_later = utc_hours(1);
        assert_eq!(
            clock.time_of_day(much_later).milliseconds_since_midnight(),
            {
                let twelve_seconds_after_dawn = i64::from(DAWN.0) + 12 * MILLISECONDS_PER_SECOND;
                u32::try_from(twelve_seconds_after_dawn).expect("fits")
            }
        );
    }

    #[test]
    fn area_clock_never_runs_ahead_of_the_world_clock() {
        let start = utc_hours(0);
        let mut clock = AreaClock::start(start);
        for _ in 0..100 {
            clock.complete_round();
        }
        let ten_seconds_in = start.saturating_add_milliseconds(10 * MILLISECONDS_PER_SECOND);
        assert_eq!(
            clock.time_of_day(ten_seconds_in),
            world_clock(ten_seconds_in)
        );
    }

    #[test]
    fn long_catch_up_counts_as_a_short_rest() {
        let clock = AreaClock::start(utc_hours(0));
        // 10 real minutes is 60 in-game minutes.
        let ten_minutes = UtcTimestamp(10 * MILLISECONDS_PER_MINUTE);
        assert_eq!(clock.behind_world_clock(ten_minutes), SHORT_REST);
        assert!(clock.catch_up_is_short_rest(ten_minutes));
        assert!(!clock.catch_up_is_short_rest(ten_minutes.saturating_add_milliseconds(-1)));
    }

    #[test]
    fn overlapping_area_clocks_share_the_earliest_time() {
        let now = utc_hours(1);
        let early = AreaClock::start(utc_hours(0));
        let late = AreaClock::start(UtcTimestamp(30 * MILLISECONDS_PER_MINUTE));
        assert_eq!(early.earliest(late, now), early);
        assert_eq!(late.earliest(early, now), early);
    }
}
