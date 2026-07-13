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

git add Cargo.toml Cargo.lock
git commit -q -m "Release v$ver"
git tag "v$ver"

echo "Bumped to $ver, committed, and tagged v$ver."
echo "Publish it with:"
echo "  git push && git push origin v$ver"
