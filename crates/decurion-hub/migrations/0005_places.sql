-- Phase 2: work runs on the PC or on a cloud worker, under the same rules.

-- Where each piece of work runs, and the app commit its copy started from.
-- The starting commit is fixed the first time a runner asks for it, so every
-- computer rebuilds the same copy.
ALTER TABLE threads ADD COLUMN place TEXT NOT NULL DEFAULT 'pc' CHECK (place IN ('pc', 'cloud'));
ALTER TABLE threads ADD COLUMN base_commit TEXT;

-- Cloud workers. Each has its own key; only a hash is kept, as for devices.
-- The PC runner is not listed: it uses the runner key in keys.json.
CREATE TABLE runners (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    removed_at TEXT
);

-- One-time codes in a cloud worker's setup text, swapped for its key on its
-- first start. The name is chosen when the code is made, so the setup text can
-- name the server.
CREATE TABLE runner_join_codes (
    code_hash  TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    used_at    TEXT
);

-- Saved copies of work (the branch and the agent's session), so any runner
-- can continue it. The files live in the hub's data folder.
CREATE TABLE checkpoints (
    id         INTEGER PRIMARY KEY,
    thread_id  INTEGER NOT NULL REFERENCES threads (id),
    runner     TEXT NOT NULL,
    head       TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX checkpoints_thread ON checkpoints (thread_id, id);

INSERT INTO settings (key, value) VALUES ('cloud_worker_seq', '0');
