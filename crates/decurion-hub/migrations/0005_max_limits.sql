-- When a Max account runs out of usage, its work waits here until the reset
-- time and then resumes on its own.
CREATE TABLE profile_limits (
    profile TEXT PRIMARY KEY CHECK (profile IN ('A', 'B')),
    until   TEXT NOT NULL,
    hit_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- The owner moved this piece of work to pay-per-use while its Max account
-- was out.
ALTER TABLE threads ADD COLUMN pay_per_use INTEGER NOT NULL DEFAULT 0 CHECK (pay_per_use IN (0, 1));
