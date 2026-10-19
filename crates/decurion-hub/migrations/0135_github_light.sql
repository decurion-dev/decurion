-- An honest GitHub light. When the owner's GitHub token could send work but
-- GitHub would not let it open a pull request on an app's repo, the hub
-- remembers that per app (when, and the plain reason), so Settings, Sign-ins
-- turns GitHub amber instead of saying all is well. The row goes away as soon
-- as a pull request opens on that app again. Never holds the token.
CREATE TABLE github_pr_trouble (
    app_id INTEGER PRIMARY KEY REFERENCES apps (id),
    -- 'no_access': GitHub said 403 or 404 (the token can't open pull
    -- requests there); 'refused': GitHub said 401 (it no longer takes it).
    kind   TEXT NOT NULL CHECK (kind IN ('no_access', 'refused')),
    reason TEXT NOT NULL,
    at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- GitHub says a pull request Decurion opened clashes with newer changes on
-- its base branch (mergeable_state "dirty"). Read on each look at it, so the
-- board can say so in plain words before the owner meets GitHub's red warning.
ALTER TABLE pr_watches ADD COLUMN clash INTEGER NOT NULL DEFAULT 0 CHECK (clash IN (0, 1));
