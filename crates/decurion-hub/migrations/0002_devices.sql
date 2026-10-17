-- Browsers and phones the owner has signed in. Only a hash of each device's
-- token is kept, so a copy of the database cannot be used to sign in.
CREATE TABLE devices (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_seen  TEXT,
    revoked_at TEXT
);

-- Short-lived codes shown as a QR code on the board to pair a phone.
CREATE TABLE pairing_codes (
    code_hash  TEXT PRIMARY KEY,
    expires_at TEXT NOT NULL,
    used_at    TEXT
);

-- Where to send phone notifications, one per device.
CREATE TABLE push_subscriptions (
    device_id  INTEGER PRIMARY KEY REFERENCES devices (id),
    endpoint   TEXT NOT NULL,
    p256dh     TEXT NOT NULL,
    auth       TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
