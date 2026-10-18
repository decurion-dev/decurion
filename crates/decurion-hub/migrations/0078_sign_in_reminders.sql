-- Which "runs out in a week" reminders the Sign-ins page has already sent,
-- so each sign-in gets one reminder per date it runs out, even across hub
-- restarts. A sign-in renewed with a new date can be reminded again.
CREATE TABLE sign_in_reminders (
    item TEXT PRIMARY KEY,
    expires_at TEXT NOT NULL,
    sent_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
