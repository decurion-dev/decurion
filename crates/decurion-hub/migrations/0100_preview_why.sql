-- A Run it preview that failed in its first seconds ended before the
-- owner's screen had connected, so the screen never learned why and looked
-- stuck. Why a preview ended is now kept with it, and whether an agent could
-- fix that (no start recipe, or the app stopped), so a screen that opens it
-- afterwards is told, and can offer to ask an agent.
ALTER TABLE browser_sessions ADD COLUMN why TEXT;
ALTER TABLE browser_sessions ADD COLUMN fixable INTEGER NOT NULL DEFAULT 0;
