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

### ChatGPT and Grok

Work can also go to ChatGPT (your OpenAI API key) or Grok (your xAI API key). Decurion runs them through [pi](https://github.com/earendil-works/pi), an open-source coding agent, in the same kind of sealed container.

1. Build the pi image: `docker build -t decurion/pi-worker:dev docker/pi-worker`
2. Save the keys you want to use: `python spike/wincred.py set openai` and `python spike/wincred.py set xai`, then paste each key (hidden input). They are stored as `decurion/openai-api` and `decurion/xai-api`.
3. On each provider's own site (OpenAI platform settings, xAI console), set a monthly spending limit equal to Decurion's cap for that key. Decurion's caps start at $25 a month each.
4. When you add work on the board, choose ChatGPT or Grok.

Decurion stops new work at the monthly cap, and stops a run once pi reports it has spent the money set aside for it. That per-run stop relies on what pi reports, and the agent holds the key while it runs, so the provider's own monthly limit is the real backstop: set it. (Claude pay-per-use is different: its key stays on your PC behind the gatekeeper.) ChatGPT work uses `gpt-5.5` and Grok work uses `grok-4.7`.

Each piece of work runs in its own container with its own copy of the code. The only credential inside is the one that work runs on: that profile's Claude token, on Claude pay-per-use the run's proxy token, or for ChatGPT and Grok that provider's API key. Work moves to *In review* only when the agent has committed to its branch.

Keep Docker Desktop's "Expose daemon on tcp://localhost:2375 without TLS" setting **off**. Containers can reach the host, and that port would give any agent full control of Docker.

## Layout

- `crates/decurion-proto` shared types and the allowed state moves
- `crates/decurion-hub` board state (SQLite), API, board page
- `crates/decurion-runner` starts one sealed agent container (Claude Code or pi) per piece of work
- `docker/claude-worker` the Claude Code image
- `docker/pi-worker` the pi image, for ChatGPT and Grok
- `docs/spikes` what we tested and learned

Licensed under Apache-2.0.
