# Sealed workspace for one Claude Code agent.
# The agent sees only /work (its own copy of the code) and /home/agent, and
# reaches only the websites its gate lets through.
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

# Rust, so agents can build and test Rust apps (Decurion itself included) and
# run the same checks as its CI: cargo fmt, cargo clippy, cargo test.
# gcc and libc6-dev are the C compiler and linker Rust needs, and build the C
# parts of some crates (SQLite, ring); pkg-config lets crates find system
# libraries. wget is only for the download and is removed again.
# rustup-init is pinned and checked against the sha256 published next to it
# for this version (static.rust-lang.org/rustup/archive/<version>/<target>/
# rustup-init.sha256); rustup then installs the pinned toolchain and checks
# each download itself. Only the minimal set plus rustfmt and clippy.
# CI uses the newest stable Rust; move RUST_VERSION along with it, or the
# agent's clippy may disagree with CI's.
# The toolchain lives in /usr/local, owned by root and only readable by
# `agent`, so an agent cannot change its own compiler. Crates the agent
# downloads go to its home volume (CARGO_HOME, set below), so they are kept
# between runs of the same piece of work. The box reaches crates.io through
# its gate; it cannot reach static.rust-lang.org, so a repo that asks for
# another Rust version (rust-toolchain.toml) gets none.
ARG RUSTUP_VERSION=1.29.1
ARG RUST_VERSION=1.99.1
ENV RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:$PATH
RUN apt-get update \
 && apt-get install -y --no-install-recommends gcc libc6-dev pkg-config wget \
 && case "$(dpkg --print-architecture)" in \
      amd64) target=x86_64-unknown-linux-gnu; \
             sum=dda7234360b7f578ca8b0ddcb80145646fa61a67c1720a5abc7051b35c9fcb71 ;; \
      arm64) target=aarch64-unknown-linux-gnu; \
             sum=15f6e4ce9f583b929c996c91562bad6d4454f3281de858b02cdfdef615fac433 ;; \
      *) echo "no pinned rustup-init for $(dpkg --print-architecture)" >&2; exit 1 ;; \
    esac \
 && wget -q -O /tmp/rustup-init "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${target}/rustup-init" \
 && echo "${sum}  /tmp/rustup-init" | sha256sum -c - \
 && chmod +x /tmp/rustup-init \
 && CARGO_HOME=/usr/local/cargo /tmp/rustup-init -y --no-modify-path --profile minimal \
      --default-toolchain "${RUST_VERSION}" --component rustfmt --component clippy \
 && rm /tmp/rustup-init \
 && chmod -R a+rX,go-w /usr/local/rustup /usr/local/cargo \
 && apt-get purge -y --auto-remove wget \
 && rm -rf /var/lib/apt/lists/* \
 && CARGO_HOME=/usr/local/cargo rustc --version \
 && CARGO_HOME=/usr/local/cargo cargo --version \
 && CARGO_HOME=/usr/local/cargo cargo clippy --version \
 && CARGO_HOME=/usr/local/cargo cargo fmt --version \
 && cc --version

# The box reaches the web only through its gate. WebFetch's own pre-check of
# each site goes to claude.ai, which is not on the gate's list; the gate does
# that job instead.
RUN mkdir -p /etc/claude-code \
 && printf '{"skipWebFetchPreflight": true}\n' > /etc/claude-code/managed-settings.json

# Decurion's MCP settings: only the browser. The runner passes
# `--mcp-config /etc/decurion/mcp.json --strict-mcp-config`, so MCP servers a
# repo brings along are never loaded.
# The browser runs headless and keeps its profile in memory (--isolated).
# Chromium's own sandbox is off (--no-sandbox) because the container runs with
# --cap-drop ALL and cannot start it; the container is the sandbox.
# Screenshots without a name go to /tmp, not /work, so they never end up in a commit.
# --no-webmcp: web pages cannot add their own tools to the agent.
# The browser goes out through the box's gate like everything else; pages the
# agent serves inside the box (localhost) are reached directly.
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
# cargo keeps downloaded crates and `cargo install`ed tools here, in the
# agent's home volume. Set only now, so no step above creates the folder as
# root. The pinned toolchain stays ahead of it on PATH.
ENV CARGO_HOME=/home/agent/.cargo \
    PATH=$PATH:/home/agent/.cargo/bin
ENTRYPOINT ["claude"]
