CREATE TABLE apps (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    repo_path  TEXT NOT NULL,
    profile    TEXT NOT NULL CHECK (profile IN ('A', 'B')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE threads (
    id         INTEGER PRIMARY KEY,
    app_id     INTEGER NOT NULL REFERENCES apps (id),
    title      TEXT NOT NULL,
    prompt     TEXT NOT NULL,
    state      TEXT NOT NULL DEFAULT 'queued'
               CHECK (state IN ('queued', 'running', 'needs_you', 'in_review', 'done', 'failed')),
    session_id TEXT,
    branch     TEXT,
    runner     TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX threads_state ON threads (state, id);

-- What agents did during runs.
CREATE TABLE events (
    id        INTEGER PRIMARY KEY,
    thread_id INTEGER NOT NULL REFERENCES threads (id),
    at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    body      TEXT NOT NULL
);
CREATE INDEX events_thread ON events (thread_id, id);

-- Every state change, with who made it. Append-only.
CREATE TABLE audit (
    id         INTEGER PRIMARY KEY,
    thread_id  INTEGER NOT NULL REFERENCES threads (id),
    at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    actor      TEXT NOT NULL,
    from_state TEXT NOT NULL,
    to_state   TEXT NOT NULL,
    note       TEXT
);

CREATE TRIGGER audit_no_update BEFORE UPDATE ON audit
BEGIN SELECT RAISE(ABORT, 'audit is append-only'); END;
CREATE TRIGGER audit_no_delete BEFORE DELETE ON audit
BEGIN SELECT RAISE(ABORT, 'audit is append-only'); END;
