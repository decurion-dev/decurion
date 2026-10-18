-- Websites the owner signed in to once, so an app's agents can test pages
-- behind a sign-in. The sign-in itself (cookies and local storage) is sealed
-- in the vault on this PC; this table never holds it, only what the board
-- shows and which saved copy is current.
CREATE TABLE site_sign_ins (
    id          INTEGER PRIMARY KEY,
    app_id      INTEGER NOT NULL REFERENCES apps (id),
    -- The owner's name for it. Inbox cards and notifications are made from
    -- this and the app's name only, never from anything a site or agent said.
    label       TEXT NOT NULL,
    -- Where the sign-in window opens.
    start_url   TEXT NOT NULL,
    -- A page that needs the sign-in, checked before each run.
    check_url   TEXT,
    -- Where the site sent the browser to sign in, as last saved.
    login_url   TEXT,
    -- The sites it covers, a JSON list of host names, as last saved.
    domains     TEXT NOT NULL DEFAULT '[]',
    -- none: never saved. ok, expiring (within a day of expires_at) and
    -- expired follow expires_at; a run can also find it expired.
    status      TEXT NOT NULL DEFAULT 'none'
                CHECK (status IN ('none', 'ok', 'expiring', 'expired')),
    -- The vault file in use: site-{id}.v{version}.bin. 0 until saved.
    version     INTEGER NOT NULL DEFAULT 0,
    captured_at TEXT,
    expires_at  TEXT,
    added_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (app_id, label)
);

-- Which saved sign-ins each run was given, and which copy. Only a runner
-- still running that work may say one of them stopped working.
CREATE TABLE sign_in_runs (
    thread_id  INTEGER NOT NULL REFERENCES threads (id),
    sign_in_id INTEGER NOT NULL REFERENCES site_sign_ins (id) ON DELETE CASCADE,
    version    INTEGER NOT NULL,
    PRIMARY KEY (thread_id, sign_in_id)
);
