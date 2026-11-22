//! PostgreSQL persistence and `PostGIS` spatial queries for the Event Log.
//!
//! Rules: `docs/architecture/persistence.md` Sections 1-3.

use std::time::Duration;

use serde_json::Value;
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgRow};
use sqlx::{PgPool, Row, SqlSafeStr};
use thiserror::Error;
use zeta_core::event_log::{
    AppliedChange, EventCause, EventLogError, EventRecord, EventSequence, MapPartitionId,
};
use zeta_core::grid::GridPos;
use zeta_core::time::UtcTimestamp;

use crate::config::DatabaseConfig;
use crate::settings::{DATABASE_CONNECT_TIMEOUT_SECONDS, DATABASE_MAX_CONNECTIONS};

const EVENT_LOG_ADVISORY_LOCK_KEY: i64 = 1;

/// Errors from connecting to PostgreSQL, applying migrations, or reading and writing the
/// Event Log.
#[derive(Debug, Error)]
pub enum DatabaseError {
    /// A PostgreSQL query or connection failed.
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// Applying a database migration failed.
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// The record would break Canonical Order in the Event Log.
    #[error(transparent)]
    OutOfOrder(#[from] EventLogError),
    /// A stored row or input value could not be converted into valid domain values.
    #[error("corrupt Event Log record: {reason}")]
    CorruptRecord {
        /// Why the record is invalid.
        reason: String,
    },
}

/// 1 Event Log record together with its Map Partition and grid coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredEvent {
    /// Map Partition where the event occurred.
    pub map_partition: MapPartitionId,
    /// Grid square where the event occurred.
    pub position: GridPos,
    /// The stored Event Log record.
    pub record: EventRecord,
}

/// PostgreSQL-backed append-only store for the Event Log.
#[derive(Clone, Debug)]
pub struct EventLogStore {
    pool: PgPool,
}

impl EventLogStore {
    /// Opens a PostgreSQL connection pool for `config`, applies migrations, and returns the
    /// ready store.
    ///
    /// # Errors
    /// Returns [`DatabaseError::Sqlx`] if connecting fails or [`DatabaseError::Migrate`] if
    /// applying migrations fails.
    pub async fn connect_and_migrate(config: &DatabaseConfig) -> Result<Self, DatabaseError> {
        let pool = default_pool_options()
            .connect_with(connect_options(config))
            .await?;
        event_log_migrator().run(&pool).await?;
        Ok(Self { pool })
    }

