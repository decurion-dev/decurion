# decurion
One coordinator. Every agent. Your rules. Decurion is an open-source control room for AI coding agents (Claude Code, Codex and more), with one board, one approvals inbox on your phone, and safe sealed workspaces.

> Early development. Windows first. Nothing here is ready for daily use yet.

## Install

Download the installer for Windows, or the `.tar.gz` for Mac or Linux, from the [Releases page](https://github.com/decurion-dev/decurion/releases). The [help pages](docs/help/src/README.md) walk you through installing, signing in your Claude accounts and adding your first app.

## Run from source (Windows, Docker Desktop)

1. Build the agent image: `docker build -t decurion/claude-worker:dev docker/claude-worker`
2. Save a Claude token for profile A: run `claude setup-token`, then `python spike/wincred.py set A` and paste it (hidden input). It is stored in Windows Credential Manager as `decurion/claude-profile-A`.
3. Start the hub: `cargo run -p decurion-hub`. Open the login link it prints.
4. Start the runner: `cargo run -p decurion-runner`.
5. On the board, add an app (a local git repo path) and some work.

To run an app on pay-per-use instead of a Max account, save an Anthropic API key with `python spike/wincred.py set api`, set a monthly spend limit on that key's Anthropic Console workspace equal to Decurion's cap, and switch the app under "Add an app". Decurion stops pay-per-use work at its own monthly cap (default $100). The key never enters a container: the runner keeps it in a small proxy on this PC, and each run gets a random token that only works through that proxy and stops working when the run ends. The Console limit stays as the backstop.

Each piece of work runs in its own container with its own copy of the code. The only credentials inside are that profile's Claude token (or on pay-per-use the run's proxy token) and any test keys you gave that app. Work moves to *In review* only when the agent has committed to its branch.

### Test keys

If an app's tests need keys (a Stripe test key, a test database key), add them on the board under *Settings*, *Test keys*. The name is the environment variable the app reads, for example `STRIPE_SECRET_KEY`.

- Values are saved in Windows Credential Manager as `decurion/app-<id>/test-key/<NAME>`, never in Decurion's database, the app's folder or git. The board shows only the last four characters. (Off Windows, for development, they go in a private `vault` folder in Decurion's data folder.)
- Live keys are refused: live Stripe keys, Anthropic, GitHub, GitLab and Slack tokens, AWS access keys and private keys.
- Each run gets that app's test keys as environment variables, and no other app's.
- The values are hidden from everything the board shows. If one turns up anywhere in the agent's new commits, the work stops before review instead of going to *In review*. A key that was already in the app's code before the agent started is not counted.

Keep Docker Desktop's "Expose daemon on tcp://localhost:2375 without TLS" setting **off**. Containers can reach the host, and that port would give any agent full control of Docker.

## Layout

- `crates/decurion-proto` shared types and the allowed state moves
- `crates/decurion-hub` board state (SQLite), API, board page
- `crates/decurion-runner` starts one sealed Claude Code container per piece of work
- `docker/claude-worker` the agent image
- `docs/spikes` what we tested and learned
- `docs/help` the help pages, `docs/adr` design decisions, `docs/addons` how to write an add-on
- `packaging` the Windows installer and the Mac and Linux install scripts (see [RELEASING.md](RELEASING.md))

Want to help? Read [CONTRIBUTING.md](CONTRIBUTING.md). Found a security problem? See [SECURITY.md](SECURITY.md).

Licensed under Apache-2.0.
