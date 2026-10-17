# Sealed workspace for one Claude Code agent.
# The agent sees only /work (its own copy of the code) and /home/agent.
FROM node:22-bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates ripgrep \
 && rm -rf /var/lib/apt/lists/*

ARG CLAUDE_CODE_VERSION=2.1.289
RUN npm install -g @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION} && npm cache clean --force

# The agent's browser: Chromium driven by the Playwright MCP server.
# Both versions are pinned exactly. @playwright/mcp pins its own Playwright
# exactly, and the browser is installed through that same copy of Playwright
# (`playwright-mcp install-browser` is Playwright's `install`), so the browser
# build always matches what the MCP server expects. The check below stops the
# build if the two pins ever drift apart.
# Browsers live in /ms-playwright, outside any home folder, readable by `agent`.
ARG PLAYWRIGHT_MCP_VERSION=0.0.83
ARG PLAYWRIGHT_VERSION=1.64.0-alpha-1790635538000
ENV PLAYWRIGHT_BROWSERS_PATH=/ms-playwright
RUN npm install -g @playwright/mcp@${PLAYWRIGHT_MCP_VERSION} \
 && found="$(node -p "require('$(npm root -g)/@playwright/mcp/node_modules/playwright-core/package.json').version")" \
 && if [ "$found" != "$PLAYWRIGHT_VERSION" ]; then \
      echo "@playwright/mcp ${PLAYWRIGHT_MCP_VERSION} uses Playwright $found, expected $PLAYWRIGHT_VERSION" >&2; exit 1; \
    fi \
 && playwright-mcp install-browser --with-deps chromium \
 && chmod -R a+rX /ms-playwright \
 && rm -rf /var/lib/apt/lists/* \
 && npm cache clean --force

# Decurion's MCP settings: only the browser. The runner passes
# `--mcp-config /etc/decurion/mcp.json --strict-mcp-config`, so MCP servers a
# repo brings along are never loaded.
# The browser runs headless and keeps its profile in memory (--isolated).
# Chromium's own sandbox is off (--no-sandbox) because the container runs with
# --cap-drop ALL and cannot start it; the container is the sandbox.
# Screenshots without a name go to /tmp, not /work, so they never end up in a commit.
# --no-webmcp: web pages cannot add their own tools to the agent.
COPY mcp.json /etc/decurion/mcp.json

# On Linux the agent must have the same user id as the runner, or neither can
# write the other's files in the shared work folder. The cloud worker builds
# with --build-arg AGENT_UID=<its runner's id>; Docker Desktop on Windows does
# not care, so the default is the image's next free id.
ARG AGENT_UID=
RUN useradd ${AGENT_UID:+--uid "$AGENT_UID" --non-unique} --create-home --shell /bin/bash agent
USER agent
WORKDIR /work
ENV CLAUDE_CONFIG_DIR=/home/agent/.claude
ENTRYPOINT ["claude"]
