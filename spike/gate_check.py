"""Checks a box's website walls with real containers.

Builds nothing: run it after building decurion/gate:dev. It sets up a box the
way the runner does (its own internal network, a gate on it), then tries to
get out from inside a throwaway container.

    python spike/gate_check.py

Each line prints PASS or FAIL. "Allowed site" needs the PC to be online.
"""

import subprocess
import sys
import time

NET = "decurion-net-check"
GATE = "decurion-gate-check"
PROBE_IMAGE = "curlimages/curl:8.11.1"
ALLOWED = "example.com"


def docker(*args, check=True):
    return subprocess.run(["docker", *args], capture_output=True, text=True, check=check)


def in_box(script, proxy=True):
    env = []
    if proxy:
        env = ["-e", "https_proxy=http://gate:3128", "-e", "http_proxy=http://gate:3128"]
    r = docker("run", "--rm", "--network", NET, "--cap-drop", "ALL", *env,
               PROBE_IMAGE, "sh", "-c", script, check=False)
    return r.returncode, (r.stdout + r.stderr).strip()


def cleanup():
    docker("rm", "-f", GATE, check=False)
    docker("network", "rm", NET, check=False)


def main():
    cleanup()
    if docker("image", "inspect", PROBE_IMAGE, check=False).returncode != 0:
        docker("pull", "-q", PROBE_IMAGE)
    docker("network", "create", "--internal", "-o", "com.docker.network.bridge.inhibit_ipv4=true", NET)
    gate = subprocess.Popen(
        ["docker", "run", "--rm", "--name", GATE, "--network", NET, "--network-alias", "gate",
         "--cap-drop", "ALL", "--security-opt", "no-new-privileges", "--read-only",
         "-e", f"DECURION_SITES=api.anthropic.com,{ALLOWED},localtest.me", "decurion/gate:dev"],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    ready = gate.stdout.readline()
    if '"ready"' not in ready:
        print("FAIL gate did not start:", ready)
        cleanup()
        return 1
    docker("network", "connect", "bridge", GATE)

    # The PC's address on the outside network, and the address it would have
    # on the box's network if it took one.
    host_ip = docker("network", "inspect", "bridge", "-f",
                     "{{(index .IPAM.Config 0).Gateway}}").stdout.strip()
    subnet = docker("network", "inspect", NET, "-f",
                    "{{(index .IPAM.Config 0).Subnet}}").stdout.strip()
    box_gw = subnet.rsplit(".", 2)[0] + ".0.1" if subnet.count(".") == 3 else ""
    results = []

    def check(name, ok, detail=""):
        results.append(ok)
        print(("PASS " if ok else "FAIL ") + name + (f"  ({detail})" if detail and not ok else ""))

    def curl(url, proxy=True):
        # The gate's answer to the tunnel request (000 without the gate), then
        # the site's own status (000 when nothing answered).
        return in_box(f"curl -s -o /dev/null -w '%{{http_connect}} %{{http_code}}' -m 15 {url}; true",
                      proxy)[1]

    out = curl("https://unlisted.example.org")
    check("unlisted site is blocked", out.startswith("403"), out)
    out = curl(f"https://{ALLOWED}")
    check("allowed site works (needs internet)", out == "200 200", out)
    for ip in filter(None, [host_ip, box_gw]):
        out = curl(f"http://{ip}:7420/healthz", proxy=False)
        check(f"box cannot reach the PC at {ip}", out == "000 000", out)
    out = curl("http://1.1.1.1", proxy=False)
    check("box cannot reach the internet directly", out == "000 000", out)
    code, out = in_box(f"nslookup {ALLOWED} 2>&1; true", proxy=False)
    check("box cannot look up outside names", "Address" not in out.split("Name:")[-1]
          if "Name:" in out else True, out)
    # localtest.me is on the list but its name points at 127.0.0.1.
    out = curl("https://localtest.me")
    check("gate refuses listed names that point at the PC (needs internet)",
          out.startswith("403"), out)
    out = curl("https://1.1.1.1")
    check("gate refuses bare IP addresses", out.startswith("403"), out)

    docker("rm", "-f", GATE, check=False)
    gate.wait(timeout=30)
    blocked = [l for l in gate.stdout.read().splitlines() if '"blocked"' in l]
    check("gate reported what it blocked", any("unlisted.example.org" in l for l in blocked),
          str(blocked))
    time.sleep(1)
    cleanup()
    return 0 if all(results) else 1


if __name__ == "__main__":
    sys.exit(main())
