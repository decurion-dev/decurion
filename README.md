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

## Work on your PC or in the cloud

Every piece of work runs on your PC or on a cloud worker, under the same rules: the same sealed box, the same single credential, the same board and inbox. Open a card and choose **Move to cloud** or **Move to PC**; running work saves its progress, stops, and carries on from the same spot on the other computer, with no new questions.

To add a cloud worker, start the hub with `DECURION_PUBLIC_URL` set to its Tailscale https address (the same one your phone uses), then open **Add cloud worker** on the board and follow the three steps (see `docs/help/add-cloud-worker.md`). The server costs about $5 to $25 a month at a hosting company.

How it works: the hub is the meeting point for code. Runners never clone your repo themselves; they get the starting commit from the hub, and at the end of every run (or when you move the work) they hand back a saved copy: the branch and the agent's session. The cloud worker gets the run's Claude credential from the hub only while that work is running on it, and never stores it. The hub refuses any runner built with different rules (`RULES_VERSION`), and the cloud worker then rebuilds itself to the hub's commit.

Each agent also has its own private Chromium browser (Playwright) inside its box, for testing the pages it builds.

Keep Docker Desktop's "Expose daemon on tcp://localhost:2375 without TLS" setting **off**. Containers can reach the host, and that port would give any agent full control of Docker.

## Layout

- `crates/decurion-proto` shared types and the allowed state moves
- `crates/decurion-hub` board state (SQLite), API, board page
- `crates/decurion-runner` starts one sealed Claude Code container per piece of work, on the PC or a cloud worker
- `docker/claude-worker` the agent image, with its test browser
- `docker/cloud` how a cloud worker sets itself up, updates itself and walls off its agents
- `docs/help` help pages
- `docs/spikes` what we tested and learned

Licensed under Apache-2.0.
