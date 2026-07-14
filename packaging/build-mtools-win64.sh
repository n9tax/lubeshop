#!/usr/bin/env bash
#
# Build the native Windows (x86_64) mtools binaries the app drives — mdir, mcopy,
# mdel, mformat — packaged as a self-contained zip the Windows installer downloads
# (WinSource::Bundle in crates/gwm-core/src/tools.rs).
#
# mtools is a *single* binary that dispatches on argv[0]: run as `mdir` it acts as
# mdir, etc. It strips the `.exe` suffix when matching, so we just ship copies named
# mdir.exe / mcopy.exe / mdel.exe / mformat.exe (verified on Windows). Linked static
# so the exes depend only on system DLLs (kernel32/msvcrt) — no mingw runtime.
#
# Validated under MSYS2 mingw-w64 gcc (2026-07-13); also a Linux cross-build with
# the mingw-w64 toolchain (the CI recipe): set HOST=x86_64-w64-mingw32.
#
# Usage:  packaging/build-mtools-win64.sh [OUTDIR]   (default OUTDIR=dist)
set -euo pipefail

MTOOLS_VER=4.0.49
MTOOLS_URL="https://ftp.gnu.org/gnu/mtools/mtools-${MTOOLS_VER}.tar.gz"
# The command names the app invokes (each shipped as a copy of mtools.exe).
COMMANDS="mdir mcopy mdel mformat"

OUT=$(cd "${1:-dist}" 2>/dev/null && pwd || (mkdir -p "${1:-dist}" && cd "${1:-dist}" && pwd))
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

HOST="${HOST:-}"
host_arg=""
[ -n "$HOST" ] && host_arg="--host=$HOST"

# gcc 14+ makes several legacy-C warnings fatal. ac_cv_header_iconv_h=no drops the
# iconv charset path, which on Windows pulls in <langinfo.h> (a POSIX header mingw
# lacks); we only browse ASCII 8.3 FAT names, so charset conversion isn't needed.
RELAX="-O2 -Wno-incompatible-pointer-types -Wno-int-conversion -Wno-implicit-function-declaration"

echo ">> fetching mtools ${MTOOLS_VER}"
cd "$WORK"
curl -fsSL -o mtools.tar.gz "$MTOOLS_URL"
tar xzf mtools.tar.gz
cd "mtools-${MTOOLS_VER}"

echo ">> building mtools (static, no iconv)"
./configure $host_arg ac_cv_header_iconv_h=no CFLAGS="$RELAX" LDFLAGS="-static"
make

echo ">> packaging (mtools.exe -> per-command copies)"
STRIP="${HOST:+$HOST-}strip"
command -v "$STRIP" >/dev/null 2>&1 && "$STRIP" mtools.exe || true
STAGE="$WORK/mtools"
mkdir -p "$STAGE"
for c in $COMMANDS; do cp mtools.exe "$STAGE/$c.exe"; done
( cd "$WORK" && zip -qr "$OUT/mtools-win64.zip" mtools )
echo ">> wrote $OUT/mtools-win64.zip"
ls -la "$OUT/mtools-win64.zip"
