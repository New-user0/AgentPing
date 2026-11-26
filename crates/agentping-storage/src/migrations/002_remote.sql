-- AgentPing schema v2: two-way remote interaction

-- Provider message ids (space separated) of a delivered notification, so replies can be matched.
ALTER TABLE notifications ADD COLUMN external_id TEXT;

-- A question/permission/continue prompt waiting for a remote answer.
CREATE TABLE interactions (
    id                TEXT PRIMARY KEY,
    kind              TEXT NOT NULL,
    agent             TEXT NOT NULL,
    session_id        TEXT,
    project_name      TEXT,
    status            TEXT NOT NULL,
    request_json      TEXT NOT NULL,
    response_json     TEXT,
    responder         TEXT,
    remote_message_id TEXT,
    note              TEXT,
    created_at        TEXT NOT NULL,
    expires_at        TEXT NOT NULL,
    resolved_at       TEXT
);
CREATE INDEX idx_interactions_status ON interactions (status, expires_at);
CREATE INDEX idx_interactions_session ON interactions (agent, session_id, status);
CREATE INDEX idx_interactions_message ON interactions (remote_message_id);

-- Small persistent key/value state (presence, gateway cursors).
CREATE TABLE kv (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