    /// Queries the `PostGIS` version installed in the connected database.
    ///
    /// # Errors
    /// Returns [`DatabaseError::Sqlx`] if the query fails.
    pub async fn postgis_version(&self) -> Result<String, DatabaseError> {
        let row = sqlx::query("SELECT postgis_version() AS version")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("version")?)
    }

    /// Appends 1 event to the Event Log and returns its assigned sequence number.
    ///
    /// Acquires a transaction-scoped PostgreSQL advisory lock so concurrent writers cannot
    /// race on sequence `0` or violate Canonical Order.
    ///
    /// # Errors
    /// Returns [`DatabaseError::OutOfOrder`] if `recorded_at` is earlier than the latest
    /// stored record, [`DatabaseError::CorruptRecord`] if a value exceeds PostgreSQL integer
    /// bounds or fails serialization, or [`DatabaseError::Sqlx`] if a database query fails.
    pub async fn append(
        &self,
        map_partition: MapPartitionId,
        position: GridPos,
        recorded_at: UtcTimestamp,
        cause: &EventCause,
        change: &AppliedChange,
    ) -> Result<EventSequence, DatabaseError> {
        let map_partition_id = u64_to_non_negative_i64(map_partition.0, "map_partition_id")?;
        let grid_x = i64::from(position.x);
        let grid_y = i64::from(position.y);
        let cause_json = serialize_json(cause, "cause")?;
        let change_json = serialize_json(change, "change")?;

        let mut transaction = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(EVENT_LOG_ADVISORY_LOCK_KEY)
            .execute(&mut *transaction)
            .await?;

        let latest = sqlx::query(
            "SELECT sequence, recorded_at_unix_ms FROM event_log ORDER BY sequence DESC LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await?;

        let (next_sequence, stored_sequence) = match latest {
            Some(row) => {
                let latest_sequence_raw: i64 = row.try_get("sequence")?;
                let latest_sequence = i64_to_u64(latest_sequence_raw, "sequence")?;
                let latest_recorded_at =
                    UtcTimestamp::from_unix_milliseconds(row.try_get("recorded_at_unix_ms")?);
                if recorded_at < latest_recorded_at {
                    return Err(DatabaseError::OutOfOrder(EventLogError::OutOfOrder {
                        attempted: recorded_at,
                        latest: latest_recorded_at,
                    }));
                }
                let incremented =
                    latest_sequence
                        .checked_add(1)
                        .ok_or_else(|| DatabaseError::CorruptRecord {
                            reason: "event sequence overflowed u64".to_owned(),
                        })?;
                let column_value = u64_to_non_negative_i64(incremented, "sequence")?;
                (EventSequence(incremented), column_value)
            }
            None => (EventSequence(0), 0_i64),
        };

        sqlx::query(
            "INSERT INTO event_log (
                sequence,
                recorded_at_unix_ms,
                map_partition_id,
                grid_x,
                grid_y,
                cause,
                change
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(stored_sequence)
        .bind(recorded_at.unix_milliseconds())
        .bind(map_partition_id)
        .bind(grid_x)
        .bind(grid_y)
        .bind(cause_json)
        .bind(change_json)
        .execute(&mut *transaction)
        .await?;

        transaction.commit().await?;
        Ok(next_sequence)
    }

    /// Returns all stored events from `sequence` onward, oldest first.
    ///
    /// # Errors
    /// Returns [`DatabaseError::Sqlx`] if the query fails or [`DatabaseError::CorruptRecord`]
    /// if a stored row cannot be decoded.
    pub async fn since(&self, sequence: EventSequence) -> Result<Vec<StoredEvent>, DatabaseError> {
        let Ok(min_sequence) = i64::try_from(sequence.0) else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT sequence, recorded_at_unix_ms, map_partition_id, grid_x, grid_y, cause, change
             FROM event_log
             WHERE sequence >= $1
             ORDER BY sequence ASC",
        )
        .bind(min_sequence)
        .fetch_all(&self.pool)
        .await?;
        decode_rows(&rows)
    }

    /// Returns all stored events in `map_partition` within `radius_squares` Euclidean
    /// distance of `center`, oldest first.
    ///
    /// # Errors
    /// Returns [`DatabaseError::Sqlx`] if the query fails or [`DatabaseError::CorruptRecord`]
    /// if a stored row cannot be decoded.
    pub async fn within_euclidean_radius(
        &self,
        map_partition: MapPartitionId,
        center: GridPos,
        radius_squares: f64,
    ) -> Result<Vec<StoredEvent>, DatabaseError> {
        if radius_squares.is_nan() || radius_squares < 0.0 {
            return Ok(Vec::new());
        }
        let Ok(partition_id) = i64::try_from(map_partition.0) else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT sequence, recorded_at_unix_ms, map_partition_id, grid_x, grid_y, cause, change
             FROM event_log
             WHERE map_partition_id = $1
               AND ST_DWithin(location, ST_MakePoint($2, $3), $4)
             ORDER BY sequence ASC",
        )
        .bind(partition_id)
        .bind(f64::from(center.x))
        .bind(f64::from(center.y))
        .bind(radius_squares)
        .fetch_all(&self.pool)
        .await?;
        decode_rows(&rows)
    }

    /// Returns all stored events in `map_partition` within `radius_squares` on the 5-foot
    /// grid of `center`, oldest first.
    ///
    /// # Errors
    /// Returns [`DatabaseError::Sqlx`] if the query fails or [`DatabaseError::CorruptRecord`]
    /// if a stored row cannot be decoded.
    pub async fn within_grid_radius(
        &self,
        map_partition: MapPartitionId,
        center: GridPos,
        radius_squares: u32,
    ) -> Result<Vec<StoredEvent>, DatabaseError> {
        let Ok(partition_id) = i64::try_from(map_partition.0) else {
            return Ok(Vec::new());
        };
        let center_x = f64::from(center.x);
        let center_y = f64::from(center.y);
        let radius = f64::from(radius_squares);
        let rows = sqlx::query(
            "SELECT sequence, recorded_at_unix_ms, map_partition_id, grid_x, grid_y, cause, change
             FROM event_log
             WHERE map_partition_id = $1
               AND location && ST_MakeEnvelope($2, $3, $4, $5, 0)
             ORDER BY sequence ASC",
        )
        .bind(partition_id)
        .bind(center_x - radius)
        .bind(center_y - radius)
        .bind(center_x + radius)
        .bind(center_y + radius)
        .fetch_all(&self.pool)
        .await?;
        let mut events = decode_rows(&rows)?;
        // PostGIS GIST `&&` uses single-precision bounding boxes, so filter exact grid
        // distance here to prevent false positives at large coordinates.
        events.retain(|event| center.grid_distance(event.position) <= radius_squares);
        Ok(events)
    }
}

