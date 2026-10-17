# ADR 0001: The add-on system

- **Status:** Proposed
- **Date:** 2026-10-17
- **Deciders:** the owner (final say), maintainers
- **Phase:** 5 (public release). Several integration points depend on Phase 2, 3 and 4 work and are marked **Depends on**.

## In plain words

An add-on is a small package that teaches Decurion something new without changing Decurion itself. With add-ons you can:

- **add a new kind of agent**, for example a coding tool that runs on a different AI model;
- **give your agents an extra tool**, for example "read and update tickets in our tracker";
- **add an automatic check**, for example "stop the agent from touching the billing folder" or "flag work that leaves TODO notes behind".

**Installing one looks like this.** You paste the add-on's link. Decurion shows one screen, in plain words, listing what it will be able to do and what it never can:

> **Add "Tickets" by Example Co (version 0.3.0)?**
>
> It will be able to:
> - Give your agents a tool to search and update tickets
> - Use your saved key "example-tracker-token". Your agents never see the key.
> - Talk to api.example-tracker.com
> - Comment on tickets. Each comment waits for your OK in Needs you, because it notifies people.
>
> It can never: merge to main, deploy, touch production data, spend past your cap, send messages without your OK, or see your Claude login.
>
> Used in: all apps (change)
>
> **Add** · Cancel

You tap Add once. After that it never asks again, unless an update wants to do more than you agreed to. Then you see one screen listing only what changed, and the old version keeps working until you decide.

**Why it is safe.** An add-on gets walls, not trust:

- Every agent and every tool runs in its own sealed box, the same kind your agents already use. It cannot see your PC, your other apps, or your logins.
- A tool that needs a password or key runs outside the agent's box and holds the key itself. The agent can use the tool but can never read the key.
- Checks run in an even tighter box that has no files, no internet and no clock unless you granted them. Checks can only make things stricter. A check can stop an action or flag it for you; it can never approve anything.
- The rules that always stop for you stay in place for every add-on: merging to main, deploying, production data, spending past your cap, sending messages to people, and anything a check finds suspicious.
- Each add-on is pinned to an exact version. If even one byte changes, Decurion refuses to run it.
- Everything an add-on is allowed, and every time it uses a key, is written to the log.

There is no add-on store at first. You add an add-on by its link, the same way you add an app.

## Context

The product plan promises "you can add your own agent types and extra tools later without changing Decurion itself" and names the technology: **WASM plus MCP** (wasmtime, MCP servers). Phase 5 ships the add-on system with the public release.

What exists today (Phase 1 code):

- `decurion-runner` starts one container per thread with `docker run -i --rm --cap-drop ALL --security-opt no-new-privileges --memory 4g --pids-limit 1024`, a bind mount of the thread's own clone at `/work`, a per-thread home volume, and exactly one credential passed by name (`CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY`). The prompt goes in on stdin.
- The runner parses Claude Code `--output-format stream-json` lines into `decurion_proto::RunEvent` (`Started`, `Message`, `Tool`, `Finished` with cost and tokens) and posts them to the hub, which stores every event in SQLite before the board shows it.
- The hub keeps an append-only `audit` table (per thread state moves), reserves pay-per-use budget per run, and moves work to Needs you at the monthly cap.
- Push and merge run in the hub, never in the agent's box.

Owner decisions that bind this design (PLAN.md D1 to D7):

- **Seamless rule (D7).** Safety comes from walls (sealed containers, scoped credentials, checks, checkpoints), never from new ask-the-user steps. Hard stops only: merge to main, production deploy or data, spend past cap, outbound messages, suspicious checks.
- **Claude only through the official Claude Code CLI (D1), one Max profile per app, never switched automatically (D2).** Never hold subscription tokens beyond that.
- **Decurion never handles PHI or PII.**

