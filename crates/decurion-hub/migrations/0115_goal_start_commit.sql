-- New work used to start from whatever branch the owner's folder had out.
-- A goal now gets one start commit when it is created: the app's default
-- branch as its git server has it. The goal's project lead and designer
-- read that commit, and every piece of the goal starts from it. Goals from
-- before this have none and keep copying the folder as they did.
ALTER TABLE goals ADD COLUMN start_commit TEXT;
