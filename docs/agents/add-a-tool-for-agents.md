# Add a tool for agents

## What agents get today

Claude Code boxes load exactly one MCP config, Decurion's own, baked into the
image: `docker/claude-worker/mcp.json`, copied to `/etc/decurion/mcp.json`
by `docker/claude-worker/Dockerfile`. It has one server, `browser`
(`@playwright/mcp`, pinned with its Playwright build, headless Chromium,
`--isolated`, `--no-webmcp`, going out through `--proxy-server http://gate:3128`).
The agent sees its tools as `mcp__browser__<function>`.

The runner starts Claude Code with `--mcp-config /etc/decurion/mcp.json --strict-mcp-config`
(`MCP_CONFIG`, `run_args` and `helper_run_args` in `crates/decurion-runner/src/lib.rs`).
`--strict-mcp-config` means only that file counts.

pi boxes get no MCP servers: `pi_run_args` passes `--no-extensions --no-skills --no-prompt-templates`,
and `docker/pi-worker/Dockerfile` makes pi's config folder (including an empty `mcp.json`) root-owned.

## Why a repo's own MCP servers never load

The code in `/work` is untrusted: an agent wrote it, or it came from the
owner's repo. A `.mcp.json` or plugin there could run any program inside the
box with the agent's credential, or add tools the owner never saw.
`--strict-mcp-config` ignores it. Helpers (coordinator, designer, checker)
also get `--setting-sources user`, so project settings and hooks in `/work`
cannot steer the agent that checks the work. Never remove these flags; the
test `token_is_passed_by_name_and_no_host_paths_are_mounted` asserts both
come after the image name. ADR 0001 lists "letting agents load MCP servers
from the repo" as rejected.

## Adding a built-in tool (today's route)

Only for tools that need **no secret** and reach only sites the box may
already reach. A tool that needs a key cannot go in the box: the box holds
only the run's one credential. That case needs the hub-side MCP proxy from
ADR 0001 (`placement = "hub"`), which is not built.

1. **Install it in the image**, pinned to an exact version, as root before `USER agent`, in `docker/claude-worker/Dockerfile`. No unpinned installs, no `latest`. Explain in a comment what it is for and why each flag is there, like the browser block.
2. **Add it to `docker/claude-worker/mcp.json`** as a `stdio` server. Keep anything it writes out of `/work` (the browser uses `--output-dir /tmp/playwright-mcp`) so it never lands in a commit. Its web traffic must go through the gate (`HTTPS_PROXY` is already set in the box by `box_args`; pass a proxy flag if the tool ignores it).
3. **Sites.** If it needs a site not in `sites::BUILT_IN` (`crates/decurion-proto/src/sites.rs`), it is blocked unless the owner adds that site to the app. Adding to `BUILT_IN` widens every box; that is the owner's decision, not yours.
4. **Tell the agent.** The built-in instructions are `RULES` in `crates/decurion-runner/src/lib.rs` (it already mentions the browser). Add one plain sentence.
5. **Helpers.** Every helper gets the same MCP config. To keep a tool from the coordinator, add `mcp__<name>__*` to `COORDINATOR_DISALLOWED` or `HELPER_DISALLOWED` in `crates/decurion-hub/src/coordinator.rs` (`valid_tool_name` accepts that pattern).
6. **Make it rebuild.** Installed copies rebuild the image only when the **Dockerfile's** hash changes (`prepare_image` in `packaging/unix/decurion-start`, `Initialize-Image` in `packaging/windows/start-decurion.ps1`). A change to `mcp.json` alone is not picked up: change the Dockerfile in the same commit (for example the pinned version or a comment). Bump `RULES_VERSION` (`crates/decurion-proto/src/lib.rs`) so a cloud worker on the old image is refused and rebuilds.
7. **Test.** Build the image (`docker build -t decurion/claude-worker:dev docker/claude-worker`) and check the server starts: `docker run --rm --entrypoint <server> decurion/claude-worker:dev --help`. Keep the runner tests green; add an assertion if you changed any argument.

Inside a Decurion box you cannot build images (no Docker there). Say in your
final summary that the image build was not tried.

## What will replace this

ADR 0001 `[[tool]]` add-ons: an MCP server in its own image pinned by digest,
either a sidecar in the box (no secrets) or beside the hub behind a per-run
MCP proxy that holds the key, with every exposed function declaring an effect.
See `write-an-add-on.md`.
