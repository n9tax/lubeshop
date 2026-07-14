#!/usr/bin/env bash
#
# Build the native Windows (x86_64) cpmtools binaries the app drives — cpmls,
# cpmcp, cpmrm, mkfs.cpm — plus the diskdefs data file, packaged as a self-
# contained zip the Windows installer downloads (WinSource::Bundle in
# crates/gwm-core/src/tools.rs).
#
# cpmtools needs a "device" layer; its POSIX one doesn't build on Windows, so we
# build it against **libdsk** (device_libdsk.c). Everything is linked static so
# the four .exe files depend only on system DLLs (kernel32/msvcrt/...) — no
# mingw runtime to ship.
#
# Validated on the Windows VM under MSYS2 (mingw-w64 gcc 16.1, 2026-07-13) and,
# unchanged, works as a Linux cross-build with the mingw-w64 toolchain (this is
# the recipe the CI Windows-tools job runs):
#
#   MSYS2:  pacman -S base-devel mingw-w64-x86_64-toolchain, run in MINGW64 shell
#   Linux:  apt-get install gcc-mingw-w64-x86-64, set HOST=x86_64-w64-mingw32 and
#           CC=$HOST-gcc (pass --host=$HOST to both configures)
#
# Usage:  packaging/build-cpmtools-win64.sh [OUTDIR]
#   OUTDIR defaults to ./dist. Produces $OUTDIR/cpmtools-win64.zip.
set -euo pipefail

CPMTOOLS_VER=2.23
LIBDSK_VER=1.5.22
CPMTOOLS_URL="http://www.moria.de/~michael/cpmtools/files/cpmtools-${CPMTOOLS_VER}.tar.gz"
LIBDSK_URL="https://www.seasip.info/Unix/LibDsk/libdsk-${LIBDSK_VER}.tar.gz"

OUT=$(cd "${1:-dist}" 2>/dev/null && pwd || (mkdir -p "${1:-dist}" && cd "${1:-dist}" && pwd))
WORK=$(mktemp -d)
PREFIX="$WORK/prefix"
trap 'rm -rf "$WORK"' EXIT

# On MSYS2 the native gcc already targets Windows; a Linux cross-build passes a
# --host. HOST empty => native (MSYS2). Set HOST=x86_64-w64-mingw32 for cross.
HOST="${HOST:-}"
host_arg=""
[ -n "$HOST" ] && host_arg="--host=$HOST"

# gcc 14+ turned several legacy-C warnings into hard errors; these old projects
# predate that. -DNOTWINDLL makes libdsk.h's API plain (not __declspec(dllimport))
# so cpmtools links the *static* libdsk. See libdsk.h LDPUBLIC32 guard.
RELAX="-O2 -Wno-incompatible-pointer-types -Wno-int-conversion -Wno-implicit-function-declaration"

echo ">> fetching sources"
cd "$WORK"
curl -fsSL -o cpmtools.tar.gz "$CPMTOOLS_URL"
curl -fsSL -o libdsk.tar.gz   "$LIBDSK_URL"
tar xzf cpmtools.tar.gz
tar xzf libdsk.tar.gz

echo ">> building libdsk (static)"
cd "$WORK/libdsk-${LIBDSK_VER}"
# --without-zlib/--without-bzlib: we drive plain CP/M image files, not compressed
# ones, so drop the compression support. This keeps libdsk.a from referencing
# zlib/bz2 — which the Linux mingw cross-sysroot (apt gcc-mingw-w64-x86-64) doesn't
# ship — so cpmtools needs no -lz/-lbz2 and configure+link work on both toolchains.
./configure $host_arg --prefix="$PREFIX" --enable-static --disable-shared \
  --without-zlib --without-bzlib CFLAGS="$RELAX"
make -j"$(nproc)"
make install   # installs libdsk.a + libdsk.h (its own tools/ may warn; we don't need them)

echo ">> building cpmtools against libdsk (static, no curses)"
cd "$WORK/cpmtools-${CPMTOOLS_VER}"
# ac_cv_lib_*_printw=no disables the curses probe so the unused fsed.cpm isn't
# built and -lncurses isn't dragged into every static link.
./configure $host_arg --with-libdsk="$PREFIX" \
  ac_cv_lib_curses_printw=no ac_cv_lib_ncurses_printw=no \
  CFLAGS="$RELAX -DNOTWINDLL" LDFLAGS="-static"
make

echo ">> packaging"
STRIP="${HOST:+$HOST-}strip"
command -v "$STRIP" >/dev/null 2>&1 && "$STRIP" cpmls.exe cpmcp.exe cpmrm.exe mkfs.cpm.exe || true
STAGE="$WORK/cpmtools"
mkdir -p "$STAGE"
cp cpmls.exe cpmcp.exe cpmrm.exe mkfs.cpm.exe diskdefs "$STAGE/"
# GPL redistribution: ship the upstream licenses + a source note in the zip.
cp COPYING "$STAGE/LICENSE-cpmtools.txt" 2>/dev/null || true
# libdsk's tarball license filename varies; fetch its license (GPL-2) to be sure.
cp "$WORK/libdsk-${LIBDSK_VER}/COPYING" "$STAGE/LICENSE-libdsk.txt" 2>/dev/null \
  || curl -fsSL -o "$STAGE/LICENSE-libdsk.txt" "https://www.gnu.org/licenses/gpl-2.0.txt" || true
cat > "$STAGE/SOURCE.txt" <<EOF
These binaries were built from unmodified upstream source:
  cpmtools ${CPMTOOLS_VER} (GPL) — ${CPMTOOLS_URL}
  libdsk   ${LIBDSK_VER} (GPL) — ${LIBDSK_URL}
The corresponding source is available at the URLs above.
Licenses are in LICENSE-cpmtools.txt and LICENSE-libdsk.txt.
EOF
# Zip the *contents* flat (no top folder) so WinSource::Bundle extracts the exes
# and diskdefs directly into %LOCALAPPDATA%\lubeshop\bin — where cpm_command()
# looks for ./diskdefs and where the bin dir itself is on PATH.
( cd "$STAGE" && zip -qr "$OUT/cpmtools-win64.zip" . )
echo ">> wrote $OUT/cpmtools-win64.zip"
ls -la "$OUT/cpmtools-win64.zip"
