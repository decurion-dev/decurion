# Sealed workspace for one Claude Code agent.
# The agent sees only /work (its own copy of the code) and /home/agent.
FROM node:22-bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates ripgrep \
 && rm -rf /var/lib/apt/lists/*

ARG CLAUDE_CODE_VERSION=2.1.289
RUN npm install -g @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION} && npm cache clean --force

RUN useradd --create-home --shell /bin/bash agent
USER agent
WORKDIR /work
ENV CLAUDE_CONFIG_DIR=/home/agent/.claude
ENTRYPOINT ["claude"]
