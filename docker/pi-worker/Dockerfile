# Sealed workspace for one pi agent (ChatGPT or Grok work).
# The agent sees only /work (its own copy of the code) and /home/agent.
FROM node:22-bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates ripgrep \
 && rm -rf /var/lib/apt/lists/*

# pi needs Node 22.19 or newer. It needs no install scripts, so none run.
ARG PI_VERSION=1.3.0
RUN npm install -g --ignore-scripts @earendil-works/pi-coding-agent@${PI_VERSION} \
 && npm cache clean --force

# pi's config folder holds its prices (models.json, models-store.json), its
# settings and its add-ons. Here every file in it belongs to root, so the agent
# cannot make its own work look cheaper or load code into pi. The folder is
# sticky and open to all only because pi takes its locks by making
# `<file>.lock` next to them; anything new written there is gone when the
# container is. Cache warming is off: its refresh requests cost money but are
# not in pi's event stream. Sessions are kept in the thread's volume instead.
RUN mkdir -p /opt/pi-agent \
 && cd /opt/pi-agent \
 && mkdir extensions skills prompts themes tools bin sessions \
 && echo '{"cacheWarming":"off"}' > settings.json \
 && for f in auth.json models.json models-store.json mcp.json trust.json; do echo '{}' > "$f"; done \
 && chmod 0644 ./*.json \
 && chmod 1777 /opt/pi-agent
ENV PI_CODING_AGENT_DIR=/opt/pi-agent \
    PI_OFFLINE=1 \
    PI_SKIP_VERSION_CHECK=1 \
    PI_TELEMETRY=0

RUN useradd --create-home --shell /bin/bash agent
USER agent
WORKDIR /work
ENTRYPOINT ["pi"]
