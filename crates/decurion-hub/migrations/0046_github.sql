-- Accepted work goes to GitHub as a branch and a pull request, sent by the
-- hub (never a box). The wish to send is saved with the accept itself, and
-- each step is recorded before the next, so a hub that stops halfway sends
-- again on restart without pushing twice or opening a second pull request.
-- A failed send never undoes the accept: the reason is kept for the board.
ALTER TABLE threads ADD COLUMN gh_state TEXT
    CHECK (gh_state IN ('waiting', 'sending', 'sent', 'failed'));
-- Always under decurion/, never the default branch.
ALTER TABLE threads ADD COLUMN gh_branch TEXT;
-- The commit sent, once read from the saved copy.
ALTER TABLE threads ADD COLUMN gh_head TEXT;
-- The pull request, or a link that opens one.
ALTER TABLE threads ADD COLUMN gh_url TEXT;
ALTER TABLE threads ADD COLUMN gh_pr INTEGER NOT NULL DEFAULT 0 CHECK (gh_pr IN (0, 1));
-- Plain words for the owner.
ALTER TABLE threads ADD COLUMN gh_error TEXT;
CREATE INDEX threads_gh_state ON threads (gh_state) WHERE gh_state IS NOT NULL;

-- Keys that belong to the owner rather than to one app. The values are in
-- the vault; only the last four characters are kept here, for the board.
CREATE TABLE owner_keys (
    name      TEXT PRIMARY KEY CHECK (name IN ('github')),
    last_four TEXT NOT NULL,
    added_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
