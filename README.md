# decurion
One coordinator. Every agent. Your rules. Decurion is an open-source control room for AI coding agents (Claude Code, Codex and more), with one board, one approvals inbox on your phone, and safe sealed workspaces.

> Early development. Windows first. Nothing here is ready for daily use yet.

## Try the skeleton (Windows, Docker Desktop)

1. Build the agent image: `docker build -t decurion/claude-worker:dev docker/claude-worker`
2. Save a Claude token for profile A: run `claude setup-token`, then `python spike/wincred.py set A` and paste it (hidden input). It is stored in Windows Credential Manager as `decurion/claude-profile-A`.
3. Start the hub: `cargo run -p decurion-hub`. Open the login link it prints.
4. Start the runner: `cargo run -p decurion-runner`.
5. On the board, add an app (a local git repo path) and some work.

To run an app on pay-per-use instead of a Max account, save an Anthropic API key with `python spike/wincred.py set api`, set a monthly spend limit on that key's Anthropic Console workspace equal to Decurion's cap, and switch the app under "Add an app". Decurion stops pay-per-use work at its own monthly cap (default $100). The key never enters a container: the runner keeps it in a small proxy on this PC, and each run gets a random token that only works through that proxy and stops working when the run ends. The Console limit stays as the backstop.

Each piece of work runs in its own container with its own copy of the code. The only credential inside is that profile's Claude token, or on pay-per-use the run's proxy token. Work moves to *In review* only when the agent has committed to its branch.

Open a card to talk to its agent while it works. *Change course now* stops its current step and gives it your message. *Do this next* waits until it finishes what it is doing. *Add a note* passes information along without interrupting. Work can be split into smaller tasks: stopping a task stops the smaller tasks inside it, except ones set to keep going on their own. If the PC or the runner stops in the middle of a step, the agent is told which step was cut off and checks what actually happened before it repeats anything that changes files or runs commands.

Keep Docker Desktop's "Expose daemon on tcp://localhost:2375 without TLS" setting **off**. Containers can reach the host, and that port would give any agent full control of Docker.

## Layout

- `crates/decurion-proto` shared types and the allowed state moves
- `crates/decurion-hub` board state (SQLite), API, board page
- `crates/decurion-runner` starts one sealed Claude Code container per piece of work
- `docker/claude-worker` the agent image
- `docs/spikes` what we tested and learned

Licensed under Apache-2.0.
