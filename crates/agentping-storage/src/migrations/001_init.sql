-- AgentPing schema v1

CREATE TABLE events (
    id              TEXT PRIMARY KEY,
    dedup_key       TEXT NOT NULL,
    schema_version  INTEGER NOT NULL,
    event_type      TEXT NOT NULL,
    agent           TEXT NOT NULL,
    session_id      TEXT,
    project_name    TEXT,
    project_path    TEXT,
    severity        TEXT NOT NULL,
    title           TEXT,
    message         TEXT,
    native_event    TEXT,
    event_json      TEXT NOT NULL,
    occurred_at     TEXT NOT NULL,
    received_at     TEXT NOT NULL,
    processed_at    TEXT,
    decision        TEXT
);
CREATE INDEX idx_events_dedup ON events (dedup_key, received_at);
CREATE INDEX idx_events_received ON events (received_at);
CREATE INDEX idx_events_session ON events (agent, session_id, event_type, received_at);
CREATE INDEX idx_events_unprocessed ON events (received_at) WHERE processed_at IS NULL;

CREATE TABLE sessions (
    agent            TEXT NOT NULL,
    session_id       TEXT NOT NULL,
    project_name     TEXT,
    project_path     TEXT,
    started_at       TEXT NOT NULL,
    last_activity_at TEXT NOT NULL,
    status           TEXT NOT NULL,
    last_event_type  TEXT,
    event_count      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (agent, session_id)
);
CREATE INDEX idx_sessions_activity ON sessions (last_activity_at);

CREATE TABLE notifications (
    id              TEXT PRIMARY KEY,
    event_id        TEXT NOT NULL REFERENCES events (id) ON DELETE CASCADE,
    provider        TEXT NOT NULL,
    status          TEXT NOT NULL,
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT,
    lease_until     TEXT,
    last_error      TEXT,
    cancel_key      TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    delivered_at    TEXT,
    UNIQUE (event_id, provider)
);
CREATE INDEX idx_notifications_due ON notifications (status, next_attempt_at);
CREATE INDEX idx_notifications_cancel ON notifications (cancel_key) WHERE cancel_key IS NOT NULL;

CREATE TABLE delivery_attempts (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    notification_id TEXT NOT NULL REFERENCES notifications (id) ON DELETE CASCADE,
    attempted_at    TEXT NOT NULL,
    outcome         TEXT NOT NULL,
    http_status     INTEGER,
    error           TEXT,
    duration_ms     INTEGER NOT NULL
);
CREATE INDEX idx_attempts_notification ON delivery_attempts (notification_id);
