-- Settings of the hub itself that the owner changes from the board while it
-- runs, and that must survive a restart. Today only one: the address phones
-- and cloud workers use to reach the hub, its Tailscale https name, saved
-- when the owner clicks "Connect your phone" (tailscale.rs). The setting
-- DECURION_PUBLIC_URL still wins over it when the hub starts.
CREATE TABLE hub_settings (
    name       TEXT PRIMARY KEY CHECK (name IN ('public_url')),
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
