-- A thread can be part of a bigger task. Stopping the parent stops its
-- children first, except background ones, which keep going.
ALTER TABLE threads ADD COLUMN parent_id INTEGER REFERENCES threads (id);
ALTER TABLE threads ADD COLUMN background INTEGER NOT NULL DEFAULT 0 CHECK (background IN (0, 1));
CREATE INDEX threads_parent ON threads (parent_id);

-- What the owner sent an agent. The runner sets delivered_at once the agent
-- has it, so nothing is lost if the runner dies in between.
CREATE TABLE messages (
    id           INTEGER PRIMARY KEY,
    thread_id    INTEGER NOT NULL REFERENCES threads (id),
    kind         TEXT NOT NULL CHECK (kind IN ('steer', 'follow_up', 'note')),
    text         TEXT NOT NULL,
    at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    delivered_at TEXT
);
CREATE INDEX messages_thread ON messages (thread_id, id);

-- Taps already acted on. A phone that retries after a dropped connection
-- sends the same receipt and gets the first answer back.
CREATE TABLE receipts (
    receipt   TEXT PRIMARY KEY,
    action    TEXT NOT NULL,
    result_id INTEGER NOT NULL,
    at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
