-- "Run it": a 'preview' live view shows the owner their app running, from
-- the app's own folder or from a piece of work's last saved copy. SQLite
-- cannot change a CHECK, so browser_sessions is rebuilt with the new kind,
-- the app it runs (app_id) and when the runner last said it is still
-- running (seen_at). Nothing references browser_sessions, so the rebuild
-- keeps every row and id as they were.
CREATE TABLE browser_sessions_new (
    id          INTEGER PRIMARY KEY,
    kind        TEXT NOT NULL CHECK (kind IN ('take_over', 'sign_in', 'preview')),
    -- The run whose browser it is (take_over), or the piece of work a
    -- preview runs (NULL runs the app as it is in its folder).
    thread_id   INTEGER REFERENCES threads (id),
    -- The saved sign-in it is for (sign_in only); see site_sign_ins.
    sign_in_id  INTEGER,
    -- The app a preview runs (preview only).
    app_id      INTEGER REFERENCES apps (id),
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
    -- Preview only: the last time its runner said it is still running.
    seen_at     TEXT,
    CHECK (kind <> 'take_over' OR thread_id IS NOT NULL),
    CHECK (kind <> 'sign_in' OR sign_in_id IS NOT NULL),
    CHECK (kind <> 'preview' OR app_id IS NOT NULL)
);
INSERT INTO browser_sessions_new
    (id, kind, thread_id, sign_in_id, runner, controller, state, created_at, updated_at, ended_at)
SELECT id, kind, thread_id, sign_in_id, runner, controller, state, created_at, updated_at, ended_at
FROM browser_sessions;
DROP TABLE browser_sessions;
ALTER TABLE browser_sessions_new RENAME TO browser_sessions;
CREATE INDEX browser_sessions_thread ON browser_sessions (thread_id, id);
-- At most one open take-over view per run.
CREATE UNIQUE INDEX browser_sessions_one_open ON browser_sessions (thread_id)
    WHERE kind = 'take_over' AND state IN ('pending', 'live');
-- At most one open preview at all: each runs a dev server and a browser on
-- the owner's PC.
CREATE UNIQUE INDEX browser_sessions_one_preview ON browser_sessions (kind)
    WHERE kind = 'preview' AND state IN ('pending', 'live');
