-- One-tap deploy (ADR 0002). After the owner merges, an app whose rules
-- allow deploys gets one "Ready to deploy" card, bound to the exact commit on
-- its main line. One Deploy tap runs the app's own deploy steps on the
-- owner's PC. Every deploy is a row here, and every tap and end is written to
-- deploy_audit.
CREATE TABLE deploys (
    id            INTEGER PRIMARY KEY,
    app_id        INTEGER NOT NULL REFERENCES apps (id) ON DELETE CASCADE,
    -- The app's main line on its remote, and the commit the card shows.
    branch        TEXT NOT NULL,
    commit_id     TEXT NOT NULL,
    -- Titles of the changes since the last live release, newest first (JSON).
    changes       TEXT NOT NULL DEFAULT '[]',
    -- Files the deploy steps name, with a hash of each at commit_id (JSON).
    files         TEXT NOT NULL DEFAULT '{}',
    -- Those files that differ from the last live release (JSON list).
    steps_changed TEXT NOT NULL DEFAULT '[]',
    state         TEXT NOT NULL DEFAULT 'waiting'
                  CHECK (state IN ('waiting', 'deploying', 'live', 'failed', 'hidden')),
    -- Plain words for how far a running deploy has got.
    step          TEXT,
    error         TEXT,
    -- The last lines of the deploy's output, for the owner's Details.
    tail          TEXT,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    started_at    TEXT,
    ended_at      TEXT
);

-- At most one card per app that is not finished with: waiting, running or
-- failed. A newer merge moves that card on instead of adding another ask.
CREATE UNIQUE INDEX deploys_one_open ON deploys (app_id)
    WHERE state IN ('waiting', 'deploying', 'failed');

CREATE TABLE deploy_audit (
    id         INTEGER PRIMARY KEY,
    deploy_id  INTEGER NOT NULL REFERENCES deploys (id) ON DELETE CASCADE,
    actor      TEXT NOT NULL,
    what       TEXT NOT NULL,
    note       TEXT,
    at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
