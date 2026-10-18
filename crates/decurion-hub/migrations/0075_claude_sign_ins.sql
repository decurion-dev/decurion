-- When Decurion itself saved each Max plan's Claude sign-in ("Sign in on this
-- PC"), so the Sign-ins page can say when it runs out: Claude's sign-ins last
-- one year. The sign-in itself is in the vault, never here. `fingerprint` is
-- the start of its SHA-256, only to tell whether the saved sign-in is still
-- the one dated here (one saved another way has an unknown date).
CREATE TABLE claude_sign_ins (
    profile TEXT PRIMARY KEY CHECK (profile IN ('A', 'B')),
    fingerprint TEXT NOT NULL,
    saved_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
