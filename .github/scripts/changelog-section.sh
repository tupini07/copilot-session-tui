#!/bin/sh
# Print one version's section body from CHANGELOG.md, without its heading.
#
# Usage: changelog-section.sh v0.29.0   (a bare 0.29.0 works too)
#
# Exits 1 when that version has no section, or an empty one. The release workflow
# relies on that: publishing auto-generated notes because the hand-written ones were
# missing would be invisible, and would restore exactly the unreadable release pages
# the changelog exists to replace.
set -eu

if [ $# -ne 1 ]; then
    echo "usage: $0 <version>" >&2
    exit 2
fi

version="${1#v}"
changelog="${CHANGELOG_PATH:-CHANGELOG.md}"

if [ ! -f "$changelog" ]; then
    echo "$changelog not found" >&2
    exit 1
fi

section="$(
    awk -v want="## v${version}" '
        # Anchored so v0.2.0 cannot match the start of a v0.2.0-rc heading, and so
        # only a real heading at column 0 opens or closes a section.
        index($0, want) == 1 && (length($0) == length(want) || substr($0, length(want) + 1, 1) == " ") {
            found = 1
            next
        }
        found && index($0, "## v") == 1 { exit }
        found { print }
    ' "$changelog"
)"

# Trim leading and trailing blank lines; the heading is always followed by one.
section="$(printf '%s\n' "$section" | sed -e '/./,$!d' | sed -e ':a' -e '/^\n*$/{$d;N;ba' -e '}')"

if [ -z "$section" ]; then
    echo "No CHANGELOG.md section for v${version}" >&2
    exit 1
fi

printf '%s\n' "$section"
