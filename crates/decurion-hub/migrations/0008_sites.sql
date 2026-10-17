-- Websites each app's agents may reach on top of the built-in ones (Claude
-- and the package stores), as a JSON array of plain site names.
ALTER TABLE apps ADD COLUMN sites TEXT NOT NULL DEFAULT '[]';
