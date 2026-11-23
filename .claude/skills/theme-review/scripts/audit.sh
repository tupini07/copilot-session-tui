#!/usr/bin/env bash
# The half of a theme review a machine can do: every pair of colours a theme
# puts together, checked for contrast. Prints a verdict, and the failing pairs
# only. Pass --table for every pair of every theme with its ratio.
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1

if [ "${1:-}" = "--table" ]; then
    cargo test --quiet theme::tests::print_the_theme_audit -- --ignored --nocapture 2>&1 |
        grep -E '^(== |FAIL|  ok)'
    exit 0
fi

out=$(cargo test --quiet theme::tests:: 2>&1)
if printf '%s\n' "$out" | grep -q '^test result: ok'; then
    echo "audit: ok - $(cargo run --quiet -- themes | wc -l | tr -d ' ') themes, every pair passes"
else
    echo "audit: FAIL"
    # A failing pair reads "Theme: what is 2.31, needs 3.0".
    printf '%s\n' "$out" | grep -E 'needs [0-9.]+$' || printf '%s\n' "$out" | tail -30
    exit 1
fi
