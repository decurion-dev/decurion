-- Many apps a day: apps take turns in the queue, the owner can star one app
-- to go first today, and an app can name who reviews its pull requests.
-- The star is a date, so it ends by itself when the day does (the hub's own
-- clock) and nothing has to clear it.

-- A GitHub login. Its pull requests ask this person for a review.
ALTER TABLE apps ADD COLUMN reviewer TEXT;
-- The day (YYYY-MM-DD, the hub's local time) the owner starred this app.
ALTER TABLE apps ADD COLUMN first_on TEXT;
-- When the queue last handed out this app's work, so apps take turns.
ALTER TABLE apps ADD COLUMN last_claim_at TEXT;
