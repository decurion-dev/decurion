-- The master plan, the page library and questions inside pages. Numbered
-- apart from other work in flight (0047 is attached files) so parallel
-- branches do not collide.

-- One ordered checklist per app, so the owner always sees what is done and
-- what is left. Every goal gets a row when it starts. To-do rows are what
-- an audit found, what the project lead suggested, or what the owner added;
-- a to-do found in a goal sits under that goal's row (`parent_id`), one the
-- owner added sits at the top level beside the goals.
CREATE TABLE plan_items (
    id          INTEGER PRIMARY KEY,
    app_id      INTEGER NOT NULL REFERENCES apps (id),
    parent_id   INTEGER REFERENCES plan_items (id),
    -- Order among rows with the same parent; smaller comes first.
    position    INTEGER NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('goal', 'todo')),
    -- A goal row's goal. Its title, steps and state are read from the goal,
    -- so they can never fall behind it.
    goal_id     INTEGER UNIQUE REFERENCES goals (id),
    title       TEXT NOT NULL,
    detail      TEXT,
    source      TEXT NOT NULL CHECK (source IN ('owner', 'lead')),
    -- To-do rows only; a goal row's state comes from its goal.
    state       TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'done', 'dropped')),
    -- The goal started to do this to-do. When that goal is closed as done,
    -- the to-do is ticked in the same transaction.
    fix_goal_id INTEGER REFERENCES goals (id),
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    done_at     TEXT,
    CHECK ((kind = 'goal') = (goal_id IS NOT NULL)),
    CHECK (kind = 'todo' OR parent_id IS NULL)
);
CREATE INDEX plan_items_app ON plan_items (app_id, parent_id, position);
CREATE INDEX plan_items_fix ON plan_items (fix_goal_id);

-- Goals that started before the plan existed, in the order they started.
INSERT INTO plan_items (app_id, position, kind, goal_id, title, source, created_at)
SELECT app_id, id, 'goal', id, title, 'owner', created_at FROM goals ORDER BY id;

-- The page's own <title>, for the library. Pages saved before this show a
-- name made from their kind.
ALTER TABLE goal_pages ADD COLUMN title TEXT;

-- Questions a designer or checker asked inside its page. The hub draws the
-- answer buttons around the page, never inside it: pages still run no
-- scripts. Answered once; the answers reach the project lead as an owner
-- message (`message_id`).
CREATE TABLE page_questions (
    page_id     INTEGER PRIMARY KEY REFERENCES goal_pages (id),
    -- JSON list of `Question`. A question with no choices takes words only.
    questions   TEXT NOT NULL,
    -- JSON list of `Answer`, once answered.
    answers     TEXT,
    answered_at TEXT,
    message_id  INTEGER REFERENCES goal_messages (id)
);
