-- Test keys the owner saved for an app. Only the name and last four
-- characters are kept here; the value itself lives in the vault, never in
-- this database.
CREATE TABLE test_keys (
    app_id    INTEGER NOT NULL REFERENCES apps (id),
    name      TEXT NOT NULL,
    last_four TEXT NOT NULL,
    added_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (app_id, name)
);
