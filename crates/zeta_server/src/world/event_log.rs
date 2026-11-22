//! Writes Event Log records for applied Freeform Action Prompts: to PostgreSQL when a
//! database is configured, otherwise to an in-memory Event Log.
//!
//! Rules: `docs/architecture/persistence.md` Sections 2 and 3. Records keep the Canonical
//! Order: 1 writer task appends them in the order the world applied them, and recorded times
//! never run backward.

use std::time::{SystemTime, UNIX_EPOCH};

use bevy_ecs::prelude::Resource;
use tokio::runtime::Handle;
use tokio::sync::mpsc::{self, UnboundedSender};
use zeta_core::event_log::{AppliedChange, EventCause, EventLog, MapPartitionId};
use zeta_core::grid::GridPos;
use zeta_core::time::UtcTimestamp;

use crate::db::EventLogStore;

/// 1 record on its way to the database writer task.
#[derive(Debug)]
struct PendingRecord {
    position: GridPos,
    recorded_at: UtcTimestamp,
    cause: EventCause,
    change: AppliedChange,
}

/// Where Event Log records go.
#[derive(Debug)]
enum Destination {
    /// Kept in memory and lost when the server stops.
    Memory(EventLog),
    /// Sent to a task that appends them to PostgreSQL in order.
    Database(UnboundedSender<PendingRecord>),
}

/// Records every applied change with its cause.
#[derive(Resource, Debug)]
pub struct EventLogWriter {
    destination: Destination,
    latest: Option<UtcTimestamp>,
}

impl Default for EventLogWriter {
    fn default() -> Self {
        Self::memory()
    }
}

impl EventLogWriter {
    /// Keeps records in memory only, for tests and for running without a database.
    pub fn memory() -> Self {
        Self {
            destination: Destination::Memory(EventLog::new()),
            latest: None,
        }
    }

    /// Appends records to `store` from a task on `runtime`, all in Map Partition
    /// `map_partition`. A failed append is reported on standard error and the server keeps
    /// running.
    pub fn database(store: EventLogStore, runtime: &Handle, map_partition: MapPartitionId) -> Self {
        let (sender, mut receiver) = mpsc::unbounded_channel::<PendingRecord>();
        runtime.spawn(async move {
            while let Some(record) = receiver.recv().await {
                if let Err(error) = store
                    .append(
                        map_partition,
                        record.position,
                        record.recorded_at,
                        &record.cause,
                        &record.change,
                    )
                    .await
                {
                    eprintln!("failed to store an Event Log record: {error}");
                }
            }
        });
        Self {
            destination: Destination::Database(sender),
            latest: None,
        }
    }

    /// The in-memory Event Log, when records are kept in memory.
    pub fn memory_log(&self) -> Option<&EventLog> {
        match &self.destination {
            Destination::Memory(log) => Some(log),
            Destination::Database(_) => None,
        }
    }

    /// Records 1 applied change that happened at `position`, stamped with the current time.
    pub(crate) fn record(&mut self, position: GridPos, cause: EventCause, change: AppliedChange) {
        let now = current_utc();
        let recorded_at = match self.latest {
            Some(latest) if now < latest => latest,
            _ => now,
        };
        self.latest = Some(recorded_at);
        match &mut self.destination {
            Destination::Memory(log) => {
                if let Err(error) = log.append(recorded_at, cause, change) {
                    eprintln!("failed to record an Event Log entry: {error}");
                }
            }
            Destination::Database(sender) => {
                let pending = PendingRecord {
                    position,
                    recorded_at,
                    cause,
                    change,
                };
                if sender.send(pending).is_err() {
                    eprintln!("the Event Log writer task stopped; a record was lost");
                }
            }
        }
    }
}

/// The current UTC time from the system clock, the only clock the server trusts. A clock
/// before 1970 counts as 1970, which the order clamp in [`EventLogWriter::record`] absorbs.
fn current_utc() -> UtcTimestamp {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since_epoch| i64::try_from(since_epoch.as_millis()).ok())
        .unwrap_or(0);
    UtcTimestamp::from_unix_milliseconds(milliseconds)
}

#[cfg(test)]
mod tests {
    use zeta_core::entity::EntityId;
    use zeta_core::event_log::FreeformActionCause;
    use zeta_core::resolution::{AiResolution, Branch};
    use zeta_core::snapshot::{PerceptionSettings, Snapshot, SnapshotEntity};

    use super::*;

    fn cause() -> EventCause {
        let actor = SnapshotEntity {
            world_id: EntityId(1),
            name: "actor".into(),
            position: GridPos::new(0, 0),
            tags: vec![],
            passable: false,
            description: "A test actor.".into(),
            player_character: true,
        };
        EventCause::FreeformAction(FreeformActionCause {
            actor: EntityId(1),
            player_text: "I wave.".into(),
            snapshot: Snapshot::build(actor, [], |_| false, PerceptionSettings::default()),
            exchanges: vec![],
            roll: None,
        })
    }

    fn change() -> AppliedChange {
        AppliedChange::resolved(
            AiResolution {
                ok: true,
                chk: None,
                fx: vec![],
                fail: vec![],
            },
            Branch::Success,
            vec![],
        )
    }

    #[test]
    fn records_never_go_back_in_time() {
        let mut writer = EventLogWriter::memory();
        writer.latest = Some(UtcTimestamp::from_unix_milliseconds(i64::MAX));
        writer.record(GridPos::new(0, 0), cause(), change());
        writer.record(GridPos::new(0, 0), cause(), change());
        let log = writer.memory_log().expect("memory log");
        assert_eq!(log.records().len(), 2);
        assert_eq!(
            log.records()[1].recorded_at,
            UtcTimestamp::from_unix_milliseconds(i64::MAX)
        );
    }
}
