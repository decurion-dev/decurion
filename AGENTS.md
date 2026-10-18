# AGENTS.md

You are working on **Decurion's own code**. Start here, then read the how-to
for your kind of task (index at the end) **before you change anything**.

## What Decurion is

- A hub that runs AI coding agents (Claude Code, and pi for ChatGPT and Grok) for an owner who is not a programmer.
- The owner gives a goal in plain words; a coordinator agent plans it; one approval starts the work.
- Each piece of work runs in its own sealed Docker box on its own branch; a checker agent reviews it.
- Everything shows on one board (PC browser) and one inbox (phone). Accepted work goes to GitHub as a branch and a pull request; the owner merges there, Decurion never does.
- Safety comes from walls (boxes, scoped credentials, checks), never from extra questions to the owner (PLAN.md D7).

## Map

| Path | Owns |
| --- | --- |
| `crates/decurion-proto` | Shared types. `ThreadState::can_move_to` (allowed moves), `RunEvent`, `Claim`, `RULES_VERSION` (`src/lib.rs`); website rules `sites::BUILT_IN`, `normalize`, `covers` (`src/sites.rs`); attached files `context::clean_name`, limits, `ContextRef` (`src/context.rs`); goal, plan, helper and review shapes (`src/goal.rs`). |
| `crates/decurion-hub` | axum server + SQLite (sqlx). `src/lib.rs` router, auth (`require_key`, `cloud_reach`), handlers. `src/store.rs` apps, threads, state moves with audit rows, claim, receipts. `src/coordinator.rs` goals, helper jobs, review rounds. `src/goals.rs` goal handlers. `src/spend.rs`, `limits.rs`, `usage.rs` money and Max meters. `src/messages.rs` owner-to-agent messages. `src/checkpoints.rs` base bundles and saved copies. `src/workers.rs` PC and cloud runners, moving work. `src/devices.rs`, `push.rs` phone pairing and Web Push; `src/tailscale.rs` "Connect your phone" (sets up and saves the hub's Tailscale https address). `src/secrets.rs` lends a run's credential to a cloud worker. `src/test_keys.rs`. `src/protected.rs` the protected-parts hold. `src/claude_sign_in.rs` "Sign in on this PC" for a Max plan (`claude setup-token` in a pseudo-terminal, saved in the vault, never logged). `src/sign_in_lights.rs` the Settings Sign-ins list (a light per account) and week-ahead reminders. `src/github.rs` sends accepted work to GitHub (branch, pull request, the owner's GitHub token setting). `src/pr_watch.rs` watches the pull requests it opened (merged, closed, review comments) and starts a fix on the owner's tap. `src/dismiss.rs` hides finished work and closed goals into a Dismissed view (never deletes). `src/context.rs` files the owner attaches for agents (stored per app, text taken out of Word, Excel and PowerPoint, the run's tar). Pages: `board.html`, `inbox.html`, `goals.html`, `cloud.html`, `confirm.html`, shared `ui.js`, `ui.css`, `files.js` (the attached-files panel). `migrations/`. |
| `crates/decurion-runner` | Claims work and runs boxes. `src/lib.rs` pure, tested pieces: `box_args`, `run_args`, `pi_run_args`, `helper_run_args`, `gate_args`, `Credential`, prompts, stream parsing, `PiRun`, redaction, `LeakScanner`. `src/main.rs` Docker, git, hub loop, `outcome`, helpers, gates. `src/gatekeeper.rs` host proxy holding the pay-per-use key. `src/pricing.rs`, `handover.rs` (moves), `identity.rs` (PC or cloud). |
| `crates/decurion-gate` | The proxy that is each box's only way out. `src/lib.rs` allowlist, `src/addr.rs` refuses private, home and Tailscale addresses, `src/head.rs` request parsing. |
| `crates/decurion-vault` | Test-key store (Windows Credential Manager, or a private folder), `check_name`; the owner's GitHub token (`put_github`, `check_github_token`), never handed to a run. |
| `docker/` | `claude-worker` (image + `mcp.json`), `pi-worker`, `gate`, `cloud` (cloud worker setup, `update.sh`, `walls.sh`). |
| `packaging/` | Windows installer (`windows/decurion.iss`, `start-decurion.ps1`), Mac and Linux (`unix/install.sh`, `decurion-start`), `winget/`. |
| `docs/` | `help/` owner help (mdBook), `adr/` decisions, `addons/` add-on design, `coordinator.md`, `spikes/`, `agents/` how-tos for you. |
| `spike/` | Small Python helpers (`wincred.py` saves tokens). |

## Build, lint, test

These are exactly what CI runs (`.github/workflows/ci.yml`, `docs-and-licenses.yml`). All must pass.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
bash docker/cloud/check-template.sh          # cloud.html copies of docker/cloud; --fix to resync
cargo test --workspace                       # CI runs this on Linux AND Windows
cargo deny check licenses bans sources       # CI; `cargo deny check` also runs advisories
mdbook build docs/help                       # mdBook 0.4.52, output in target/help-book
```

One test: `cargo test -p decurion-hub --test api <test_name>`, `cargo test -p decurion-runner --lib <test_name>`.
In a Decurion box, `cargo fmt`, `cargo clippy` and `cargo test` run as they are: the Claude worker image has a pinned Rust toolchain (rustfmt, clippy) and a C compiler. Run them before you finish. `check-template.sh` (needs Python), `cargo deny`, `mdbook` and Docker are not available in the box; say so in your summary instead.
Images: `docker build -t decurion/claude-worker:dev docker/claude-worker`, same for `pi-worker`;
gate: `docker build -t decurion/gate:dev -f docker/gate/Dockerfile .`.

## Conventions

- **Comments** are plain-word doc comments that say *why* and what must stay true, not what the next line does. Every module starts with a `//!` paragraph naming what it owns and its rules. Match that.
- **Owner-facing text** (errors from `StoreError::Invalid`/`Refused`, board labels, toasts, help pages) is for a non-programmer: short sentences, no jargon, say what happened and what to do next ("Remove spaces and other marks from the name."). See CONTRIBUTING.md "Rust style" and the pages in `docs/help/src/`.
- **Rust**: stable only; `unsafe` forbidden and `unwrap()` denied outside tests (`Cargo.toml` workspace lints); use `expect("why it cannot fail")`. `thiserror` in libraries, `anyhow` with context in binaries. Log with `tracing`; never log tokens, keys or test-key values.
- **Migrations**: `crates/decurion-hub/migrations/NNNN_name.sql`, embedded by `sqlx::migrate!` in `Store::open`. Numbers run 0001-0008, then 0040+ (spaced apart so parallel branches do not collide, see `0040_goals.sql`). Use the next free number above the highest file. Never edit or renumber a shipped migration. Start the file with a `--` comment saying why.
- **UI**: each page is one `.html` file in `crates/decurion-hub/src/`, served by `page(include_str!("x.html"))` in `lib.rs` with a strict CSP (no outside scripts). Shared code is `ui.js` (`window.D`: `D.api`, `D.el`, `D.toast`) and `ui.css`. Build DOM with `D.el`; text goes in as text, never as HTML.
- **State moves** go through `checked_move` in `store.rs`, which checks `can_move_to` and writes an audit row in the same transaction.
- **Owner taps** carry a `receipt`; the hub acts once and answers a resend with the first result (`receipt_result`, `save_receipt`).
- **Box or handover changes** bump `RULES_VERSION` in `crates/decurion-proto/src/lib.rs`.
- **Commits**: Conventional Commits; add a line under `[Unreleased]` in `CHANGELOG.md` when the owner will notice.

## Walls you must never loosen

Never weaken one of these to get a task done. If a task seems to need it, stop and say so in your final summary.

1. **Sealed box.** `box_args` (`crates/decurion-runner/src/lib.rs`): `--rm`, `--cap-drop ALL`, `--security-opt no-new-privileges`, `--memory 4g`, `--pids-limit 1024`, exactly two mounts (`/work`, the `/home/agent` volume), the box's own `--internal` network, all traffic through the gate. No Docker socket, no host paths, no `--privileged`, no host network. The gate itself runs `--read-only --cap-drop ALL --pull never` (`gate_args`). Tests: `token_is_passed_by_name_and_no_host_paths_are_mounted`, `boxes_sit_on_their_own_network_behind_the_gate`, `pi_runs_are_sealed_the_same_way_and_get_their_key_by_name`.
2. **Only Decurion's tools.** Claude boxes get `--mcp-config /etc/decurion/mcp.json --strict-mcp-config` (`run_args`, `helper_run_args`); helpers also get `--setting-sources user`; pi gets `--no-extensions --no-skills --no-prompt-templates` (`pi_run_args`). A repo's own MCP servers or add-ons never load.
3. **Website allowlist.** The gate (`crates/decurion-gate`) lets through only `sites::BUILT_IN` plus the app's owner-chosen sites (`POST /api/apps/{id}/sites`, owner-only), and never private, home or Tailscale addresses (`addr.rs`). Cloud worker: `docker/cloud/walls.sh`.
4. **Credentials by name only, one per run.** `Credential::vars()` and `-e NAME` (never `NAME=value`) in `box_args`. The pay-per-use Anthropic key never enters a box: the host gatekeeper swaps a per-run token (`gatekeeper.rs`). Test keys come from the vault, are hidden from output (`hide`), and a leaked one stops review (`leaked_keys` in runner `main.rs`). The host `~/.claude/.credentials.json` is never mounted (PLAN.md). Boxes never get the hub's owner or runner key (`lib.rs` header).
5. **Push only in the hub, never in a box, never to the default branch.** Boxes hold no GitHub key and are told not to push (`RULES` in runner `lib.rs`). The only push is in `crates/decurion-hub/src/github.rs`: after the owner taps Accept (or types a plain yes with Accept on screen), or after the checker passes a piece of a goal whose plan the owner approved and no protected part holds it (`finish_helper` in `coordinator.rs`), the hub pushes the accepted commit from the app's own folder with the owner's own git sign-in, plain (never forced), only to a new `decurion/...` branch (`check_branch`), never the default branch, with every prompt off. The one other push is a fix of review comments the owner tapped Fix these for (`crates/decurion-hub/src/pr_watch.rs`): once accepted, it goes to that same pull request's `decurion/...` branch (`check_fix_branch`) only as a plain fast-forward from the last commit Decurion pushed there, checked before the push. The GitHub token (Settings, or `gh auth token`) only opens the pull request, in a request header; never on a command line or in a log. Tests: `crates/decurion-hub/tests/github.rs`.
6. **Merge to main is always the owner's.** No code merges, on GitHub or anywhere: Decurion opens a pull request at most. Agents only return text that the hub checks against fixed shapes (`last_json_object` + `validate` in `coordinator.rs`); a runner cannot mark work Done (`runner_cannot_add_work_or_mark_done`). One approval per goal, bound to the exact plan shown (`approve_plan`; a plain yes counts only for the plan the owner's screen showed, `goal_say`); it covers the goal's pieces that pass their check, and nothing else.
7. **Spend cap.** Money is set aside before a paid run starts and unknown cost counts as spent (`spend.rs`); capped work moves to Needs you at claim (`ClaimOutcome.capped`, `store.rs`); per-run limits via `--max-budget-usd`, the gatekeeper's meter, and `over_budget` for pi. Nothing may raise or skip the cap.
8. **Hard stops** (PLAN.md D7): merge to main, production deploy or data, spend past cap, outbound messages, suspicious checks. Most have no code path at all today (there is no merge code, only a pull request for the owner); keep it that way unless the path stops for the owner every time. Do not add new ask-the-owner steps elsewhere.
9. **Claude only via the official Claude Code CLI** (`docker/claude-worker/Dockerfile` pins `@anthropic-ai/claude-code`). One Max profile per app (`App.profile`, `Account::for_work`), never switched automatically: running out waits or moves to pay-per-use only on the owner's tap (`limits.rs`), never to the other profile (PLAN.md D1, D2).
10. **No PHI or PII.** Decurion never handles health or personal data (SECURITY.md, ADR 0001 section 14).
11. **Runner keys reach little.** Cloud workers reach only the routes in `cloud_reach`; anything not listed, including new routes, is refused. Keep new routes owner-only (`forbidden`) unless a runner truly needs them.
12. **Protected parts.** `.decurion/protected-paths`, read from the commit the work started from, holds any piece that touches a listed file for the owner (`crates/decurion-hub/src/protected.rs`; see `docs/agents/improve-decurion-itself.md`). Never edit that file to finish a task.

## How-to index

Read the matching how-to before you start a task of that kind. They name the real files, functions and tests to copy from.

- [How a goal flows through Decurion](docs/agents/architecture.md)
- [Add a screen, a setting or an API route](docs/agents/add-a-screen-or-setting.md)
- [Add a new kind of agent (worker type)](docs/agents/add-a-worker-type.md)
- [Add a tool for agents](docs/agents/add-a-tool-for-agents.md)
- [Write an add-on (designed, not built yet)](docs/agents/write-an-add-on.md)
- [Fix a bug](docs/agents/fix-a-bug.md)
- [Improve Decurion itself](docs/agents/improve-decurion-itself.md)

Also: `CONTRIBUTING.md`, `PLAN.md`, `docs/coordinator.md`, `docs/adr/0001-add-on-system.md`.
