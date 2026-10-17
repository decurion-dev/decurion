-- Work can be done by ChatGPT or Grok as well as Claude.
ALTER TABLE threads ADD COLUMN ai TEXT NOT NULL DEFAULT 'claude'
    CHECK (ai IN ('claude', 'chatgpt', 'grok'));

-- An app's own monthly cap on pay-per-use money, across every paid account.
-- NULL means only the accounts' caps apply.
ALTER TABLE apps ADD COLUMN monthly_cap_micros INTEGER
    CHECK (monthly_cap_micros IS NULL OR monthly_cap_micros >= 0);

-- Runs can be paid by the OpenAI and xAI keys. SQLite cannot change a CHECK
-- in place, so the table is rebuilt with the same rows.
CREATE TABLE runs_new (
    id              INTEGER PRIMARY KEY,
    thread_id       INTEGER NOT NULL REFERENCES threads (id),
    account         TEXT NOT NULL CHECK (account IN ('A', 'B', 'api', 'openai', 'xai')),
    reserved_micros INTEGER NOT NULL DEFAULT 0,
    cost_micros     INTEGER,
    input_tokens    INTEGER,
    output_tokens   INTEGER,
    started_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    finished_at     TEXT,
    prior_micros    INTEGER NOT NULL DEFAULT 0
);
INSERT INTO runs_new (id, thread_id, account, reserved_micros, cost_micros, input_tokens,
                      output_tokens, started_at, finished_at, prior_micros)
    SELECT id, thread_id, account, reserved_micros, cost_micros, input_tokens,
           output_tokens, started_at, finished_at, prior_micros FROM runs;
DROP TABLE runs;
ALTER TABLE runs_new RENAME TO runs;
CREATE INDEX runs_thread ON runs (thread_id, id);
CREATE INDEX runs_month ON runs (account, started_at);

-- Each paid account has its own monthly cap. The Anthropic key keeps the
-- existing `monthly_cap_micros`; the others start at $25.
INSERT INTO settings (key, value) VALUES ('monthly_cap_micros:openai', '25000000');
INSERT INTO settings (key, value) VALUES ('monthly_cap_micros:xai', '25000000');
