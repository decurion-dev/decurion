# Decurion Phase 1 plan

**This change makes it so that** Claude Code agents run in sealed containers on the owner's PC and show up on one board.

**Not in this change:** phone inbox and pairing, spending cap enforcement, crash-safe restart reconciliation beyond the runner basics, coordinator, voice, website sign-ins, computer use, cloud runners, Mac/Linux installers, other-model workers, plugins, the per-app secrets vault (Credential Manager plus AWS references, hub-side MCP proxies, test-only per-run .env in tmpfs, gitleaks). Each is its own later PR.

## Decisions (owner, 2026-10-17)

- D1 Claude runs only through the official Claude Code CLI. Each app is assigned Max profile A or B. Decurion never switches profiles on its own.
- D2 When a profile runs out: offer extra usage or the pay-per-use API key (cap $100/month, key in AWS Secrets Manager `Decurion/AnthropicKey`). Never the other profile.
- D3 One Docker container per agent. Inside it the agent runs with no prompts.
- D4 PC sleeps: work pauses and resumes.
- D5 Phase 1 = board, agents in containers, phone inbox, crash-safe restarts, spending cap.
- D6 Voice never approves anything.
- D7 Seamless rule: safety comes from containers, scoped credentials, checks and checkpoints, never from new ask-the-user steps. Hard stops only: merge to main, production deploy or data, spend past cap, outbound messages, suspicious checks. One approval per goal.

## Technical calls (mine)

- Worker credential: only the Claude token for the app's profile enters the container. No GitHub, AWS or other keys. Push and merge run in the hub, outside the container, keyed by an operation id so a replay is a no-op.
- The host `~/.claude/.credentials.json` is never mounted: it also holds MCP OAuth tokens for other services, and sharing its refresh token would log out the host session. Production uses a per-profile `claude setup-token` token held in Windows Credential Manager.
- Hub: Rust, axum, SQLite via sqlx. Runner: Rust, drives Docker through the CLI first (bollard later if needed).
- The plan's Agent SDK worker is replaced by the CLI in stream-json mode (D1).

## Parked from review (later PRs)

- Pin the worker base image by digest and CI actions by commit SHA.
- Outbound network allowlist for containers.
- Hub-side push and merge: run only in a runner-owned bare repo (never in the agent's folder), bound to an approved commit.
- Durability (coordinator note): classify actions idempotent or effectful and reconcile effectful ones after a crash ("Interrupted, checking"); idempotency keys on messages and approvals; steer / follow_up / note messages to a running agent; parent tasks with depth-first cancel and workflow checkpoints; snapshot on reconnect; cost of failed and cancelled attempts. Already true here: every run event is saved to SQLite before the board shows it.
- An orphaned container keeps working while the runner is down; it is removed when the runner restarts. Stopping it sooner needs a runner watchdog.

## Slices for this PR

1. Windows spike (container, kill, resume, leak scan, push replay). Results in `docs/spikes/`.
2. Cargo workspace: `decurion-proto`, `decurion-hub`, `decurion-runner`. CI: fmt, clippy -D warnings, tests.
3. Hub: SQLite store for apps and threads with explicit state transitions, REST API.
4. Runner: start one Claude Code container per thread, stream events to the hub, mark threads Running / Done / Failed.
5. Minimal board page served by the hub.
