-- One tap on "Continue on pay-per-use" now keeps a Max plan's Claude work on
-- pay-per-use until that plan resets, not just the work waiting at the time.
-- Empty, or a time already past, means the plan's work waits for the reset.
ALTER TABLE profile_limits ADD COLUMN pay_per_use_until TEXT;
