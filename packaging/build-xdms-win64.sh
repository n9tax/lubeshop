#!/usr/bin/env bash
#
# Build the native Windows (x86_64) `xdms.exe` — the Amiga DMS (Disk Masher
# System) archive unpacker lubeshop uses to turn dropped `.dms` files into
# browsable `.adf` disks (crates/gwm-core/src/library.rs `unpack_dms`). Packaged
# as a self-contained zip the Windows installer downloads (WinSource::Bundle in
# crates/gwm-core/src/tools.rs).
#
# xDMS is portable, public-domain C. Two wrinkles handled below:
#   * its `configure` is a hand-written script (NOT autotools), so we don't pass
#     --host; we cross-compile by overriding CC in `make` instead.
#   * modern gcc drops its old GNU inline functions (`decode_c`/`decode_p` fail to
#     link) unless we force pre-C99 inline semantics with -fgnu89-inline. Passing
#     it via CC keeps the Makefile's own -I/CFLAGS intact.
# Linked static so the .exe depends only on system DLLs — no mingw runtime.
#
# Validated as a Linux cross-build with the mingw-w64 toolchain (the CI recipe):
#   Linux:  apt-get install gcc-mingw-w64-x86-64, set HOST=x86_64-w64-mingw32
#   MSYS2:  pacman -S base-devel mingw-w64-x86_64-toolchain, run in MINGW64 shell
#           (HOST empty => native)
#
# Usage:  packaging/build-xdms-win64.sh [OUTDIR]   (default OUTDIR=dist)
set -euo pipefail

XDMS_VER=1.3.2
XDMS_URL="http://zakalwe.fi/~shd/foss/xdms/xdms-${XDMS_VER}.tar.bz2"

OUT=$(cd "${1:-dist}" 2>/dev/null && pwd || (mkdir -p "${1:-dist}" && cd "${1:-dist}" && pwd))
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# On MSYS2 the native gcc already targets Windows; a Linux cross-build sets HOST.
# xDMS's configure ignores --host, so we drive the toolchain through CC/make.
HOST="${HOST:-}"
CC="${HOST:+$HOST-}gcc"
STRIP="${HOST:+$HOST-}strip"

echo ">> fetching xDMS ${XDMS_VER}"
cd "$WORK"
curl -fsSL -o xdms.tar.bz2 "$XDMS_URL"
tar -xjf xdms.tar.bz2 -C "$WORK"
cd "xdms-${XDMS_VER}"

echo ">> configuring (generates the Makefile from Makefile.in)"
./configure >/dev/null

echo ">> building (static, gnu89 inline, cross via CC=$CC)"
# `make CC=...` overrides the Makefile's compiler for both compile and link so the
# mingw toolchain + -fgnu89-inline apply without clobbering the Makefile's own -I.
make CC="$CC -fgnu89-inline" LDFLAGS="-static"

echo ">> packaging"
STAGE="$WORK/xdms"
mkdir -p "$STAGE"
# mingw links `-o xdms`, producing a PE binary named `xdms`; ship it as xdms.exe.
command -v "$STRIP" >/dev/null 2>&1 && "$STRIP" src/xdms || true
cp src/xdms "$STAGE/xdms.exe"
# xDMS is public domain; ship its COPYING + a source note.
cp COPYING "$STAGE/LICENSE-xdms.txt" 2>/dev/null || true
cat > "$STAGE/SOURCE.txt" <<EOF
This binary was built from unmodified upstream source:
  xDMS ${XDMS_VER} (Public Domain) — ${XDMS_URL}
The corresponding source is available at the URL above.
License is in LICENSE-xdms.txt.
EOF
# Zip the contents flat (no top folder) so WinSource::Bundle extracts xdms.exe
# directly into %LOCALAPPDATA%\lubeshop\bin (which is on PATH).
( cd "$STAGE" && zip -qr "$OUT/xdms-win64.zip" . )
echo ">> wrote $OUT/xdms-win64.zip"
ls -la "$OUT/xdms-win64.zip"
