# Write an add-on

**The add-on system is designed but not built.** The design is
[ADR 0001](../adr/0001-add-on-system.md) (status **Proposed**), with an
author guide in [`docs/addons/README.md`](../addons/README.md) and a
commented manifest in
[`docs/addons/example/decurion-addon.toml`](../addons/example/decurion-addon.toml).
There is no `addons` table, no `decurion addon` command, no wasmtime and no
MCP proxy in the code today. Do not write code that assumes they exist, and
do not tell the owner an add-on can be installed.

**Until it is built, a new capability goes in as a normal change to
Decurion**, through the self-improvement flow
([`improve-decurion-itself.md`](improve-decurion-itself.md)): a new agent kind
per [`add-a-worker-type.md`](add-a-worker-type.md), a built-in tool per
[`add-a-tool-for-agents.md`](add-a-tool-for-agents.md). If you are asked to
build the add-on system itself, that is Phase 5 work: read the whole ADR,
its "Depends on" notes and its open questions first, and keep to it.

## The design in short

- **Package:** a git repo (or folder) with `decurion-addon.toml` (`decurion_api = "1"`, `[addon]` id, name, semver version) and the files it names. Installed from a git URL pinned to a full commit plus a SHA-256 tree hash. No store, no install scripts, nothing runs at install.
- **Three kinds:**
  - `[[agent]]` a new kind of agent: an image by `@sha256:` digest, a command, start, resume and budget argument templates, and an event adapter (`builtin:claude-stream-json` or a WASM `parse-line`). Runs with exactly the Claude worker's hardening; it cannot add mounts, devices, capabilities or networks.
  - `[[tool]]` an MCP server in its own image. `placement = "box"`: a sidecar sharing the run's allowlist, no secrets. `placement = "hub"`: beside the hub, holds its declared secrets, reached only through a per-run MCP proxy. Every exposed function lists an `effect`: `read`, `write`, or a hard stop (`outbound_message`, `deploy`, `production_data`, `spend`) that waits for the owner on every call.
  - `[[check]]` a WASM component in wasmtime with fuel, memory and time limits and only the host functions granted. Hooks `before-tool`, `after-run`, `redact-event`. Verdicts can only tighten (`pass`, `deny`, `flag`); a trap counts as deny or flag.
- **Permissions** are a closed list (`network`, `worktree`, `secrets`, hooks, effects), each shown to the owner as one fixed sentence. One approval to install; an update that widens anything needs a new approval and the old version keeps running meanwhile. An unknown key in `permissions`, `secrets` or `expose` refuses the install.

## What an add-on can never do

See ADR 0001 section 3 "Never grantable": use or see Claude logins (vault
names `claude-profile-*` and `anthropic-api` are reserved), switch an app's
profile or provider, get the hub's GitHub, cloud or push credentials, reach
the Docker socket or host paths, merge, push, deploy, touch production data,
raise or skip the spending cap, approve inbox items, edit the policy file, or
send messages without a hard stop. Add-ons do not change the rule that
Decurion never handles PHI or PII. These are enforced by the hub and runner
having no path that hands them over, not by trusting add-on code.

## What is not possible even in the design (v1)

A store; automatic updates; board panels or any add-on UI; add-ons that
change workflows, policy or board states; add-ons depending on other add-ons;
tools running directly on the host; computer-use or browser add-ons; paid
add-ons; installs by anyone but the owner (ADR 0001 "Out of scope for v1").
