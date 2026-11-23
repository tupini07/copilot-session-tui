#!/usr/bin/env bash
# The other half: the same screens drawn in every theme by the real binary, for
# looking at. Runs the e2e driver once per theme from sheet.e2e.tmpl and prints
# one verdict line per theme with the folder its pictures are in.
#
#   sheets.sh                 every theme
#   sheets.sh nord dracula    only these (names as `copilot-session-tui themes` prints)
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1
here=.claude/skills/theme-review
out="${THEME_SHEETS_DIR:-${TMPDIR:-/tmp}/cst-theme-sheets}"
mkdir -p "$out"

# The list comes from the binary, so a theme added in code is reviewed without
# anybody remembering to add it here.
listing=$(cargo run --quiet -- themes) || { echo "sheets: FAIL - cannot list themes"; exit 1; }
if [ "$#" -gt 0 ]; then
    wanted=" $* "
    listing=$(printf '%s\n' "$listing" | while IFS=$'\t' read -r id rest; do
        case "$wanted" in *" $id "*) printf '%s\t%s\n' "$id" "$rest" ;; esac
    done)
fi
[ -n "$listing" ] || { echo "sheets: FAIL - no such theme"; exit 1; }

cargo build --quiet --manifest-path tools/e2e/Cargo.toml || { echo "sheets: FAIL - driver does not build"; exit 1; }
cargo build --quiet || { echo "sheets: FAIL - CST does not build"; exit 1; }

fail=0
while IFS=$'\t' read -r id appearance label; do
    # Copilot has a dark set of colours and a light one; Classic gets the dark.
    card=dark
    [ "$appearance" = "light" ] && card=light
    script="$out/theme-$id.e2e"
    sed -e "s/@THEME@/$id/g" -e "s/@CARD@/$card/g" "$here/sheet.e2e.tmpl" > "$script"
    result=$(cargo run --quiet --manifest-path tools/e2e/Cargo.toml -- "$script" --no-build 2>&1 | grep '^RESULT' | tail -1)
    shots=$(printf '%s\n' "${TMPDIR:-/tmp}/cst-e2e/theme-$id")
    case "$result" in
        "RESULT: pass") echo "$id ($appearance): pass - $shots" ;;
        *) echo "$id ($appearance): $result - $shots/failed.png"; fail=1 ;;
    esac
done <<EOF
$listing
EOF
exit $fail
