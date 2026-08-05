#!/usr/bin/env bash
#
# Cut a release without the version drifting between the crate, the tarballs, and
# the .deb. Bumps the workspace version, syncs the lockfile, commits, and tags —
# so the git tag and every built artifact always agree.
#
#   packaging/release.sh 0.1.1
#
# then, when you're happy:
#
#   git push && git push origin v0.1.1
#
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "usage: $(basename "$0") <version>   e.g. $(basename "$0") 0.1.1" >&2
  exit 1
fi
ver="$1"

# Accept either "0.1.1" or "v0.1.1".
ver="${ver#v}"
if ! printf '%s' "$ver" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "error: version must look like 1.2.3 (got '$ver')" >&2
  exit 1
fi

cd "$(dirname "$0")/.."

if [ -n "$(git status --porcelain)" ]; then
  echo "error: working tree isn't clean — commit or stash first." >&2
  exit 1
fi

if git rev-parse "v$ver" >/dev/null 2>&1; then
  echo "error: tag v$ver already exists." >&2
  exit 1
fi

# Bump the [workspace.package] version (the only line starting with `version = `).
sed -i -E "s/^version = \"[^\"]*\"/version = \"$ver\"/" Cargo.toml
# Sync Cargo.lock to the new version so --locked/--frozen builds keep working.
cargo update --workspace --quiet

# Keep the Arch recipe in step. Its source= line fetches the v$pkgver tarball,
# so a stale pkgver doesn't error -- it quietly builds an *old* release for
# anyone following the README's `makepkg -si` instructions. That is how it sat
# at 1.0.0 through three releases: this script bumped the crate and forgot the
# package. A new upstream version also restarts the package revision.
sed -i -E "s/^pkgver=.*/pkgver=$ver/" packaging/PKGBUILD
sed -i -E "s/^pkgrel=.*/pkgrel=1/" packaging/PKGBUILD

git add Cargo.toml Cargo.lock packaging/PKGBUILD
git commit -q -m "Release v$ver"
git tag "v$ver"

echo "Bumped to $ver, committed, and tagged v$ver."
echo "Publish it with:"
echo "  git push && git push origin v$ver"
