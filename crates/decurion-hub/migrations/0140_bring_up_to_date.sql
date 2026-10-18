-- Bring up to date: when a pull request Decurion opened falls behind its
-- base branch or conflicts with it, the owner can tap Bring up to date, and
-- an agent merges the base branch into it on the same pull request.
--
-- What GitHub last said about each watched pull request, read from the
-- pull request itself (no check runs): 'dirty' (conflicts with its base
-- branch), 'behind' (behind it), 'checking' (GitHub is still working it
-- out), or nothing to say. Kept with the head commit GitHub said it about,
-- so it is shown only while that is still the commit Decurion pushed.
ALTER TABLE pr_watches ADD COLUMN health TEXT
    CHECK (health IS NULL OR health IN ('dirty', 'behind', 'checking'));
ALTER TABLE pr_watches ADD COLUMN health_head TEXT;
-- The branch the pull request asks to merge into (GitHub's base.ref).
-- Bring up to date merges the tip of this branch, nothing else.
ALTER TABLE pr_watches ADD COLUMN base_ref TEXT;

-- For a Bring up to date run: the base branch's commit it merges in,
-- pinned in the hub's copy of the app before the run was created.
ALTER TABLE threads ADD COLUMN merge_from TEXT;
-- Plain words for why finished work is held for the owner (protected parts
-- changed), set by the hub when work with no goal piece reaches review.
ALTER TABLE threads ADD COLUMN hold TEXT;