fn event_log_migrator() -> Migrator {
    Migrator::with_migrations(vec![Migration::new(
        1,
        "event log".into(),
        MigrationType::Simple,
        include_str!("../migrations/0001_event_log.sql").into_sql_str(),
        false,
    )])
}

fn connect_options(config: &DatabaseConfig) -> PgConnectOptions {
    PgConnectOptions::new()
        .host(&config.host)
        .port(config.port)
        .username(&config.user)
        .password(config.password.expose())
        .database(&config.database)
}

fn default_pool_options() -> PgPoolOptions {
    PgPoolOptions::new()
        .max_connections(DATABASE_MAX_CONNECTIONS)
        .acquire_timeout(Duration::from_secs(DATABASE_CONNECT_TIMEOUT_SECONDS))
}

fn u64_to_non_negative_i64(value: u64, field: &str) -> Result<i64, DatabaseError> {
    i64::try_from(value).map_err(|_| DatabaseError::CorruptRecord {
        reason: format!("{field} value {value} exceeds PostgreSQL BIGINT range"),
    })
}

fn i64_to_u64(value: i64, field: &str) -> Result<u64, DatabaseError> {
    u64::try_from(value).map_err(|_| DatabaseError::CorruptRecord {
        reason: format!("{field} value {value} is negative"),
    })
}

fn i64_to_u32(value: i64, field: &str) -> Result<u32, DatabaseError> {
    u32::try_from(value).map_err(|_| DatabaseError::CorruptRecord {
        reason: format!("{field} value {value} is outside u32 range"),
    })
}

fn serialize_json(value: &impl serde::Serialize, field: &str) -> Result<Value, DatabaseError> {
    serde_json::to_value(value).map_err(|error| DatabaseError::CorruptRecord {
        reason: format!("failed to serialize {field}: {error}"),
    })
}

fn decode_rows(rows: &[PgRow]) -> Result<Vec<StoredEvent>, DatabaseError> {
    rows.iter().map(decode_row).collect()
}

fn decode_row(row: &PgRow) -> Result<StoredEvent, DatabaseError> {
    let sequence_raw: i64 = row.try_get("sequence")?;
    let recorded_at_unix_ms: i64 = row.try_get("recorded_at_unix_ms")?;
    let map_partition_raw: i64 = row.try_get("map_partition_id")?;
    let grid_column: i64 = row.try_get("grid_x")?;
    let grid_row: i64 = row.try_get("grid_y")?;
    let cause_json: Value = row.try_get("cause")?;
    let change_json: Value = row.try_get("change")?;
    decode_stored_event(
        sequence_raw,
        recorded_at_unix_ms,
        map_partition_raw,
        grid_column,
        grid_row,
        cause_json,
        change_json,
    )
}

