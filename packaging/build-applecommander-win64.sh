#!/usr/bin/env bash
#
# Build a self-contained Windows `applecommander-ac.exe` (Apple II DOS 3.3 /
# ProDOS / Pascal disk tool) the app downloads (WinSource::BundleFolder in
# crates/gwm-core/src/tools.rs). AppleCommander is a Java (Spring Boot) jar, so we
# use **jpackage** to wrap it as an app-image — a folder with a native launcher
# exe plus a bundled Java runtime — so the user needs no Java install.
#
# The runtime is a **jlink**'d minimal image (the curated module set below covers
# every operation the app drives: -lsj/-g/-p/-d/-dos140/-pro140/…), cutting the
# bundle from ~154 MB (full JRE) to ~51 MB. Needs a JDK (jlink+jpackage) and
# python (for zipping) on PATH — supplied by the windows-latest CI job. PyInstaller
# and jpackage both must run on Windows, hence a Windows CI job, not the Linux one.
#
# Usage:  packaging/build-applecommander-win64.sh [OUTDIR]   (default OUTDIR=dist)
set -euo pipefail

AC_VER=13.1
AC_URL="https://github.com/AppleCommander/AppleCommander/releases/download/${AC_VER}/AppleCommander-ac-${AC_VER}.jar"
# Modules AppleCommander needs at runtime (verified against all app operations).
MODS="java.base,java.desktop,java.logging,java.naming,java.xml,java.management,java.sql,java.scripting,java.prefs,java.datatransfer,jdk.zipfs,jdk.unsupported"

OUT=$(cd "${1:-dist}" 2>/dev/null && pwd || (mkdir -p "${1:-dist}" && cd "${1:-dist}" && pwd))
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"

echo ">> fetching AppleCommander ${AC_VER}"
mkdir input
curl -fsSL -o input/ac.jar "$AC_URL"

echo ">> jlink minimal runtime"
jlink --add-modules "$MODS" --strip-debug --no-header-files --no-man-pages \
  --compress=zip-6 --output minjre

echo ">> jpackage app-image (native launcher + bundled runtime)"
jpackage --type app-image --name applecommander-ac \
  --input input --main-jar ac.jar --runtime-image minjre --dest appdir

echo ">> packaging"
( cd appdir && python -m zipfile -c "$OUT/applecommander-win64.zip" applecommander-ac )
echo ">> wrote $OUT/applecommander-win64.zip"
ls -la "$OUT/applecommander-win64.zip"
