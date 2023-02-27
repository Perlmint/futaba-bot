-- Add migration script here
CREATE TABLE IF NOT EXISTS events (
    channel TEXT NOT NULL,
    created_at DATETIME NOT NULL,
    modified_at DATETIME NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    begin_date DATE NOT NULL,
    begin_time TIME,
    end_date DATE,
    end_time TIME
);