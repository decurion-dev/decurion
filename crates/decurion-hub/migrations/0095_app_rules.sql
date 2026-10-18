-- Rules for each app: what Decurion may do for this one app, set by the
-- owner on the app's own Settings page. Every app, old and new, starts at
-- "PR only": Decurion pushes and opens the pull request, people merge on
-- GitHub, and nothing deploys. Merging and deploying from Decurion still
-- take the owner's tap every time; these only say whether the button shows.

-- The owner may merge this app's pull requests from Decurion, one tap each.
ALTER TABLE apps ADD COLUMN can_merge INTEGER NOT NULL DEFAULT 0 CHECK (can_merge IN (0, 1));
-- The owner may deploy this app from Decurion, one tap each, with the steps
-- below.
ALTER TABLE apps ADD COLUMN can_deploy INTEGER NOT NULL DEFAULT 0 CHECK (can_deploy IN (0, 1));
-- The owner's deploy steps, one command per line (decurion_proto::deploy).
ALTER TABLE apps ADD COLUMN deploy_steps TEXT NOT NULL DEFAULT '';
-- Where Open site goes once a deploy finishes. https only.
ALTER TABLE apps ADD COLUMN deploy_site TEXT;
-- A goal's piece that passes its check goes to GitHub by itself (on), or
-- waits for the owner's Accept (off). On was the only way before.
ALTER TABLE apps ADD COLUMN send_checked INTEGER NOT NULL DEFAULT 1 CHECK (send_checked IN (0, 1));
-- The app's live version holds patient or personal data, so its agents are
-- told to use made-up data only. On until the owner says otherwise.
ALTER TABLE apps ADD COLUMN patient_data INTEGER NOT NULL DEFAULT 1 CHECK (patient_data IN (0, 1));
