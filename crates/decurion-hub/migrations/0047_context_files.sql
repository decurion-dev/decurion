-- Files the owner attaches for agents to read: specs, briefs, price lists,
-- sketches. The files themselves live in the hub's saved-work folder under
-- context/app-<app_id>/<id> (and <id>.txt for a text version the hub made);
-- this table names them. A file with a goal_id goes only to that goal's
-- helpers and pieces; one without goes to all of the app's work.
CREATE TABLE context_files (
    id         INTEGER PRIMARY KEY,
    app_id     INTEGER NOT NULL REFERENCES apps (id),
    goal_id    INTEGER REFERENCES goals (id),
    -- Already cleaned (decurion_proto::context::clean_name): also its name
    -- in the box.
    name       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    kind       TEXT NOT NULL
               CHECK (kind IN ('pdf', 'word', 'sheet', 'slides', 'image', 'text', 'other')),
    -- Set when the hub made a text version: its length in characters.
    text_chars INTEGER,
    -- Plain words for the owner about reading the file.
    note       TEXT NOT NULL DEFAULT '',
    added_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX context_files_app ON context_files (app_id, id);
