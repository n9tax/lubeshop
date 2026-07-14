#!/usr/bin/env bash
#
# Package HxC's `hxcfe` (the flux/image converter) for Windows — the app uses it
# for the TRS-80 flux -> DMK path (gw can't write DMK). Unlike the other tools we
# build, HxC already ships an **official prebuilt Windows binary**, so we just
# repackage the x64 command-line pieces into the folder bundle the app downloads
# (WinSource::BundleFolder in crates/gwm-core/src/tools.rs). Pure download +
# extract + rezip (the binaries are Windows PE but we never run them), so this
# runs on the Linux CI job. HxC is GPL — redistribution is fine.
#
# `hxcfe.exe` needs `libhxcfe.dll` and `libusbhxcfe.dll` beside it (verified via
# its import table). All three go in the bundle.
#
# Usage:  packaging/build-hxc-win64.sh [OUTDIR]   (default OUTDIR=dist)
set -euo pipefail

HXC_REL=HxCFloppyEmulator_V2_16_13_1
HXC_URL="https://github.com/jfdelnero/HxCFloppyEmulator/releases/download/${HXC_REL}/HxCFloppyEmulator_soft.zip"
WINDIR="HxCFloppyEmulator_soft/HxCFloppyEmulator_Software/Windows_x64"

OUT=$(cd "${1:-dist}" 2>/dev/null && pwd || (mkdir -p "${1:-dist}" && cd "${1:-dist}" && pwd))
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"

echo ">> fetching official HxC ${HXC_REL}"
curl -fsSL -o soft.zip "$HXC_URL"

echo ">> extracting the x64 hxcfe command-line pieces"
mkdir hxc
unzip -j -q soft.zip \
  "$WINDIR/hxcfe.exe" "$WINDIR/libhxcfe.dll" "$WINDIR/libusbhxcfe.dll" -d hxc
ls hxc

echo ">> packaging"
zip -qr "$OUT/hxc-win64.zip" hxc
echo ">> wrote $OUT/hxc-win64.zip"
ls -la "$OUT/hxc-win64.zip"
