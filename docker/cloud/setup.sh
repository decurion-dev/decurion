#!/bin/bash
# First-boot setup of a Decurion cloud server (Ubuntu 24.04).
#
# The setup text from Decurion's "Add cloud worker" page puts this script and
# the other files from docker/cloud/ on the server, writes
# /etc/decurion/setup.env and /etc/decurion/runner.env, and runs this once.
# Its output is in /var/log/cloud-init-output.log.
#
# What it does:
#   1. joins the owner's Tailscale network (so it can reach the hub),
#   2. installs Docker, git, build tools and Rust,
#   3. creates the `decurion` user and its folders,
#   4. downloads Decurion from GitHub at the hub's version,
#   5. turns on the walls and the runner. The runner's first start builds
#      everything (update.sh), then it joins the hub with the one-time code.
set -euo pipefail

# Values from the setup page: TAILSCALE_AUTH_KEY, SERVER_NAME, REPO, HUB_COMMIT.
# shellcheck source=/dev/null
. /etc/decurion/setup.env

BUILD=/opt/decurion/build
export DEBIAN_FRONTEND=noninteractive

log() { echo "decurion-setup: $*"; }

# Tries a command up to 5 times; the network is sometimes slow on first boot.
retry() {
  local n
  for n in 1 2 3 4 5; do
    "$@" && return 0
    log "attempt $n of: $1 failed, trying again in 15 seconds"
    sleep 15
  done
  return 1
}

apt_get() { apt-get -o DPkg::Lock::Timeout=600 -y "$@"; }

log "1/5 joining Tailscale as $SERVER_NAME"
if ! command -v tailscale >/dev/null; then
  retry sh -c 'curl -fsSL https://tailscale.com/install.sh | sh'
fi
# --shields-up: other devices on the Tailscale network cannot connect to this
# server. It only connects out (to the hub).
retry tailscale up --auth-key="$TAILSCALE_AUTH_KEY" --hostname="$SERVER_NAME" --shields-up

log "2/5 installing Docker, git and build tools"
retry apt_get update
retry apt_get install --no-install-recommends \
  docker.io docker-buildx git build-essential pkg-config ca-certificates curl iptables
systemctl enable --now docker

log "3/5 creating the decurion user and folders"
id decurion >/dev/null 2>&1 \
  || useradd --system --create-home --home-dir /home/decurion --shell /usr/sbin/nologin decurion
usermod -aG docker decurion
install -d -o root -g root -m 0755 /opt/decurion /opt/decurion/bin
install -d -o decurion -g decurion -m 0755 "$BUILD"
install -d -o decurion -g decurion -m 0700 /var/lib/decurion
chown root:root /etc/decurion/runner.env
chmod 0600 /etc/decurion/runner.env
# The version to build first. Later the runner updates this file itself.
if [ ! -s /var/lib/decurion/wanted-commit ] && [ -n "$HUB_COMMIT" ]; then
  echo "$HUB_COMMIT" > /var/lib/decurion/wanted-commit
  chown decurion:decurion /var/lib/decurion/wanted-commit
fi

as_decurion() {
  runuser -u decurion -- env HOME=/home/decurion \
    RUSTUP_HOME="$BUILD/rustup" CARGO_HOME="$BUILD/cargo" \
    PATH="$BUILD/cargo/bin:/usr/local/bin:/usr/bin:/bin" "$@"
}

log "4/5 installing Rust and downloading Decurion"
if [ ! -x "$BUILD/cargo/bin/cargo" ]; then
  retry as_decurion sh -c 'curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain stable --no-modify-path'
fi
if [ ! -d "$BUILD/src/.git" ]; then
  retry as_decurion git clone --quiet "$REPO" "$BUILD/src"
fi

log "5/5 turning on the walls and the runner"
systemctl daemon-reload
systemctl enable --now decurion-walls.service
# --no-block: the first start builds Decurion, which takes a while.
systemctl enable --now --no-block decurion-runner.service

# The Tailscale key was single-use and is spent; remove it anyway.
rm -f /etc/decurion/setup.env
log "done. The runner is building Decurion now; follow it with: journalctl -fu decurion-runner"
