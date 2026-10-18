-- Tokens per run and per helper job, split the way the board shows them:
-- input read afresh, read back from the prompt cache, or written to it; and
-- how full the conversation got. `input_tokens` stays all input. `resumed`
-- marks a run (or job) whose agent reports its whole session so far, so the
-- earlier runs' share is taken off, the same as `prior_micros` for money.
ALTER TABLE runs ADD COLUMN cache_read_tokens INTEGER;
ALTER TABLE runs ADD COLUMN cache_write_tokens INTEGER;
ALTER TABLE runs ADD COLUMN peak_context_tokens INTEGER;
ALTER TABLE runs ADD COLUMN compactions INTEGER;
ALTER TABLE runs ADD COLUMN resumed INTEGER NOT NULL DEFAULT 0;

ALTER TABLE helper_jobs ADD COLUMN cache_read_tokens INTEGER;
ALTER TABLE helper_jobs ADD COLUMN cache_write_tokens INTEGER;
ALTER TABLE helper_jobs ADD COLUMN peak_context_tokens INTEGER;
ALTER TABLE helper_jobs ADD COLUMN compactions INTEGER;
ALTER TABLE helper_jobs ADD COLUMN resumed INTEGER NOT NULL DEFAULT 0;
