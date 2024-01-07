-- Add migration script here
CREATE TABLE IF NOT EXISTS server_events (
    discord_id INTEGER(64) PRIMARY KEY NOT NULL,
    google_event_id TEXT NOT NULL,
    synced_at DATETIME,
    UNIQUE (`google_event_id`)
);
DROP TABLE events;
