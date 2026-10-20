-- Fix conflicts by itself: an app's rule that lets the pull request watcher
-- start a Bring up to date run by itself when a pull request Decurion opened
-- conflicts with its base branch. Starting is the only automatic part: the
-- result still waits in review for the owner's Accept before anything is
-- sent. Off for every app, old and new, until the owner turns it on.
ALTER TABLE apps ADD COLUMN update_conflicts INTEGER NOT NULL DEFAULT 0
    CHECK (update_conflicts IN (0, 1));

-- What the watcher last tried by itself on this pull request, as
-- 'head:merge_from' (the commit Decurion pushed there and the base branch's
-- commit it merged in). The same conflict on the same commits is never
-- tried by itself again; a new pushed head or a new base tip allows one
-- more try.
ALTER TABLE pr_watches ADD COLUMN auto_update_for TEXT;

-- Work Decurion started by itself (the rule above), not on the owner's tap,
-- so the screens can say so.
ALTER TABLE threads ADD COLUMN auto_started INTEGER NOT NULL DEFAULT 0
    CHECK (auto_started IN (0, 1));
