-- A live view of a test browser on the owner's screen. 'take_over' watches
-- (and lets the owner drive) the agent's browser during a run; 'sign_in' is
-- the window the owner signs in to a site with, so it can be saved. The hub
-- only relays the picture and the owner's input; neither is ever kept here.
CREATE TABLE browser_sessions (
    id          INTEGER PRIMARY KEY,
    kind        TEXT NOT NULL CHECK (kind IN ('take_over', 'sign_in')),
    -- The run whose browser it is (take_over only).
    thread_id   INTEGER REFERENCES threads (id),
    -- The saved sign-in it is for (sign_in only); see site_sign_ins.
    sign_in_id  INTEGER,
    -- The runner holding the browser, by name ('local' for the PC).
    runner      TEXT,
    controller  TEXT NOT NULL DEFAULT 'agent' CHECK (controller IN ('agent', 'owner')),
    -- pending: waiting for the runner to connect; live: connected;
    -- saved: a sign-in was saved from it; closed: over; failed: broke.
    state       TEXT NOT NULL DEFAULT 'pending'
                CHECK (state IN ('pending', 'live', 'saved', 'closed', 'failed')),
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    ended_at    TEXT,
    CHECK (kind <> 'take_over' OR thread_id IS NOT NULL),
    CHECK (kind <> 'sign_in' OR sign_in_id IS NOT NULL)
);
CREATE INDEX browser_sessions_thread ON browser_sessions (thread_id, id);
-- At most one open take-over view per run.
CREATE UNIQUE INDEX browser_sessions_one_open ON browser_sessions (thread_id)
    WHERE kind = 'take_over' AND state IN ('pending', 'live');
