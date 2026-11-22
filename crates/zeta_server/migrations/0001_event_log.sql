CREATE EXTENSION IF NOT EXISTS postgis;

CREATE TABLE event_log (
    sequence BIGINT PRIMARY KEY CHECK (sequence >= 0),
    recorded_at_unix_ms BIGINT NOT NULL,
    map_partition_id BIGINT NOT NULL CHECK (map_partition_id >= 0),
    grid_x BIGINT NOT NULL CHECK (grid_x >= 0),
    grid_y BIGINT NOT NULL CHECK (grid_y >= 0),
    location geometry(Point, 0) GENERATED ALWAYS AS (
        ST_MakePoint(grid_x::double precision, grid_y::double precision)
    ) STORED,
    cause JSONB NOT NULL,
    change JSONB NOT NULL
);

CREATE INDEX event_log_location_idx ON event_log USING GIST (location);

CREATE INDEX event_log_partition_sequence_idx ON event_log (map_partition_id, sequence);
