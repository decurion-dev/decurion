-- Add-ons (ADR 0001, v1: tools only). The owner adds one from its link with
-- one approval, and turns it on per app. Only new tables, so parallel
-- branches that change other tables still apply cleanly in any order.

-- One row per installed add-on: exactly the version the owner approved.
-- `tools` is what runs (JSON list of decurion_proto::addon::ToolUse);
-- `grants` is what the owner agreed to (JSON), which an update is compared
-- with: an update asking for nothing more is applied without asking again.
CREATE TABLE addons (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    version     TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    source      TEXT NOT NULL,
    folder      TEXT NOT NULL DEFAULT '',
    rev         TEXT NOT NULL,
    tree_hash   TEXT NOT NULL,
    tools       TEXT NOT NULL,
    grants      TEXT NOT NULL,
    approved_by TEXT NOT NULL,
    approved_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- Which apps an add-on is on for. Off is the absence of a row.
CREATE TABLE addon_apps (
    addon_id TEXT NOT NULL REFERENCES addons (id),
    app_id   INTEGER NOT NULL REFERENCES apps (id),
    on_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (addon_id, app_id)
);

-- Everything that happens to an add-on, with who did it. Append-only, and
-- kept after the add-on is removed. The state-move `audit` table needs a
-- piece of work on every row, so add-ons keep their own.
CREATE TABLE addon_log (
    id        INTEGER PRIMARY KEY,
    at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    actor     TEXT NOT NULL,
    addon_id  TEXT NOT NULL,
    event     TEXT NOT NULL CHECK (event IN (
                  'installed', 'updated', 'removed', 'enabled', 'disabled',
                  'used', 'key_used', 'refused')),
    app_id    INTEGER,
    thread_id INTEGER,
    detail    TEXT NOT NULL DEFAULT ''
);
CREATE INDEX addon_log_addon ON addon_log (addon_id, id);

CREATE TRIGGER addon_log_no_update BEFORE UPDATE ON addon_log
BEGIN SELECT RAISE(ABORT, 'addon_log is append-only'); END;
CREATE TRIGGER addon_log_no_delete BEFORE DELETE ON addon_log
BEGIN SELECT RAISE(ABORT, 'addon_log is append-only'); END;

-- What an add-on tap (Add, Update, on or off for an app, Remove) answered.
-- The tap's receipt goes in `receipts` as usual, naming the row here, so a
-- phone that resends after a dropped connection gets that same answer
-- back and the hub never acts twice. Add-ons have text ids, so the answer
-- is kept whole rather than rebuilt from a number.
CREATE TABLE addon_answers (
    id     INTEGER PRIMARY KEY,
    answer TEXT NOT NULL
);
