-- What Claude last said about each Max plan's allowances, for the meters:
-- one row per plan and window ('five_hour', 'seven_day', ...), replaced by
-- each new reading. Readings with no window name are kept as 'plan'.
CREATE TABLE account_usage (
    account     TEXT NOT NULL CHECK (account IN ('A', 'B')),
    window_name TEXT NOT NULL,
    -- Share of the allowance used, 0 to 100.
    used_pct    REAL,
    -- When the allowance renews, in Unix seconds.
    resets_at   INTEGER,
    -- The plan's state ('allowed', 'allowed_warning', 'rejected') from the
    -- last reading that gave one for this window, and that reading's place in
    -- order across all windows, so the newest status wins.
    status      TEXT,
    status_seq  INTEGER,
    seen_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (account, window_name)
);
