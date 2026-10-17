-- What earlier runs of the same Claude session were charged. Claude Code
-- reports a resumed session's cost as a running total, so a run's own cost
-- is the reported total minus this.
ALTER TABLE runs ADD COLUMN prior_micros INTEGER NOT NULL DEFAULT 0;
