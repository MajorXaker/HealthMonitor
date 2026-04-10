-- Create the emails table for storing monitored inbox messages.
CREATE TABLE IF NOT EXISTS emails (
    id          SERIAL PRIMARY KEY,
    message_id  TEXT UNIQUE NOT NULL,
    subject     TEXT NOT NULL,
    sender      TEXT NOT NULL,
    account     TEXT NOT NULL DEFAULT '',
    received_at TIMESTAMPTZ NOT NULL,
    is_new      BOOLEAN NOT NULL DEFAULT TRUE
);
