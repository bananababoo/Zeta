//! Headless authoritative Zeta server.
//!
//! For now it only reports the current time values, which proves the workspace builds and
//! links against `zeta_core`. The game loop arrives with the Bevy prototype; until then the
//! server library is exercised by its tests and the `ai_check` binary.

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use zeta_core::settings::CALENDAR_EPOCH_UNIX_MILLISECONDS;
use zeta_core::time::{CalendarDay, CalendarEpoch, DayCycle, UtcTimestamp, world_clock};

fn main() -> ExitCode {
    let Some(now) = current_utc() else {
        eprintln!("system clock is outside the supported range");
        return ExitCode::FAILURE;
    };
    let epoch = match CalendarEpoch::new(UtcTimestamp::from_unix_milliseconds(
        CALENDAR_EPOCH_UNIX_MILLISECONDS,
    )) {
        Ok(epoch) => epoch,
        Err(error) => {
            eprintln!("invalid calendar epoch setting: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "Calendar Day {}, Day Cycle {}, World Clock {}",
        CalendarDay::containing(now, epoch).index(),
        DayCycle::containing(now).index(),
        world_clock(now),
    );
    ExitCode::SUCCESS
}

/// The current UTC time from the system clock, the only clock the server trusts.
fn current_utc() -> Option<UtcTimestamp> {
    let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    let milliseconds = i64::try_from(since_epoch.as_millis()).ok()?;
    Some(UtcTimestamp::from_unix_milliseconds(milliseconds))
}
