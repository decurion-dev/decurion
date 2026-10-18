# Writing a Decurion add-on

> Add-ons version 1 is built: **tools for agents**, added from a link in Settings, **Add-ons**. Agent types and checks are still a design, in [ADR 0001](../adr/0001-add-on-system.md) ("What v1 ships" says exactly what runs today). This page is about what works now.

An add-on gives Decurion's agents an extra tool without changing Decurion. It is a git repository (or a folder in one) with a `decurion-addon.toml` manifest and the files it names. There is no store: the owner adds an add-on by pasting its link, and Decurion pins it to the commit it finds and to a hash of its files it works out itself.

The smallest working one is [`word-count/`](word-count/): a manifest and a small MCP server with no dependencies. Copy it.

## How a tool runs

- Your tool is an **MCP server that speaks HTTP** (streamable HTTP; plain JSON answers are fine). It listens on all of its box's addresses (`0.0.0.0`) at `/mcp`, on the port in the `DECURION_MCP_PORT` variable (also put in place of `{port}` in your command). The agent calls it by the name in `DECURION_MCP_HOST` (such as `addon-0`); refuse any other `Host` header, as the example does.
- It runs in a **sealed box of its own**, next to the agent's box, from Decurion's agent image (so `node` is there). Your add-on's files are at `/addon`, read-only, and that is the working folder. The root is read-only too; `/tmp` is writable and is `HOME`.
- The box sits on the run's own network as a box of its own, so the agent reaches your tool by its name, and your tool reaches the web only through the run's gate: the websites always allowed, the app's own, and the ones your manifest names. Nothing else. It never shares the agent's localhost, so it cannot reach the browser tools, the agent's database or anything else the agent runs there.
- It gets **none of the app's code**, no Claude sign-in, no home folder, and at most the one test key it names. Agents see your functions as `mcp__<tool name>__<function>`.
- Decurion waits up to 45 seconds for your port to answer. A tool that does not start is left out of that run, and the owner is told.

## The manifest

```toml
decurion_api = "1"

[addon]
id = "example.tickets"        # lowercase, dots and dashes, starts with something yours
name = "Tickets"              # shown to the owner
version = "0.3.0"             # MAJOR.MINOR.PATCH
description = "Lets agents look up tickets in Example Tracker."
authors = ["Example Co"]      # optional
license = "Apache-2.0"        # optional
homepage = "https://github.com/example/decurion-tickets"   # optional, https only

[[tool]]                      # one to four tools
name = "tickets"              # lowercase letters, digits, dashes; not browser or decurion
summary = "Search tickets in Example Tracker"
command = ["node", "server.mjs"]   # a program the box has, by plain name, then its arguments
effect = "read"               # "read" (only looks things up) or "write" (changes things)

[tool.permissions]
network = ["api.example-tracker.com"]   # up to 10 websites; each covers the addresses under it

[tool.secrets]
EXAMPLE_TRACKER_TOKEN = "EXAMPLE_TRACKER_TOKEN"   # variable in your box = one of the app's test keys
```

| Field | Required | Notes |
| --- | --- | --- |
| `decurion_api` | yes | `"1"` |
| `addon.id`, `addon.name`, `addon.version` | yes | |
| `addon.description`, `authors`, `license`, `homepage` | no | shown on the approval card |
| `tool.name`, `tool.summary`, `tool.command`, `tool.effect` | yes | `summary` is plain words for the owner and the agent |
| `[tool.permissions]` | no | only `network` (and `worktree = "none"`) |
| `[tool.secrets]` | no | at most one entry, naming one of the app's **test keys** |

Decurion refuses, in plain words:

- `[[agent]]` or `[[check]]` (not built yet), `placement = "hub"`, `transport = "stdio"`, `image = "..."`, `worktree` other than `"none"`;
- an `effect` that is a hard stop: `outbound_message`, `deploy`, `production_data` or `spend`, including inside `[[tool.expose]]`;
- a website known for sending messages or deploying (Slack, email services, Vercel and others: `HARD_STOP_SITES` in `crates/decurion-proto/src/addon.rs`), because each such call would need the owner's OK every time, which v1 cannot ask for;
- any unknown key inside `permissions`, `secrets` or `expose`, because Decurion will not show the owner a permission it cannot explain;
- a key name Decurion or the box uses (`ANTHROPIC_...`, `CLAUDE_...`, `PATH` and the like), and any Claude sign-in: those are never reachable.

Unknown keys elsewhere, `port` and `[tool.resources]` are ignored with a note on the card. `[[tool.expose]]` is read for its effects, but v1 gives agents every function your server offers.

Files: at most 1,000, 5 MB each, 20 MB in all; names of letters, digits and `. _ - + @`; no links. Everything in the add-on's folder counts toward its hash except a `.git` folder at its top.

## About keys

A tool that names a test key gets it in its own box, by name. **The agent's box gets the app's test keys too**, as it always does, and Decurion hides their values from what agents report and stops work whose commits contain one. So only use test keys: a key the agent must never see needs the hub-side tool and MCP proxy from ADR 0001, which is not built yet. The approval card says this in plain words.

## What owners see

One card, before anything is kept:

> **Add "Tickets" (version 0.3.0)?**
>
> It will be able to:
> - Gives your agents a tool: Search tickets in Example Tracker.
> - Talks to api.example-tracker.com. Your agents can reach that site too while they work.
> - Uses your saved test key EXAMPLE_TRACKER_TOKEN in each app it is on. Like every test key, your agents can use it too, and Decurion hides it from what they report.
> - Runs in a sealed box of its own beside your agents. It gets none of your app's code and never sees your Claude sign-in.
>
> It can never merge to main, deploy, touch production data, spend past your cap, send messages to people, or see your Claude sign-in.
>
> Use it in: (each app, with a switch)

Those sentences are Decurion's, fixed per permission; your `summary` goes inside them, never instead. After **Add**, it never asks again. **Check for an update** applies a new version at once when it asks for nothing more (no new tool, no tool that now writes, no new website, no new key); otherwise the owner sees one card listing only what is new, and your old version keeps running until they tap **Update**.

## Testing locally

1. Run your server by hand: `DECURION_MCP_PORT=8940 node server.mjs`, then post an `initialize` and a `tools/list` to `http://127.0.0.1:8940/mcp` with curl.
2. Commit it to a git repo. In Settings, **Add-ons**, paste the repo's full path on your PC (or its https link) and tap **Check it**. Fix anything the card refuses.
3. Tap **Add**, turn it on for a small app, and run a piece of work there. The work's activity says if your tool was left out and why; the **What add-ons did lately** list shows each use.

## Publishing

Push to a public git repo and share the link (and the folder, if the add-on is in one). Tag releases with semver. Owners see the commit Decurion pinned on the card. Signing is planned as optional and would add to the pin and hash, not replace them.

## What an add-on can never do

See any Claude login or the run's credential, switch an app's plan, merge, push, deploy, touch production data, spend past the cap, approve anything in the inbox, reach the Docker socket or the owner's files, see the app's code, or send messages to people. These are walls in the hub and runner, not promises in the add-on. Do not build add-ons that move health or personal data: Decurion does not handle PHI or PII, and does not check add-ons for it.
