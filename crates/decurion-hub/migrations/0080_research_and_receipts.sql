-- Research pieces, "Ask why" on finished work, and the checker's receipts.
-- Only new columns and a new table, so parallel branches that change the
-- same tables still apply cleanly in any order.

-- A research piece studies the app and writes a report; it changes no code,
-- skips the checker and is never sent to GitHub. Set once, at plan approval.
ALTER TABLE threads ADD COLUMN research INTEGER NOT NULL DEFAULT 0 CHECK (research IN (0, 1));

-- What the checker ran and what came of it, and its claims marked checked
-- or guessed, from its last check. JSON lists, like review_issues.
ALTER TABLE goal_pieces ADD COLUMN review_evidence TEXT NOT NULL DEFAULT '[]';
ALTER TABLE goal_pieces ADD COLUMN review_claims TEXT NOT NULL DEFAULT '[]';

-- For a coordinator turn that answers "why" about a finished piece: the
-- piece's real change and its agent's log, gathered by the hub when the
-- owner asked, so the turn is not held up running git at claim time.
ALTER TABLE helper_jobs ADD COLUMN why_context TEXT;

-- A research piece's report page, written by its agent (or made by the hub
-- from the agent's summary). One per piece; a later run replaces it. Served
-- only to the owner, in the same sandbox as the goal pages.
CREATE TABLE piece_reports (
    thread_id  INTEGER PRIMARY KEY REFERENCES goal_pieces (thread_id),
    goal_id    INTEGER NOT NULL REFERENCES goals (id),
    html       TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
