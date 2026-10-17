#!/bin/bash
# Brings the cloud server's Decurion up to the version the hub wants.
#
# Runs as root before every start of decurion-runner.service. The runner
# writes the hub's commit to /var/lib/decurion/wanted-commit when the hub says
# it is out of date; this script then downloads that commit from GitHub
# (or the newest main if that commit is not on GitHub), builds the runner and
# the agent image as the `decurion` user, and installs them.
#
# It must never leave the server without a working runner: if anything fails
# while a working version is installed, it logs the problem and keeps the old
# version (exit 0). Only when nothing has ever been built does it fail, so
# systemd tries again 10 seconds later.
set -uo pipefail

BUILD=/opt/decurion/build                 # owned by decurion: source, Rust, build output
SRC=$BUILD/src
BIN=/opt/decurion/bin/decurion-runner     # owned by root: the runner cannot replace itself
BUILT_FILE=/opt/decurion/built-commit
WANTED_FILE=/var/lib/decurion/wanted-commit
IMAGE=decurion/claude-worker:dev

log() { echo "decurion-update: $*"; }

# Runs a command as the decurion user with the Rust tools on PATH.
as_decurion() {
  runuser -u decurion -- env HOME=/home/decurion \
    RUSTUP_HOME="$BUILD/rustup" CARGO_HOME="$BUILD/cargo" \
    PATH="$BUILD/cargo/bin:/usr/local/bin:/usr/bin:/bin" "$@"
}

have_working_version() {
  [ -x "$BIN" ] && [ -s "$BUILT_FILE" ] && docker image inspect "$IMAGE" >/dev/null 2>&1
}

# Something went wrong: keep what is installed, or fail if there is nothing yet.
give_up() {
  log "$*"
  if have_working_version; then
    log "keeping the version that is already installed ($(cat "$BUILT_FILE"))"
    exit 0
  fi
  log "nothing is installed yet; will try again shortly"
  exit 1
}

# One update at a time.
exec 9>/run/decurion-update.lock
flock 9

# The wanted commit comes from the hub via the runner: only accept a full sha.
wanted=$(tr -d '[:space:]' < "$WANTED_FILE" 2>/dev/null || true)
if ! [[ $wanted =~ ^[0-9a-f]{40}$ ]]; then
  [ -n "$wanted" ] && log "ignoring wanted-commit, it is not a commit id"
  wanted=""
fi
built=$(cat "$BUILT_FILE" 2>/dev/null || true)

# Nothing to do: the wanted version (or, with no wish, any version) is installed.
if have_working_version && { [ -z "$wanted" ] || [ "$wanted" = "$built" ]; }; then
  exit 0
fi

[ -d "$SRC/.git" ] || give_up "the Decurion source is missing from $SRC"

log "downloading updates"
as_decurion git -C "$SRC" fetch --quiet --prune origin '+refs/heads/*:refs/remotes/origin/*' \
  || give_up "could not download updates from GitHub"

target=""
if [ -n "$wanted" ]; then
  # A commit that is not on a branch (for example a pull request) can still be
  # fetched by its id.
  as_decurion git -C "$SRC" cat-file -e "$wanted^{commit}" 2>/dev/null \
    || as_decurion git -C "$SRC" fetch --quiet origin "$wanted" 2>/dev/null
  if as_decurion git -C "$SRC" cat-file -e "$wanted^{commit}" 2>/dev/null; then
    target=$wanted
  else
    log "commit $wanted is not on GitHub; using the newest main instead"
  fi
fi
if [ -z "$target" ]; then
  target=$(as_decurion git -C "$SRC" rev-parse --verify --quiet 'refs/remotes/origin/main^{commit}') \
    || give_up "could not find the main branch"
fi

if have_working_version && [ "$target" = "$built" ]; then
  exit 0
fi

log "building $target (this can take a while)"
as_decurion git -C "$SRC" -c advice.detachedHead=false checkout --quiet --force --detach "$target" \
  || give_up "could not switch to $target"

# 1. The runner. Built in place; installed only after the image also builds.
as_decurion cargo build --quiet --release --locked --manifest-path "$SRC/Cargo.toml" -p decurion-runner \
  || give_up "building the runner failed"

# 2. The agent image, first under a temporary name so a failed build leaves
#    the current image alone.
#    The agent gets the runner's user id, so both can write the work folder.
as_decurion docker build --build-arg "AGENT_UID=$(id -u decurion)" \
    --tag "$IMAGE-next" "$SRC/docker/claude-worker" \
  || give_up "building the agent image failed"

# 3. Install both. The binary is swapped in one step (rename), so a crash
#    here never leaves half a file.
install -d -o root -g root -m 0755 "$(dirname "$BIN")"
if ! { install -o root -g root -m 0755 "$SRC/target/release/decurion-runner" "$BIN.new" \
       && mv -f "$BIN.new" "$BIN"; }; then
  give_up "installing the runner failed"
fi
docker tag "$IMAGE-next" "$IMAGE" || give_up "installing the agent image failed"
docker rmi "$IMAGE-next" >/dev/null 2>&1 || true
echo "$target" > "$BUILT_FILE.new" && mv -f "$BUILT_FILE.new" "$BUILT_FILE"

# Old image layers are no longer needed.
docker image prune --force >/dev/null 2>&1 || true
log "installed $target"
