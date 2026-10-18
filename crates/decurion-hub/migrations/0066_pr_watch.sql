-- Decurion keeps an eye on the pull requests it opened, with the owner's
-- token, until each one is merged or closed: it records the end and any new
-- review comments, so the owner hears about them without opening GitHub.
-- One row per pull request, keyed by the work that opened it.
CREATE TABLE pr_watches (
    thread_id  INTEGER PRIMARY KEY REFERENCES threads (id),
    app_id     INTEGER NOT NULL REFERENCES apps (id),
    owner      TEXT NOT NULL,
    repo       TEXT NOT NULL,
    number     INTEGER NOT NULL CHECK (number > 0),
    url        TEXT NOT NULL,
    -- Always under decurion/, never the default branch.
    branch     TEXT NOT NULL,
    -- The last commit Decurion pushed to that branch. A fix of review
    -- comments starts here and may only move the branch forward from it.
    head       TEXT NOT NULL,
    -- Who opened it on GitHub (the owner's login), once known.
    author     TEXT,
    state      TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'merged', 'closed')),
    checked_at TEXT,
    -- Plain words for the last thing that happened, and when.
    event      TEXT,
    event_at   TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX pr_watches_open ON pr_watches (state) WHERE state = 'open';
CREATE INDEX pr_watches_app ON pr_watches (app_id);

-- Comments people left on a watched pull request. They are other people's
-- words: a fix run gets them as data to weigh, never as its rules.
CREATE TABLE pr_comments (
    id         INTEGER PRIMARY KEY,
    thread_id  INTEGER NOT NULL REFERENCES pr_watches (thread_id),
    -- 'review' (a review's own text), 'line' (on a line of code), 'comment'.
    kind       TEXT NOT NULL CHECK (kind IN ('review', 'line', 'comment')),
    gh_id      INTEGER NOT NULL,
    author     TEXT NOT NULL,
    body       TEXT NOT NULL,
    path       TEXT,
    line       INTEGER,
    url        TEXT,
    -- When it was written, as GitHub says.
    at         TEXT NOT NULL,
    seen_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    -- The fix run the owner started for it, once they tap Fix these.
    fix_thread INTEGER REFERENCES threads (id),
    UNIQUE (kind, gh_id)
);
CREATE INDEX pr_comments_open ON pr_comments (thread_id) WHERE fix_thread IS NULL;

-- A fix of review comments is new work that goes to the same branch as the
-- work whose pull request it answers.
ALTER TABLE threads ADD COLUMN fix_of INTEGER REFERENCES threads (id);