fn decode_stored_event(
    sequence_raw: i64,
    recorded_at_unix_ms: i64,
    map_partition_raw: i64,
    grid_column: i64,
    grid_row: i64,
    cause_json: Value,
    change_json: Value,
) -> Result<StoredEvent, DatabaseError> {
    let sequence = EventSequence(i64_to_u64(sequence_raw, "sequence")?);
    let map_partition = MapPartitionId(i64_to_u64(map_partition_raw, "map_partition_id")?);
    let position = GridPos::new(
        i64_to_u32(grid_column, "grid_x")?,
        i64_to_u32(grid_row, "grid_y")?,
    );
    let cause: EventCause =
        serde_json::from_value(cause_json).map_err(|error| DatabaseError::CorruptRecord {
            reason: format!("invalid cause JSON: {error}"),
        })?;
    let change: AppliedChange =
        serde_json::from_value(change_json).map_err(|error| DatabaseError::CorruptRecord {
            reason: format!("invalid change JSON: {error}"),
        })?;
    Ok(StoredEvent {
        map_partition,
        position,
        record: EventRecord {
            sequence,
            recorded_at: UtcTimestamp::from_unix_milliseconds(recorded_at_unix_ms),
            cause,
            change,
        },
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;
    use zeta_core::entity::EntityId;
    use zeta_core::event_log::FreeformActionCause;
    use zeta_core::resolution::{AiResolution, Branch};
    use zeta_core::snapshot::{PerceptionSettings, Snapshot, SnapshotEntity};

    use super::*;

    static NEXT_TEST_SCHEMA_ID: AtomicU64 = AtomicU64::new(0);

    fn sample_cause(actor_id: u64, text: &str) -> EventCause {
        let actor = SnapshotEntity {
            world_id: EntityId(actor_id),
            name: "Actor".to_owned(),
            position: GridPos::new(0, 0),
            tags: vec![],
            passable: true,
            description: "An adventurer.".to_owned(),
            player_character: true,
        };
        EventCause::FreeformAction(FreeformActionCause {
            actor: EntityId(actor_id),
            player_text: text.to_owned(),
            snapshot: Snapshot::build(actor, [], |_| false, PerceptionSettings::default()),
            exchanges: vec![],
            roll: None,
        })
    }

    fn sample_change() -> AppliedChange {
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

    struct IsolatedTestSchema {
        schema_name: String,
        admin_pool: PgPool,
        store: EventLogStore,
    }

    impl IsolatedTestSchema {
        async fn create() -> Self {
            let config = DatabaseConfig::from_environment().expect("database config in .env");
            let admin_pool = default_pool_options()
                .connect_with(connect_options(&config))
                .await
                .expect("connect admin pool");

            let suffix = NEXT_TEST_SCHEMA_ID.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos());
            let schema_name = format!("zeta_test_{}_{}_{}", std::process::id(), nanos, suffix);

            sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema_name}")))
                .execute(&admin_pool)
                .await
                .expect("create temporary test schema");

            let schema_for_hook = schema_name.clone();
            let pool = default_pool_options()
                .after_connect(move |connection, _meta| {
                    let schema = schema_for_hook.clone();
                    Box::pin(async move {
                        sqlx::query(sqlx::AssertSqlSafe(format!(
                            "SET search_path TO {schema}, public"
                        )))
                        .execute(connection)
                        .await?;
                        Ok(())
                    })
                })
                .connect_with(connect_options(&config))
                .await
                .expect("connect isolated test pool");

            event_log_migrator()
                .run(&pool)
                .await
                .expect("apply event log migration");

            Self {
                schema_name,
                admin_pool,
                store: EventLogStore { pool },
            }
        }

        async fn drop_schema(self) {
            self.store.pool.close().await;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP SCHEMA IF EXISTS {} CASCADE",
                self.schema_name
            )))
            .execute(&self.admin_pool)
            .await
            .expect("drop temporary test schema");
            self.admin_pool.close().await;
        }
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime for test")
            .block_on(future)
    }

    #[test]
    fn checked_integer_conversions_reject_negative_and_overflow_values() {
        assert_eq!(u64_to_non_negative_i64(42, "sequence").expect("fits"), 42);
        assert!(matches!(
            u64_to_non_negative_i64(u64::MAX, "sequence"),
            Err(DatabaseError::CorruptRecord { .. })
        ));
        assert_eq!(i64_to_u64(42, "sequence").expect("fits"), 42);
        assert!(matches!(
            i64_to_u64(-1, "sequence"),
            Err(DatabaseError::CorruptRecord { .. })
        ));
        assert_eq!(i64_to_u32(7, "grid_x").expect("fits"), 7);
        assert!(matches!(
            i64_to_u32(-1, "grid_x"),
            Err(DatabaseError::CorruptRecord { .. })
        ));
        assert!(matches!(
            i64_to_u32(i64::from(u32::MAX) + 1, "grid_x"),
            Err(DatabaseError::CorruptRecord { .. })
        ));
    }

    #[test]
    fn stored_event_round_trips_through_json_values() {
        let cause = sample_cause(7, "I examine the gate.");
        let change = sample_change();
        let cause_json = serialize_json(&cause, "cause").expect("serializes cause");
        let change_json = serialize_json(&change, "change").expect("serializes change");

        let decoded = decode_stored_event(3, 1_700_000_000_000, 9, 12, 34, cause_json, change_json)
            .expect("decodes stored event");

        assert_eq!(decoded.map_partition, MapPartitionId(9));
        assert_eq!(decoded.position, GridPos::new(12, 34));
        assert_eq!(decoded.record.sequence, EventSequence(3));
        assert_eq!(
            decoded.record.recorded_at,
            UtcTimestamp::from_unix_milliseconds(1_700_000_000_000)
        );
        assert_eq!(decoded.record.cause, cause);
        assert_eq!(decoded.record.change, change);
    }

    #[test]
    fn corrupt_json_in_stored_row_returns_corrupt_record_error() {
        let change_json = serialize_json(&sample_change(), "change").expect("serializes change");
        let error = decode_stored_event(
            0,
            100,
            1,
            2,
            3,
            json!({"kind": "unknown_cause"}),
            change_json,
        )
        .expect_err("corrupt cause JSON");
        assert!(matches!(error, DatabaseError::CorruptRecord { .. }));
    }

    #[test]
    #[ignore = "requires local PostgreSQL from tools/dev_up.ps1"]
    fn append_assigns_monotonic_sequences_rejects_out_of_order_and_queries_since() {
        block_on(async {
            let env = IsolatedTestSchema::create().await;
            let partition = MapPartitionId(1);
            let first_time = UtcTimestamp::from_unix_milliseconds(1_000);
            let second_time = UtcTimestamp::from_unix_milliseconds(2_000);
            let earlier_time = UtcTimestamp::from_unix_milliseconds(1_500);

            let first_seq = env
                .store
                .append(
                    partition,
                    GridPos::new(5, 5),
                    first_time,
                    &sample_cause(1, "First action."),
                    &sample_change(),
                )
                .await
                .expect("append first event");
            let second_seq = env
                .store
                .append(
                    partition,
                    GridPos::new(6, 7),
                    second_time,
                    &sample_cause(2, "Second action."),
                    &sample_change(),
                )
                .await
                .expect("append second event");

            assert_eq!(first_seq, EventSequence(0));
            assert_eq!(second_seq, EventSequence(1));

            let out_of_order = env
                .store
                .append(
                    partition,
                    GridPos::new(8, 8),
                    earlier_time,
                    &sample_cause(3, "Late arrival."),
                    &sample_change(),
                )
                .await
                .expect_err("out of order timestamp must fail");
            assert_eq!(
                out_of_order.to_string(),
                DatabaseError::OutOfOrder(EventLogError::OutOfOrder {
                    attempted: earlier_time,
                    latest: second_time,
                })
                .to_string()
            );

            let all = env.store.since(EventSequence(0)).await.expect("since 0");
            assert_eq!(all.len(), 2);
            assert_eq!(all[0].record.sequence, EventSequence(0));
            assert_eq!(all[1].record.sequence, EventSequence(1));

            let tail = env.store.since(EventSequence(1)).await.expect("since 1");
            assert_eq!(tail.len(), 1);
            assert_eq!(tail[0].record.sequence, EventSequence(1));
            assert_eq!(tail[0].position, GridPos::new(6, 7));

            let empty = env.store.since(EventSequence(5)).await.expect("since 5");
            assert_eq!(empty, Vec::new());

            env.drop_schema().await;
        });
    }

    #[test]
    #[ignore = "requires local PostgreSQL from tools/dev_up.ps1"]
    fn spatial_queries_filter_by_partition_grid_radius_and_euclidean_radius() {
        block_on(async {
            let env = IsolatedTestSchema::create().await;
            let partition_one = MapPartitionId(1);
            let partition_two = MapPartitionId(2);
            let center = GridPos::new(10, 10);

            // Sequence 0: at (13, 14) -> Euclidean distance 5.0, grid distance 4.
            env.store
                .append(
                    partition_one,
                    GridPos::new(13, 14),
                    UtcTimestamp::from_unix_milliseconds(100),
                    &sample_cause(1, "Diagonal 3-4-5."),
                    &sample_change(),
                )
                .await
                .expect("append seq 0");
            // Sequence 1: at (15, 15) -> Euclidean distance ~7.07, grid distance 5.
            env.store
                .append(
                    partition_one,
                    GridPos::new(15, 15),
                    UtcTimestamp::from_unix_milliseconds(200),
                    &sample_cause(2, "Grid distance 5."),
                    &sample_change(),
                )
                .await
                .expect("append seq 1");
            // Sequence 2: at (10, 10) in partition 2 -> must not appear in partition 1 queries.
            env.store
                .append(
                    partition_two,
                    GridPos::new(10, 10),
                    UtcTimestamp::from_unix_milliseconds(300),
                    &sample_cause(3, "Other partition."),
                    &sample_change(),
                )
                .await
                .expect("append seq 2");

            let euclidean_5 = env
                .store
                .within_euclidean_radius(partition_one, center, 5.0)
                .await
                .expect("within euclidean radius 5");
            assert_eq!(euclidean_5.len(), 1);
            assert_eq!(euclidean_5[0].record.sequence, EventSequence(0));

            let grid_5 = env
                .store
                .within_grid_radius(partition_one, center, 5)
                .await
                .expect("within grid radius 5");
            assert_eq!(grid_5.len(), 2);
            assert_eq!(grid_5[0].record.sequence, EventSequence(0));
            assert_eq!(grid_5[1].record.sequence, EventSequence(1));

            let other_partition = env
                .store
                .within_grid_radius(partition_two, center, 0)
                .await
                .expect("partition 2 radius 0");
            assert_eq!(other_partition.len(), 1);
            assert_eq!(other_partition[0].record.sequence, EventSequence(2));

            env.drop_schema().await;
        });
    }
}
