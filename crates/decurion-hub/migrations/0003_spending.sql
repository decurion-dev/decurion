-- Apps can run on the pay-per-use API key instead of a Max account.
ALTER TABLE apps ADD COLUMN pay_per_use INTEGER NOT NULL DEFAULT 0 CHECK (pay_per_use IN (0, 1));

-- One row per run of a thread: which account paid, how much was set aside
-- before it started, and what it actually cost. Money is in micro-dollars
-- (1 USD = 1,000,000) so sums never round.
CREATE TABLE runs (
    id              INTEGER PRIMARY KEY,
    thread_id       INTEGER NOT NULL REFERENCES threads (id),
    account         TEXT NOT NULL CHECK (account IN ('A', 'B', 'api')),
    reserved_micros INTEGER NOT NULL DEFAULT 0,
    cost_micros     INTEGER,
    input_tokens    INTEGER,
    output_tokens   INTEGER,
    started_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    finished_at     TEXT
);
CREATE INDEX runs_thread ON runs (thread_id, id);
CREATE INDEX runs_month ON runs (account, started_at);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT INTO settings (key, value) VALUES ('monthly_cap_micros', '100000000');
