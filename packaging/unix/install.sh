#!/bin/sh
# Installs Decurion from an unpacked release download on Mac or Linux.
#
#   ./install.sh                  install for you only, into ~/.local
#   ./install.sh --prefix DIR     install somewhere else (for example /usr/local, with sudo)
#   ./install.sh --uninstall      remove it again (your boards and settings stay)
set -eu

prefix="$HOME/.local"
uninstall=0
while [ $# -gt 0 ]; do
    case "$1" in
    --prefix)
        [ $# -ge 2 ] || { echo "--prefix needs a folder" >&2; exit 2; }
        prefix=$2
        shift 2
        ;;
    --uninstall)
        uninstall=1
        shift
        ;;
    -h | --help)
        sed -n '2,7p' "$0"
        exit 0
        ;;
    *)
        echo "Unknown option: $1 (try --help)" >&2
        exit 2
        ;;
    esac
done

bins="decurion-hub decurion-runner decurion-start"

if [ "$uninstall" = 1 ]; then
    for b in $bins; do rm -f "$prefix/bin/$b"; done
    rm -rf "$prefix/share/decurion"
    echo "Decurion is removed from $prefix."
    echo "Your boards and settings are still in ${DECURION_DATA:-$HOME/decurion}. Delete that folder to remove them too."
    exit 0
fi

src=$(cd "$(dirname "$0")" && pwd)
for b in $bins; do
    [ -f "$src/bin/$b" ] || { echo "This folder is missing bin/$b. Unpack the whole download and try again." >&2; exit 1; }
done

mkdir -p "$prefix/bin" "$prefix/share/decurion"
for b in $bins; do
    cp "$src/bin/$b" "$prefix/bin/$b"
    chmod 755 "$prefix/bin/$b"
done
rm -rf "$prefix/share/decurion/docker"
cp -R "$src/share/decurion/docker" "$prefix/share/decurion/docker"
for f in LICENSE NOTICE README.md; do
    [ -f "$src/$f" ] && cp "$src/$f" "$prefix/share/decurion/$f"
done

# A Mac marks downloaded files as untrusted until you open them once. Running
# this script is that choice, so clear the mark on the programs it installed.
if [ "$(uname -s)" = Darwin ]; then
    for b in $bins; do xattr -d com.apple.quarantine "$prefix/bin/$b" 2>/dev/null || true; done
fi

echo "Decurion is installed in $prefix."
case ":$PATH:" in
*":$prefix/bin:"*) echo "Start it any time with: decurion-start" ;;
*) echo "Start it with: $prefix/bin/decurion-start" ;;
esac
command -v docker >/dev/null 2>&1 ||
    echo "You also need Docker Desktop: https://www.docker.com/products/docker-desktop/"
command -v git >/dev/null 2>&1 || echo "You also need Git: https://git-scm.com/downloads"
command -v claude >/dev/null 2>&1 || echo "You also need Claude Code: https://code.claude.com/docs/en/setup"
