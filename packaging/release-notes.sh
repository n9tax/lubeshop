#!/usr/bin/env bash
#
# Print a version's release notes: its section of CHANGELOG.md, without the
# heading, surrounding blank lines trimmed, and wrapped lines joined (GitHub
# shows every newline in a release body as a break). Empty output means there
# are no notes for that version.
#
#   packaging/release-notes.sh 1.1.9          (or v1.1.9)
#
# Used by release.yml to publish the notes, by release.sh to refuse a release
# that has none, and to backfill old releases — one reader, so they all agree.
set -euo pipefail

ver="${1:?usage: $(basename "$0") <version>}"
ver="${ver#v}"
file="${2:-$(dirname "$0")/../CHANGELOG.md}"

awk -v ver="$ver" '
  # A section starts at "## <ver>" (alone, or followed by a space and a date).
  /^## / {
    if (inside) exit
    if ($0 == "## " ver || index($0, "## " ver " ") == 1) { inside = 1; next }
  }
  inside { lines[++n] = $0 }
  END {
    first = 1; while (first <= n && lines[first] ~ /^[[:space:]]*$/) first++
    last = n;  while (last >= first && lines[last] ~ /^[[:space:]]*$/) last--
    # CHANGELOG.md is wrapped at 80 columns for reading in a terminal, but
    # GitHub turns every newline in a release body into a hard break. Join a
    # wrapped line onto the one before it; a blank line, a new bullet or a
    # heading starts afresh.
    out = ""
    for (i = first; i <= last; i++) {
      line = lines[i]
      starts = (line ~ /^[[:space:]]*$/ || line ~ /^[-*] / || line ~ /^#/)
      if (!starts && out != "" && prev !~ /^[[:space:]]*$/) {
        sub(/^[[:space:]]+/, "", line)
        out = out " " line
      } else {
        if (out != "") print out
        out = line
      }
      prev = lines[i]
    }
    if (out != "") print out
  }
' "$file"
