# Write an add-on

**Add-ons version 1 is built, for tools only.** The design is
[ADR 0001](../adr/0001-add-on-system.md); its "What v1 ships" section says
exactly what runs today. The author guide is
[`docs/addons/README.md`](../addons/README.md), and a working add-on to copy
is [`docs/addons/word-count/`](../addons/word-count/). The full design's
manifest, with agent types and checks that do not run yet, is
[`docs/addons/example/decurion-addon.toml`](../addons/example/decurion-addon.toml).

## Where the code is

| Piece | File |
| --- | --- |
| Shared shapes, tree hash, path and name rules, hard-stop websites | `crates/decurion-proto/src/addon.rs` (`AddonUse`, `ToolUse`, `Bundle`, `tree_hash`, `HARD_STOP_SITES`), `Claim.addons` |
| Reading a manifest, the card's fixed sentences, update diffs | `crates/decurion-hub/src/addon_manifest.rs` (`parse`, `can_lines`, `new_lines`, `NEVER`) |
| Fetch, stage, approve, per-app on and off, update, remove, the log, the run's download | `crates/decurion-hub/src/addons.rs`, migration `0120_addons.sql` |
| The Settings tab | `crates/decurion-hub/src/addons.js` (`window.DAddons`), one line each in `settings.js` |
| Tool box arguments, MCP config, the bundle check | `crates/decurion-runner/src/addons.rs` |
| Starting and stopping tool boxes in a run | `crates/decurion-runner/src/addon_box.rs`, called from `run` and `run_in_box` in runner `main.rs` |
| Tests | `crates/decurion-hub/tests/addons.rs`, and the unit tests in each file above |

## Rules to keep

- **No add-on code in the agent's box.** The agent's box holds the run's Claude credential. Tools run in boxes of their own (`tool_box_args`): no credential, no `/work`, no home volume, read-only root, only the add-on's checked files at `/addon`, and at most one test key by name.
- **Only Decurion's MCP config.** Tools reach the agent as a second value of the one `--mcp-config` (`add_to_run`), before `--strict-mcp-config`. Never add a path a repo controls.
- **Exactly the approved bytes.** The hub hashes the staged files at Add, its kept copy before every run's download, and the runner hashes what it got. Never skip one of the three.
- **Hard stops are refused, not half-supported.** Until the hub-side MCP proxy exists, an add-on whose tool sends messages, deploys, touches production data or spends money is refused at install, and so are websites in `HARD_STOP_SITES`. Do not add a way around that.
- **Owner only.** Every add-on route is owner-only except `GET /api/runner/threads/{id}/addons/{addon}`, which needs the runner holding that work. Do not add add-on routes to `cloud_reach`.
- **Everything goes in `addon_log`**, which is append-only.
- **Box changes bump `RULES_VERSION`.**

## Not built yet

Agent types, checks (wasmtime), `placement = "hub"` with the MCP proxy (a key agents never see, per-function filtering, per-call hard stops), tools in images of their own, stdio tools, tool access to the app's code, a `decurion addon` command line, dev mode, signing. Each is refused today with a plain message. If you are asked to build one, read the ADR's design and open questions first and keep to them.

## What an add-on can never do

See ADR 0001 section 3 "Never grantable": use or see Claude logins (vault
names `claude-profile-*` and `anthropic-api` are reserved, and a test key's
name can never start with `CLAUDE_` or `ANTHROPIC_`), switch an app's
profile or provider, get the hub's GitHub, cloud or push credentials, reach
the Docker socket or host paths, merge, push, deploy, touch production data,
raise or skip the spending cap, approve inbox items, edit the policy file, or
send messages without a hard stop. Add-ons do not change the rule that
Decurion never handles PHI or PII. These are enforced by the hub and runner
having no path that hands them over, not by trusting add-on code.
