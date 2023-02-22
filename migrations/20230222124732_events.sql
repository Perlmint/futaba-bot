-- Add migration script here
CREATE TABLE IF NOT EXISTS events (
    event_id INTEGER(64) PRIMARY KEY NOT NULL,
    begin_date_time DATETIME NOT NULL,
    end_date_time DATETIME,
    name TEXT NOT NULL,
    description TEXT
);