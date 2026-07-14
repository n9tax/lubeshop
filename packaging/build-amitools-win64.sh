#!/usr/bin/env bash
#
# Freeze amitools' `xdftool` (Amiga ADF/HDF tool) into a self-contained Windows
# folder bundle the app downloads (WinSource::BundleFolder in
# crates/gwm-core/src/tools.rs). Unlike the cpmtools/mtools C builds, amitools is
# **pure Python** — its native m68k emulator (`machine68k`) is an *optional*
# `[vamos]` extra that xdftool doesn't use — so we PyInstaller-freeze it instead
# of cross-compiling. That must run on Windows (PyInstaller doesn't cross-build),
# so this is invoked by a windows-latest CI job, not the Linux one.
#
# Needs `python` on PATH (with pip). Produces $OUTDIR/amitools-win64.zip whose
# single top folder `xdftool/` holds xdftool.exe + its _internal/ runtime.
#
# Usage:  packaging/build-amitools-win64.sh [OUTDIR]   (default OUTDIR=dist)
set -euo pipefail

AMITOOLS_VER=0.8.1

OUT=$(cd "${1:-dist}" 2>/dev/null && pwd || (mkdir -p "${1:-dist}" && cd "${1:-dist}" && pwd))
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"

echo ">> installing amitools ${AMITOOLS_VER} + pyinstaller"
python -m pip install --quiet --upgrade pip
python -m pip install --quiet "amitools==${AMITOOLS_VER}" pyinstaller

cat > xdftool_main.py <<'PY'
# PyInstaller entry point: invoke amitools' xdftool CLI.
import sys
from amitools.tools.xdftool import main
if __name__ == "__main__":
    sys.exit(main())
PY

echo ">> freezing xdftool (onedir, excluding the optional m68k/vamos parts)"
# xdftool dispatches subcommands from a static cmd_map and imports only
# amitools.fs / amitools.util, so collect those submodules and exclude the
# vamos tooling that would drag in the (absent) machine68k C extension.
python -m PyInstaller --name xdftool \
  --collect-submodules amitools.fs --collect-submodules amitools.util \
  --exclude-module machine68k \
  --exclude-module amitools.vamos --exclude-module amitools.tools.vamos \
  --console --noconfirm --clean xdftool_main.py

echo ">> packaging"
# GPL-2.0: include the upstream license + a source note in the bundle.
curl -fsSL -o dist/xdftool/LICENSE-amitools.txt "https://www.gnu.org/licenses/gpl-2.0.txt" || true
cat > dist/xdftool/SOURCE.txt <<EOF
This bundle contains amitools ${AMITOOLS_VER} (GPL-2.0), frozen with PyInstaller.
Source: https://github.com/cnvogelg/amitools  (pip: amitools==${AMITOOLS_VER})
License: LICENSE-amitools.txt
EOF
( cd dist && python -m zipfile -c "$OUT/amitools-win64.zip" xdftool )
echo ">> wrote $OUT/amitools-win64.zip"
ls -la "$OUT/amitools-win64.zip"
