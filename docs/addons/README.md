# Writing a Decurion add-on

> The add-on system is designed in [ADR 0001](../adr/0001-add-on-system.md) (status: Proposed) and ships in Phase 5. The `decurion addon` commands below are planned, not yet built. Expect small changes before release.

An add-on teaches Decurion something new without changing Decurion: a new **agent type**, an extra **tool** for agents, or an automatic **check**. It is a git repository (or a folder in one) with a `decurion-addon.toml` manifest and the files it names. There is no store: people install an add-on from its git URL, pinned to a commit and a hash.

## The three kinds

| Kind | What you ship | Runs as |
| --- | --- | --- |
| Agent type `[[agent]]` | A container image (by digest), a command, and an event adapter | A sealed container per piece of work, same walls as the Claude worker |
| Tool `[[tool]]` | An MCP server in a container image (by digest) | `placement = "hub"`: beside the hub, holds secrets, reached through Decurion's MCP proxy. `placement = "box"`: a sidecar inside the agent's sandbox, no secrets |
| Check `[[check]]` | A prebuilt WASM component (`.wasm`) | wasmtime, with fuel, memory and time limits, and only the host functions the owner granted |

Pick hub placement whenever your tool needs a key: the agent can call the tool but never sees the key. Pick box placement for tools that only need the code or public sites.

## Writing one

1. Create `decurion-addon.toml` at the add-on root. Start from the [commented example](example/decurion-addon.toml); delete the components you do not need.
2. **Tool:** build any MCP server, put it in an image, push it, and reference it by `@sha256:` digest. List every function the agent may call under `[[tool.expose]]` with an honest `effect`. Unlisted functions are hidden. `outbound_message`, `deploy`, `production_data` and `spend` make every call wait for the owner, so only use them where they are true, and always use them where they are.
3. **Check:** implement the `check` world from the `decurion:addon@1` WIT package (Rust: `cargo component build --release`; other languages: `wit-bindgen`). Import the `worktree` interface only if you need the code, and declare `worktree = "read"`. Return `deny(reason)` to refuse one tool call, `flag(reason)` to hold work for the owner, `pass` otherwise. A trap or timeout counts against you (deny or flag), so keep checks small and fast.
4. **Agent type:** make an image whose command reads the task on stdin and writes one JSON event per line on stdout. Use `builtin:claude-stream-json` if your output matches Claude Code's, otherwise write a `wasm:` adapter that exports `parse-line` and returns `run-event`s. If it spends money, set `cost = "reported"`, accept a budget through `budget_args`, and report the cost in the final event.
5. Commit the built `.wasm` files and put their SHA-256 in the manifest. Nothing runs at install time, so there are no build or install scripts.

## Manifest fields

| Field | Required | Notes |
| --- | --- | --- |
| `decurion_api` | yes | `"1"` |
| `addon.id` | yes | lowercase, dots and hyphens, prefixed with something yours (`example.tickets`) |
| `addon.name`, `addon.version` | yes | `version` is semver |
| `addon.description`, `authors`, `license`, `homepage` | recommended | shown on the install screen |
| `name`, `summary` | yes, per component | `summary` is plain words, shown to the owner |
| `image` | agent, tool | must be `...@sha256:<digest>` |
| `command`, `start_args`, `resume_args`, `budget_args`, `prompt`, `adapter`, `adapter_sha256`, `cost` | agent | templates may use `{session_id}`, `{budget_usd}`, `{model}` |
| `placement`, `transport`, `port`, `[[tool.expose]]` | tool | `expose` needs `name`, `effect`, `summary` |
| `module`, `sha256`, `hooks`, `[check.limits]` | check | hooks: `before-tool`, `after-run`, `redact-event` |
| `[*.secrets]` | optional | `ENV_VAR = "vault-name"`. Hub tools and agents only. Never put a secret value here |
| `[*.permissions]` | optional | `network = [hosts]`, `worktree` = `"none"`, `"read"` or `"write"` |
| `[*.resources]` | optional | `memory`, `cpus`, capped by the hub |

An unknown key inside `permissions`, `secrets` or `expose` refuses the install, because Decurion will not show the owner a permission it cannot explain.

## What owners see

Installing is one approval. Decurion turns your permissions into fixed plain sentences ("Talks to api.example-tracker.com", "Uses your saved key "example-tracker-token". Your agents never see it.") and lists what no add-on can ever do. After that it never asks again, unless an update adds or widens a permission; then the owner sees only the difference, and your old version keeps running until they approve. Ask for the least you need: every extra line is a reason to say no.

## What an add-on can never do

Use or see Claude logins (the `claude-profile-*` and `anthropic-api` vault names are reserved), switch an app's profile, merge, push, deploy, touch production data, spend past the cap, approve anything in the inbox, change the policy file, reach the Docker socket or host files, or send messages to people without the owner's OK. Do not build add-ons that move health or personal data: Decurion does not handle PHI or PII, and does not certify add-ons.

## Testing locally

```sh
decurion addon check ./my-addon            # validate manifest, hashes, WIT imports; print the install screen
decurion addon hash ./my-addon             # print the tree hash to publish
decurion addon add ./my-addon --dev        # install from a folder: no pin needed, re-read every run, "dev" badge
decurion addon test-check ./my-addon --check no-todo-left --diff change.patch
decurion addon test-check ./my-addon --check keep-out-of-billing --tool-call call.json
decurion addon test-adapter ./my-addon --agent tinycoder < sample-output.jsonl   # prints RunEvents
```

Then put a small app on the board, enable the add-on for it, and run a piece of work. Tool calls, check verdicts and secret use show up in the run's events and the audit log.

## Publishing

Push to a public git repo and tell people three things: the URL, the full commit hash, and the tree hash from `decurion addon hash`:

```sh
decurion addon add https://github.com/example/decurion-tickets --rev <commit> --sha256 <tree hash>
```

Tag releases with semver. Signing (minisign or sigstore) is planned as optional; it will add to the pin and hash, not replace them.
