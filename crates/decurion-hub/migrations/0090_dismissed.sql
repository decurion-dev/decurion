-- Finished work and closed goals stayed on every screen forever. The owner
-- can now dismiss them: this only hides them from the board, the inbox and
-- the goal list, into a Dismissed view where one tap brings them back.
-- Nothing is deleted. Work that moves again (a retry) shows again.
ALTER TABLE threads ADD COLUMN dismissed_at TEXT;
ALTER TABLE goals ADD COLUMN dismissed_at TEXT;
