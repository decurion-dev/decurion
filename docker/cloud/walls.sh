#!/bin/bash
# Decurion walls for the cloud server.
#
# Agent containers may reach the public internet, but nothing private:
#   - the hosting company's metadata address (169.254.0.0/16), which can hand
#     out the server's setup text and other secrets,
#   - the Tailscale network (100.64.0.0/10, fd7a:115c:a1e0::/48), which is
#     where the owner's PC and phones are,
#   - private networks the server may be attached to (10/8, 172.16/12,
#     192.168/16, fc00::/7, fe80::/10),
#   - the server itself (its own addresses, including its Tailscale one),
#     except the runner's gatekeeper on port 7421.
#
# Traffic from containers passes Docker's DOCKER-USER chain (on its way out)
# or the INPUT chain (when it is addressed to the server itself). We hang our
# own chains off both, and only for packets that come in on a Docker bridge
# (docker0 or br-*), so the server's own traffic is never touched.
#
# Safe to run again and again: every rule is checked with -C before it is
# added. decurion-walls.service runs this at boot and every time Docker
# starts, because Docker rebuilds its firewall rules when it starts.
set -euo pipefail

V4_BLOCKED=(169.254.0.0/16 100.64.0.0/10 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16)
V6_BLOCKED=(fd7a:115c:a1e0::/48 fc00::/7 fe80::/10)
BRIDGES=(docker0 br-+)

# ensure TOOL CHAIN RULE...: add RULE to CHAIN (at the end) unless it is there.
ensure() {
  local tool=$1 chain=$2; shift 2
  "$tool" -w -C "$chain" "$@" 2>/dev/null || "$tool" -w -A "$chain" "$@"
}

# ensure_first TOOL CHAIN RULE...: put RULE at the top of CHAIN unless it is there.
ensure_first() {
  local tool=$1 chain=$2; shift 2
  "$tool" -w -C "$chain" "$@" 2>/dev/null || "$tool" -w -I "$chain" 1 "$@"
}

# ensure_chain TOOL CHAIN: create CHAIN if it does not exist yet.
ensure_chain() {
  "$1" -w -n -L "$2" >/dev/null 2>&1 || "$1" -w -N "$2"
}

wall() {
  local tool=$1 reject=$2; shift 2
  local ranges=("$@") bridge range

  # Docker normally makes DOCKER-USER itself; making it here too is harmless
  # and means the rules are ready even before the first container starts.
  ensure_chain "$tool" DOCKER-USER
  ensure_chain "$tool" DECURION-OUT
  ensure_chain "$tool" DECURION-IN

  # Leaving the server: refuse private destinations.
  for range in "${ranges[@]}"; do
    ensure "$tool" DECURION-OUT -d "$range" -j REJECT --reject-with "$reject"
  done

  # Talking to the server itself: allow replies, refuse everything else.
  # (Containers get DNS from public servers, see /etc/docker/daemon.json,
  # so they never need to talk to the server.)
  ensure "$tool" DECURION-IN -m conntrack --ctstate ESTABLISHED,RELATED -j RETURN
  # The one exception: the runner's gatekeeper, which lends pay-per-use runs
  # the API key without handing it over. It listens only on the Docker
  # bridge's address, port 7421 (IPv4).
  if [ "$tool" = iptables ]; then
    ensure_first "$tool" DECURION-IN -p tcp --dport 7421 -j ACCEPT
  fi
  ensure "$tool" DECURION-IN -j REJECT --reject-with "$reject"

  for bridge in "${BRIDGES[@]}"; do
    ensure_first "$tool" DOCKER-USER -i "$bridge" -j DECURION-OUT
    ensure_first "$tool" INPUT -i "$bridge" -j DECURION-IN
  done
}

wall iptables icmp-port-unreachable "${V4_BLOCKED[@]}"
wall ip6tables icmp6-port-unreachable "${V6_BLOCKED[@]}"
echo "decurion-walls: walls are up"
