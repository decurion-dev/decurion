-- The owner can delete rows of the master plan and bring them back later.
-- A deleted row keeps its place (`position`) and is only hidden, so it can
-- return where it was. Deleting a goal row sets the same `deleted_at` on its
-- to-dos, so bringing the goal back brings back only those, not a to-do the
-- owner deleted on its own earlier. "Empty" removes the deleted rows for good;
-- it touches only this table, never the goal, its work or its pages.
ALTER TABLE plan_items ADD COLUMN deleted_at TEXT;