Work happening in parallel that add-ons must slot into, without this ADR inventing their APIs: the per-box website allowlist, the test-key vault, crash-safety extras (steer, follow-up and note messages; task tree; effectful vs idempotent actions), Phase 2 (one policy engine, `decurion-policy`, for PC and cloud), Phase 3 (accounts, ChatGPT and Grok through a pi worker), and Phase 4 (the coordinator).

## Decision

### 1. Three kinds of component

An add-on is a git repository (or a folder in one) containing a manifest, `decurion-addon.toml`, and the files it names. It declares one or more components of three kinds. Board panels are a possible fourth kind later (see Out of scope).

**Agent types** (`[[agent]]`): a new kind of worker.

- An OCI image pinned by digest, a command, argument templates for start, resume and budget (`{session_id}`, `{budget_usd}`, `{model}`), and how the prompt is delivered (`stdin` in v1, matching today's runner).
- An **event adapter** that turns the agent's output lines into `RunEvent`s: either a built-in adapter (`builtin:claude-stream-json` today; `builtin:acp` once Phase 3 settles the worker protocol) or a WASM component exporting `parse-line`. Adapters are pure functions with no host imports.
- The runner starts it with exactly the same hardening as the Claude worker (`run_args`): same flags, `/work` plus the home volume and nothing else. The manifest cannot add mounts, devices, capabilities, `--privileged`, host networking or the Docker socket. It may ask for memory or CPU up to a hub-wide ceiling.
- **Depends on Phase 3:** the pi worker for ChatGPT and Grok is expected to be packaged as an agent-type add-on (first-party, shipped with Decurion), which is the best test that this kind is general enough.

**Tools** (`[[tool]]`): an MCP server, always in its own container image pinned by digest, never added to the agent's image. Two placements:

- `placement = "box"`: a sidecar in the run's sandbox, sharing the run's network namespace and therefore its website allowlist. It reaches the agent over MCP streamable HTTP on localhost. It may see `/work` (`none`, `read` or `write`, as declared). **It gets no secrets.** Anything it can reach, the agent can reach too, and the install screen says so.
- `placement = "hub"`: runs next to the hub (in its own container, not in the hub process), holds the secrets it declared, and is reachable only through the hub's **MCP proxy**. The proxy gives each run a URL and a bearer token valid for that run only, forwards only the MCP tools listed in the manifest (`[[tool.expose]]`), runs every call through policy and checks, logs it, and revokes the token when the run ends. The proxy speaks stdio or HTTP to the server; the server's own egress is limited to its declared hosts.

For the Claude worker, the runner writes an MCP config into the box and starts Claude Code with `--mcp-config <file> --strict-mcp-config`, so only the servers Decurion configured load, never a repo's own `.mcp.json`. Tools appear to the agent as `mcp__<tool name>__<function>`, which the existing `RunEvent::Tool` already records. (To verify against the pinned CLI version before shipping.)

Every exposed function declares an **effect**: `read`, `write`, `outbound_message`, `deploy`, `production_data` or `spend`. The last four are hard stops: each call goes to Needs you, every time, whatever the policy file says. A function the server offers but the manifest does not list is hidden from the agent. Effects are the author's claim; see Open questions.

**Checks** (`[[check]]`): WASM components run by wasmtime.

- Hook points: `before-tool` (sees a proposed tool call, returns pass, deny with a reason the agent sees, or flag), `after-run` (sees the run's diff and summary before it can move to In review), and `redact-event` (returns spans to mask in event text before it is stored).
- Verdicts can only tighten. `deny` refuses that one call. `flag` is the "suspicious check" hard stop: the work moves to Needs you with the check's reason. No verdict means "allow"; checks cannot approve inbox items, change policy or skip other checks. `redact-event` returns spans, not new text, so it can only hide, never rewrite what an agent said.
- No ambient access. The component gets no WASI filesystem, sockets, environment or wall clock. The linker defines only the host functions the owner granted: `log` always; `read-diff` and `read-file` (the thread's worktree, read-only) only with `worktree = "read"`. A component that imports an ungranted function fails to instantiate, so the grant check is structural, done at install and again at load.
- Limits per call: wasmtime fuel (default 50 million, author may lower, hub-wide ceiling), memory via `StoreLimits` (default 64 MiB), and an epoch deadline (default 2 s). A trap, timeout or limit hit counts as `deny` for `before-tool` and `flag` for `after-run`: a broken check never silently passes work. The board shows the check as broken with a one-tap disable.
- **Depends on Phase 2:** `before-tool` needs the tool-call interception that `decurion-policy` builds (a Claude Code PreToolUse hook in the box reporting to the policy engine). Checks run wherever that engine runs; the WASM bytes are the same on PC and cloud. `after-run` and `redact-event` can ship on Phase 1 plumbing (the runner already counts new commits before In review; the hub already stores events).

### 2. Manifest

TOML, at the add-on root, named `decurion-addon.toml`. A full commented example is in `docs/addons/example/decurion-addon.toml`. Summary:

| Field | Meaning |
| --- | --- |
| `decurion_api` | Major API version, `"1"`. Required. |
| `[addon]` `id` | Lowercase, dots and hyphens, author-prefixed (`example.tickets`). Unique per hub. |
| `name`, `version`, `description`, `authors`, `license`, `homepage` | `version` is semver. The rest is shown on the install screen. |
| `[[agent]]` | `name`, `summary`, `image` (by digest), `command`, `start_args`, `resume_args`, `budget_args`, `prompt`, `adapter`, `cost`, `[agent.secrets]`, `[agent.permissions]`, `[agent.resources]`. |
| `[[tool]]` | `name`, `summary`, `placement`, `image`, `command`, `transport`, `port`, `[[tool.expose]]` (`name`, `effect`, `summary`), `[tool.secrets]`, `[tool.permissions]`, `[tool.resources]`. |
| `[[check]]` | `name`, `summary`, `module`, `sha256`, `hooks`, `[check.permissions]`, `[check.limits]`. |

Every file the manifest names carries a `sha256`; every image is referenced by `@sha256:` digest. Secrets are declared as `ENV_VAR = "vault-name"`; the add-on names a secret, it never contains one.

Parsing is strict where it matters: an unknown key inside any `permissions`, `secrets` or `expose` table refuses the install, because Decurion cannot show the owner a permission it does not understand. Unknown keys elsewhere are a warning.

### 2a. WASM interface (sketch)

The shape is decided here; exact record fields are settled in the implementation PR and frozen as `decurion:addon@1.0.0`.

```wit
package decurion:addon@1.0.0;

interface types {
  record tool-call { tool: string, summary: string, input-json: string }
  record run-summary { thread-id: s64, ok: bool, summary: string, commits: u32 }
  variant verdict { pass, deny(string), flag(string) }
  record span { start: u32, end: u32 }
  // Mirrors decurion_proto::RunEvent.
  variant run-event { started, message(string), tool(tuple<string, string>), finished(finished) }
  record finished { ok: bool, summary: string, cost-usd: option<f64>,
                    input-tokens: option<u64>, output-tokens: option<u64> }
}

interface log { log: func(level: u8, msg: string); }
interface worktree {            // linked only with worktree = "read"
  read-diff: func() -> result<string, string>;
  read-file: func(path: string) -> result<list<u8>, string>;
}

world check {
  use types.{tool-call, run-summary, verdict, span};
  import log;
  export before-tool: func(call: tool-call) -> verdict;
  export after-run: func(run: run-summary) -> verdict;
  export redact-event: func(text: string) -> list<span>;
}
world check-with-worktree { include check; import worktree; }

world adapter {                 // no imports at all
  use types.{run-event};
  export parse-line: func(line: string) -> list<run-event>;
}
```

The author SDK (Rust first, via `cargo component`; others through `wit-bindgen`) supplies defaults that return `pass` or no spans for hooks the check does not use. Decurion calls only the hooks the manifest lists.

### 3. Permissions and how they read

Permissions are a closed list. Each maps to one fixed sentence on the install screen; the author's own `summary` is shown beside it but never replaces it.

| Permission | Install screen says |
| --- | --- |
| agent type (any) | "Adds a new kind of agent: {summary}. It works on its own copy of your code." |
| agent `cost = "reported"` | "Can spend money on {hosts}. It counts toward your monthly cap." |
| `secrets` on a hub tool or agent | "Uses your saved key "{name}". Your agents never see it." (agent type: "The {name} agent uses your saved key "{name}" inside its own box.") |
| `network = [...]` | "Talks to {hosts}." For box tools add: "Your agents can reach these sites too." |
| `worktree = "read"` | "Can read your app's code." |
| `worktree = "write"` (box tool) | "Can change your app's code in the agent's copy." |
| `before-tool` hook | "Sees every action your agents take, and can stop one." |
| `after-run` hook | "Reviews finished work, and can hold it for you." |
| `redact-event` hook | "Can hide parts of what agents report." |
| effect `write` | "Can change things in {hosts}." |
| hard-stop effect | "{function summary}. Each one waits for your OK in Needs you." |

**Never grantable**, and listed on every install screen as "It can never": Claude profile tokens or any Max login, the hub's GitHub, cloud or push credentials, the Docker socket or host paths, merge, push, deploy, production data, raising or bypassing the spending cap, approving inbox items, editing the policy file, switching an app's profile or provider, or sending messages without a hard stop. These are not checks inside add-on code: the runner and hub simply have no path that hands them over.

Vault names `claude-profile-*` and `anthropic-api` are reserved. Only Decurion's built-in Claude agent type uses them, which keeps D1 and D2 true: an add-on cannot run Claude another way, and cannot move an app to another profile.

### 4. Distribution and install

- **Source:** a git URL, pinned to a full commit hash, plus the add-on's **tree hash**: SHA-256 over the sorted list of `path NUL sha256(file)` lines for the add-on directory at that commit. Decurion computes it itself; it does not trust git's SHA-1. Authors publish both, e.g. `decurion addon add https://github.com/example/tickets --rev 3f9c... --sha256 9a1e...`.
- **No code runs at install.** No build steps, no install scripts. WASM modules are prebuilt and committed; images are pulled by digest.
- **No central store in v1.** Signing is planned as optional and additive (minisign or sigstore, see Open questions); a pin plus hash already gives "exactly these bytes".
- **Install** is one owner approval, from the board or phone, of the screen in "In plain words". It records the app scope (all apps, or chosen ones). Only the owner can install; **Depends on Phase 3** accounts for who else may.
- **Dev mode** (`--dev`, local path): no pin or hash needed, re-read before each run, shown with a "dev" badge, this hub only, never synced to cloud runners. Permission changes still need re-approval, so authors see what their users will see.

### 5. Versioning and compatibility

- `decurion_api = "1"` is the contract: manifest schema, permission vocabulary, WIT package `decurion:addon@1.x`, the MCP proxy behavior, and the `RunEvent` shape adapters produce. Within major 1, changes are additive only.
- A hub states the majors it supports and refuses others with a plain message ("This add-on needs a newer Decurion").
- WIT minor versions are checked by wasmtime's component linker; a component built against a newer minor that imports something this hub lacks fails at install, not mid-run.
- Each run records the add-on id, version and tree hash it started with. A run finishes on the version it started on.

### 6. Updates

- Never automatic in v1. The board may show "update available" when the owner asks it to look; applying one is `update` with a new rev and hash.
- Decurion diffs the permissions. **Same or fewer:** applied with no prompt and logged. **Anything new or broader** (a host, a secret, an effect, a hook, worktree access, a new component): one approval showing only the difference. Until approved, the old version keeps running.
- Rollback is installing the previous pin; it is never broader than what was approved before.

### 7. Uninstall

`remove` stops new runs from using the add-on at once. Runs in flight finish on their pinned copy, unless the owner chooses "stop now", in which case those threads move back to Queued and, if their agent type is gone, to Needs you. Files and cached images are deleted. Vault entries are the owner's and stay, marked "no longer used". Audit rows stay.

### 8. Where add-ons live

The hub owns the record of truth: a new SQLite table `addons` (id, version, source URL, commit, tree hash, approved permissions as JSON, app scope, approved by, approved at, enabled). Bundles are stored read-only under `<data dir>/addons/<id>/<tree hash>/`. Runners fetch bundles from the hub by tree hash and verify it before use, so a cloud runner runs the same bytes. **Depends on Phase 2** for cloud runners and bundle sync.

### 9. Credentials

Add-ons name secrets; Decurion resolves them from the vault **in the scope of the app the run belongs to** and injects them as environment variables into that one container at start, never into the agent's box for hub tools. Values never appear in manifests, command lines, logs or events (the existing redaction applies to add-on output too). **Depends on the vault work** (per-app secrets, test keys in a tmpfs `.env`): this ADR fixes only "by name, scoped per app, read at container start"; the vault's naming and fallback rules are its own.

### 10. Spending

An agent type declares `cost = "none"` (local or free model) or `cost = "reported"`. For `reported`, the hub reserves budget exactly as it does for pay-per-use Claude runs, passes it through `budget_args`, and counts the adapter's `Finished.cost_usd` against the monthly cap. As with the Anthropic key today, the owner must also set a provider-side limit, because the agent holds the key while it runs. A tool function with effect `spend` is a hard stop on every call. Nothing an add-on reports can raise the cap.

### 11. Failure isolation

- Every agent and tool is its own container with its own memory and process limits. A crashed box tool makes that tool's calls fail; the agent carries on. A crashed hub tool cannot take down the hub.
- Checks and adapters run in wasmtime stores with fuel, memory and epoch limits; a trap is an error value, not a hub crash. An adapter that fails on a line drops that line and counts it; too many failures fail the run with a clear reason.
- An add-on failing repeatedly is shown as unhealthy on the board. Tools and agent types are not auto-removed; checks are never auto-disabled, because that would quietly loosen safety.
- **Depends on crash-safety work:** hub-tool calls with effect `write` or a hard-stop effect are recorded before they are forwarded and carry an idempotency key, so after a crash they are reconciled ("Interrupted, checking") instead of repeated.

### 12. Audit log

New append-only entries, written by the hub: `addon.installed`, `addon.updated` (with the permission diff), `addon.approved`, `addon.removed`, `addon.enabled` and `addon.disabled` (per app), `addon.secret_used` (name only, which container, which run), `addon.tool_call` (hub tools: function, effect, a redacted argument summary, result size, outcome), `addon.check_verdict` (deny and flag only; passes are counted, not logged), and `addon.failure`. Today's `audit` table requires a `thread_id` and records only state moves, so this needs a schema change (a general audit table, or a sibling). That change should be made once, with the policy engine's needs in mind.

### 13. Fit with the parallel work

| Piece | How add-ons use it |
| --- | --- |
| Website allowlist | Box tools share the run's allowlist; their declared hosts are added only for runs where the tool is enabled. Hub tools get their own allowlist of exactly their declared hosts. |
| Policy engine (Phase 2) | Every add-on tool call is a policy request, named `addon:<id>/<tool>/<function>`; effects map to policy categories; checks run in the engine's pipeline after the policy file. |
| Test-key vault | Agent types and box tools may receive test keys the same way the Claude worker does, under the vault's rules. |
| Steer, follow-up, note | Agent types opt in by declaring support in a later minor of the adapter world; until then those messages queue for the next run. |
| pi worker (Phase 3) | Shipped as a first-party agent-type add-on. |
| Coordinator (Phase 4) | Can choose among installed agent types and tools for an app; it cannot install, update or approve add-ons. |

### 14. PHI and PII

Add-ons do not change the rule that Decurion never handles PHI or PII. The manifest has no way to declare data handling, and Decurion does not certify add-ons. Tool results enter the agent's context and the event log through the same redaction pipeline as everything else; `redact-event` checks can add to it, never remove from it.

## Consequences

**Good**

- New agents and tools need no Decurion release, and the first-party pi worker proves the path.
- One approval per add-on, then silence, which keeps the seamless rule. Safety is in the walls: containers, scoped secrets, structural WASM grants, hard stops in the hub.
- The existing MCP ecosystem works with a container and a manifest. Checks are cheap enough to run on every tool call.
- Hub-placed tools finally give a clean answer for "the agent needs a service that needs a key": the agent never holds it.

**Costs and risks**

- Two runtimes (containers and wasmtime) to maintain and secure. wasmtime adds a sizable dependency to the hub and runner.
- Effects are self-declared. A malicious or careless tool can label a messaging call `write`. Mitigations are the install review, pinning, egress limits, and logging; they are not proof.
- Box tools widen what the agent can reach (their hosts join the box's allowlist). The install screen says so, but owners may skim it.
- Strict hashing makes "just try the latest" harder than a store would. That is intended for v1.
- The audit schema change and the PreToolUse interception are prerequisites shared with Phase 2; the add-on work cannot finish before them.

## Alternatives considered

- **Native plugins (Rust dynamic libraries).** Fast and flexible, but full process access, no isolation and no stable ABI. Rejected.
- **Embedded scripting (Lua, or JavaScript via deno_core).** Easier for authors, but ambient access must be carved away by hand and the language is fixed. WASM gives any language and deny-by-default imports.
- **MCP only.** Covers tools, but a container per check per tool call is too slow, and checks do not need processes or networks. Rejected for checks.
- **WASM only.** Cannot reuse the existing MCP servers, and agent types need full containers anyway. Rejected.
- **WASI with filesystem preopens for checks.** Simpler for authors, but grants more than "read the diff". Revisit as a read-only preopen of the worktree if `read-file` proves awkward.
- **Letting agents load MCP servers from the repo or Claude Code's own plugins.** Unpinned, unreviewed, invisible to Decurion. Blocked with `--strict-mcp-config`.
- **A per-use approval for each tool call.** Violates the seamless rule. Hard-stop effects are the only per-call stop.
- **A central store from day one.** Moderation burden and a supply-chain target before there are users. Later, maybe.

## Out of scope for v1

A store or registry; automatic updates; board panels or any add-on UI; add-ons that change workflows, policy or the board's states; add-ons depending on other add-ons; tools running directly on the host (no container); computer-use or browser add-ons (the agent browser stays built in); paid add-ons; sharing add-ons between hubs; key-value state for checks; installs by anyone but the owner.

## Open questions

1. **Trusting effects.** Can the hub verify a tool's declared effects at all? Options: a first-party allowlist of known messaging and deploy hosts that forces hard-stop effects for tools reaching them; only allowing `write` for tools whose hosts the owner already trusts. Needs a decision before release.
2. **Signing.** minisign (simple keys, trust on first use per author) or sigstore keyless (identity-bound, needs a transparency log and network at install). Either is additive to pin plus hash.
3. **Spend for non-Claude agents.** `RunEvent` has no running-cost event, so the hub learns cost only at `Finished`. A `Usage` event in a later minor would let the runner stop a run at its reservation instead of relying on the agent and the provider-side limit.
4. **Windows.** Box sidecars need a shared network namespace; confirm on Docker Desktop with WSL 2 alongside the Windows spike. Hub tools in containers on the owner's PC also need Docker running whenever the hub does.
5. **Default app scope.** "All apps" for checks seems right; for tools that reach outside services, "pick apps" may be the safer default even if it adds a choice on the one screen.
6. **Where checks run** once there are cloud runners: in the hub (one place, more latency) or on each runner (same bytes, verified by hash). Decided with `decurion-policy`.
7. **CLI flags** (`--mcp-config`, `--strict-mcp-config`) behavior in `-p` stream-json mode must be checked against the pinned Claude Code version.
