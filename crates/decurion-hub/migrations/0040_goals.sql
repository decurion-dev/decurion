-- Goals and the coordinator. Numbered apart from the other work in flight so
-- parallel branches do not collide; sqlx applies any unapplied migration.

CREATE TABLE goals (
    id         INTEGER PRIMARY KEY,
    app_id     INTEGER NOT NULL REFERENCES apps (id),
    title      TEXT NOT NULL,
    state      TEXT NOT NULL DEFAULT 'talking'
               CHECK (state IN ('talking', 'plan_ready', 'working', 'for_review', 'done', 'stopped')),
    -- The coordinator's Claude Code session. Replaced with a new one after a
    -- turn fails, and the next turn then gets the whole conversation.
    session_id TEXT NOT NULL,
    session_ok INTEGER NOT NULL DEFAULT 0 CHECK (session_ok IN (0, 1)),
    -- The last message the coordinator has seen.
    seen_message_id INTEGER NOT NULL DEFAULT 0,
    stuck      INTEGER NOT NULL DEFAULT 0 CHECK (stuck IN (0, 1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- The conversation: owner, coordinator and Decurion's own notes. Append-only.
CREATE TABLE goal_messages (
    id      INTEGER PRIMARY KEY,
    goal_id INTEGER NOT NULL REFERENCES goals (id),
    at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    body    TEXT NOT NULL
);
CREATE INDEX goal_messages_goal ON goal_messages (goal_id, id);
CREATE TRIGGER goal_messages_no_update BEFORE UPDATE ON goal_messages
BEGIN SELECT RAISE(ABORT, 'goal messages are append-only'); END;
CREATE TRIGGER goal_messages_no_delete BEFORE DELETE ON goal_messages
BEGIN SELECT RAISE(ABORT, 'goal messages are append-only'); END;

-- One row per piece of an approved plan, beside the thread that builds it.
CREATE TABLE goal_pieces (
    thread_id      INTEGER PRIMARY KEY REFERENCES threads (id),
    goal_id        INTEGER NOT NULL REFERENCES goals (id),
    title          TEXT NOT NULL,
    done_when      TEXT NOT NULL,
    review         TEXT NOT NULL DEFAULT 'building'
                   CHECK (review IN ('building', 'reviewing', 'fixing', 'passed', 'needs_a_look', 'failed')),
    rounds         INTEGER NOT NULL DEFAULT 0,
    review_summary TEXT,
    review_issues  TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX goal_pieces_goal ON goal_pieces (goal_id, thread_id);

-- Coordinator turns and reviews. Money works like `runs`: micro-dollars,
-- set aside before the container starts, real cost recorded when known.
CREATE TABLE helper_jobs (
    id              INTEGER PRIMARY KEY,
    kind            TEXT NOT NULL CHECK (kind IN ('coordinator', 'design', 'review')),
    goal_id         INTEGER NOT NULL REFERENCES goals (id),
    thread_id       INTEGER REFERENCES threads (id),
    -- talk: answer the owner; report: sum up finished work; design: draw the
    -- plan in `message_id`; review: check a piece.
    purpose         TEXT NOT NULL CHECK (purpose IN ('talk', 'report', 'design', 'review')),
    message_id      INTEGER REFERENCES goal_messages (id),
    -- 2 when this job asks again after an answer that could not be read.
    attempt         INTEGER NOT NULL DEFAULT 1,
    -- Why the previous attempt's answer was refused, for the retry prompt.
    fix_note        TEXT,
    state           TEXT NOT NULL DEFAULT 'queued'
                    CHECK (state IN ('queued', 'running', 'done', 'failed', 'cancelled')),
    session_id      TEXT,
    prompt          TEXT,
    -- The last goal message included in this turn's prompt.
    upto_message_id INTEGER,
    runner          TEXT,
    account         TEXT CHECK (account IN ('A', 'B', 'api')),
    reserved_micros INTEGER NOT NULL DEFAULT 0,
    prior_micros    INTEGER NOT NULL DEFAULT 0,
    cost_micros     INTEGER,
    input_tokens    INTEGER,
    output_tokens   INTEGER,
    container_started INTEGER NOT NULL DEFAULT 0,
    result          TEXT,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    started_at      TEXT,
    finished_at     TEXT
);
CREATE INDEX helper_jobs_state ON helper_jobs (state, id);
CREATE INDEX helper_jobs_goal ON helper_jobs (goal_id, id);

-- What the coordinator learned about each app, read at the start of a goal.
CREATE TABLE app_notes (
    id         INTEGER PRIMARY KEY,
    app_id     INTEGER NOT NULL REFERENCES apps (id),
    text       TEXT NOT NULL,
    goal_id    INTEGER REFERENCES goals (id),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    removed_at TEXT
);
CREATE INDEX app_notes_app ON app_notes (app_id, id);

-- Visual pages agents drew: plans with mockups, finished pieces with
-- screenshots. Served only to the owner, in a sandbox that runs no scripts.
CREATE TABLE goal_pages (
    id         INTEGER PRIMARY KEY,
    goal_id    INTEGER NOT NULL REFERENCES goals (id),
    kind       TEXT NOT NULL CHECK (kind IN ('plan', 'check')),
    message_id INTEGER REFERENCES goal_messages (id),
    thread_id  INTEGER REFERENCES threads (id),
    job_id     INTEGER NOT NULL REFERENCES helper_jobs (id),
    html       TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX goal_pages_goal ON goal_pages (goal_id, id);
