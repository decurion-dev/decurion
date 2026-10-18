-- When a piece's pull request was merged on GitHub, as the hub's watch of
-- its pull requests saw it. A goal whose pieces are all merged (or never
-- went to GitHub) closes by itself, so the owner never taps "Mark done"
-- after merging. Decurion only records the merge; it never merges.
ALTER TABLE goal_pieces ADD COLUMN merged_at TEXT;
