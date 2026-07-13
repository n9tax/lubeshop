#!/usr/bin/env bash
#
# Validate the Tools-menu install recipes on real Debian-family systems, in
# throwaway containers, so a wrong package name or missing build dep is caught
# before a tester hits it. Mirrors the recipes in crates/gwm-core/src/tools.rs —
# keep the two in sync.
#
#   packaging/test-installers.sh              # tests debian:12 and ubuntu:24.04
#   packaging/test-installers.sh debian:12    # one image
#
# Needs podman (or docker). On a host whose home FS can't do rootless overlayfs
# (ext4 on some kernels), point it at the vfs driver, e.g.:
#   PODMAN="podman --storage-driver vfs --root $HOME/.local/share/containers/vfs" \
#     packaging/test-installers.sh
#
set -u
PODMAN="${PODMAN:-podman}"
IMAGES=("${@:-debian:12 ubuntu:24.04}")
# shellcheck disable=SC2206
IMAGES=(${IMAGES[*]})

# The in-container test: run each tool's recipe and check the command lands.
read -r -d '' SCRIPT <<'IN' || true
set -u
export DEBIAN_FRONTEND=noninteractive
export PATH="$HOME/.local/bin:$PATH"
apt-get update -qq >/dev/null 2>&1
apt-get install -y -qq pipx curl >/dev/null 2>&1   # pipx = the one thing we ask the user to have
pass(){ echo "PASS: $1"; }; fail(){ echo "FAIL: $1"; }

apt-get install -y -qq cpmtools >/dev/null 2>&1 && command -v cpmls >/dev/null && pass cpmtools || fail cpmtools
apt-get install -y -qq mtools   >/dev/null 2>&1 && command -v mdir  >/dev/null && pass mtools   || fail mtools
apt-get install -y -qq vice     >/dev/null 2>&1 && command -v c1541 >/dev/null && pass vice     || fail "vice (no Debian package; Ubuntu-only)"

apt-get install -y -qq git build-essential python3-dev >/dev/null 2>&1
pipx install "git+https://github.com/keirf/greaseweazle@latest" >/dev/null 2>&1 && command -v gw >/dev/null && pass gw || fail gw
pipx install amitools >/dev/null 2>&1 && command -v xdftool >/dev/null && pass amitools || fail amitools

( d=$(mktemp -d); git clone --depth 1 -q https://github.com/jhallen/atari-tools "$d/s" && make -C "$d/s" >/dev/null 2>&1 && test -x "$d/s/atr" ) && pass atari-tools || fail atari-tools

apt-get install -y -qq libusb-1.0-0-dev >/dev/null 2>&1
( d=$(mktemp -d); git clone --depth 1 -q https://github.com/jfdelnero/HxCFloppyEmulator "$d/s" && make -C "$d/s/build" HxCFloppyEmulator_cmdline >/dev/null 2>&1 && test -x "$d/s/HxCFloppyEmulator_cmdline/build/hxcfe" ) && pass hxc || fail hxc

# AppleCommander: bundled Temurin 21 JRE (its jars need Java 21).
arch=$(uname -m); case "$arch" in x86_64) j=x64;; aarch64) j=aarch64;; *) j=x64;; esac
share="$HOME/.local/share/lubeshop"; mkdir -p "$share/jre" "$share/tools" "$HOME/.local/bin"
curl -fsSL "https://api.adoptium.net/v3/binary/latest/21/ga/linux/$j/jre/hotspot/normal/eclipse" -o "$share/jre.tgz" && tar xzf "$share/jre.tgz" -C "$share/jre" --strip-components=1
url=$(curl -fsSL https://api.github.com/repos/AppleCommander/AppleCommander/releases/latest | grep -oE 'https://[^"]*AppleCommander-ac-[0-9.]*\.jar' | head -1)
curl -fsSL -o "$share/tools/ac.jar" "$url"
printf '#!/bin/sh\nexec "$HOME/.local/share/lubeshop/jre/bin/java" -jar "$HOME/.local/share/lubeshop/tools/ac.jar" "$@"\n' > "$HOME/.local/bin/applecommander-ac"
chmod +x "$HOME/.local/bin/applecommander-ac"
applecommander-ac 2>&1 | grep -q "AppleCommander command line" && pass applecommander || fail applecommander
echo "=== DONE ==="
IN

rc=0
for img in "${IMAGES[@]}"; do
  echo "############ $img ############"
  out=$($PODMAN run --rm -i "$img" bash -s <<<"$SCRIPT" 2>&1)
  echo "$out" | grep -E "PASS:|FAIL:|DONE"
  # vice failing on Debian is expected; any other FAIL is a real problem.
  if echo "$out" | grep "FAIL:" | grep -qv "vice"; then rc=1; fi
done
exit $rc
