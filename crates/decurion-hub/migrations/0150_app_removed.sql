-- The owner can remove an app from Decurion. Removing only hides it: the row
-- stays (with removed_at set) so its work, goals, audit rows and spend keep
-- counting. The name is renamed on removal so the owner can add the same one
-- again (apps.name is UNIQUE).
ALTER TABLE apps ADD COLUMN removed_at TEXT;
