#!/bin/bash
# Keeps the "Add cloud worker" page in step with this folder.
#
# The hub serves crates/decurion-hub/src/cloud.html as one file, so the page
# carries its own copy of every file here (in <script type="text/plain"
# data-file="..."> blocks) and builds the setup text from them in the browser.
# The files in this folder are the ones to read and edit.
#
#   docker/cloud/check-template.sh           fail if the page's copies differ
#   docker/cloud/check-template.sh --fix     copy these files into the page
#   docker/cloud/check-template.sh --render  print a sample setup text (fake
#                                            values), e.g. for
#                                            `cloud-init schema --config-file`
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
exec python3 - "$here" "$here/../../crates/decurion-hub/src/cloud.html" "${1:-}" <<'PY'
import pathlib, re, sys

here, page_path, mode = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), sys.argv[3]
FILES = ["cloud-config.yaml", "daemon.json", "setup.sh", "update.sh", "walls.sh",
         "decurion-walls.service", "decurion-runner.service"]
BLOCK = re.compile(r'(<script type="text/plain" data-file="([^"]+)">\n)(.*?)(</script>)', re.S)

files = {name: (here / name).read_text() for name in FILES}
for name, text in files.items():
    # Text inside <script> must not be able to end the block early.
    if "</script" in text.lower() or "<!--" in text:
        sys.exit(f"{name} contains '</script' or '<!--', which cannot go inside the page")

page = page_path.read_text()
found = {m.group(2): m.group(3) for m in BLOCK.finditer(page)}

if mode == "--fix":
    page = BLOCK.sub(lambda m: m.group(1) + files.get(m.group(2), m.group(3)) + m.group(4), page)
    page_path.write_text(page)
    missing = [n for n in FILES if n not in found]
    if missing:
        sys.exit(f"cloud.html has no block for: {', '.join(missing)}")
    print("cloud.html updated")
elif mode == "--render":
    # Same steps as render() in cloud.html.
    values = {"TAILSCALE_AUTH_KEY": "tskey-auth-EXAMPLE-0000", "SERVER_NAME": "decurion-cloud-1",
              "REPO": "https://github.com/decurion-dev/decurion", "HUB_COMMIT": "0" * 40,
              "HUB_URL": "https://my-pc.example.ts.net", "JOIN_CODE": "EXAMPLE0123456789"}
    def include(m):
        indent, name = m.group(1), m.group(2)
        return "\n".join(indent + line if line else "" for line in found[name].rstrip("\n").split("\n"))
    text = re.sub(r"^( *)\{\{FILE:([\w.-]+)\}\}$", include, found["cloud-config.yaml"], flags=re.M)
    text = re.sub(r"\{\{([A-Z_]+)\}\}", lambda m: values[m.group(1)], text)
    sys.stdout.write(text)
else:
    bad = [n for n in FILES if found.get(n) != files[n]]
    if bad:
        sys.exit("cloud.html is out of date for: " + ", ".join(bad) +
                 "\nRun docker/cloud/check-template.sh --fix")
    print("cloud.html matches docker/cloud")
PY
