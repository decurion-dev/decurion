# decurion
One coordinator. Every agent. Your rules. Decurion is an open-source control room for AI coding agents (Claude Code, Codex and more), with one board, one approvals inbox on your phone, and safe sealed workspaces.

> Early development. Windows first. Nothing here is ready for daily use yet.

## Try the skeleton (Windows, Docker Desktop)

1. Build the agent image: `docker build -t decurion/claude-worker:dev docker/claude-worker`
2. Save a Claude token for profile A: run `claude setup-token`, then `python spike/wincred.py set A` and paste it (hidden input). It is stored in Windows Credential Manager as `decurion/claude-profile-A`.
3. Start the hub: `cargo run -p decurion-hub`. Open the login link it prints.
4. Start the runner: `cargo run -p decurion-runner`.
5. On the board, add an app (a local git repo path) and some work.

To run an app on pay-per-use instead of a Max account, save an Anthropic API key with `python spike/wincred.py set api`, set a monthly spend limit on that key's Anthropic Console workspace equal to Decurion's cap, and switch the app under "Add an app". Decurion stops pay-per-use work at its own monthly cap (default $100); the Console limit is the backstop, because the agent holds the key while it runs.

Each piece of work runs in its own container with its own copy of the code. The only credential inside is that profile's Claude token. Work moves to *In review* only when the agent has committed to its branch.

Keep Docker Desktop's "Expose daemon on tcp://localhost:2375 without TLS" setting **off**. Containers can reach the host, and that port would give any agent full control of Docker.

## Layout

- `crates/decurion-proto` shared types and the allowed state moves
- `crates/decurion-hub` board state (SQLite), API, board page
- `crates/decurion-runner` starts one sealed Claude Code container per piece of work
- `docker/claude-worker` the agent image
- `docs/spikes` what we tested and learned

Licensed under Apache-2.0.
