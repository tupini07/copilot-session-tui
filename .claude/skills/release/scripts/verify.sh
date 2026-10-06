#!/usr/bin/env bash
# Pre-release verification with the output turned down. cargo test on this
# repo is ~900 tests; its raw output pasted into a conversation is the single
# biggest token cost of a release, so this prints verdicts and summary lines
# on success and full detail only for whatever failed.
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1

fail=0

if out=$(cargo fmt --check 2>&1); then
    echo "fmt: ok"
else
    echo "fmt: FAIL"
    printf '%s\n' "$out"
    fail=1
fi

if out=$(cargo clippy --all-targets 2>&1); then
    summary=$(printf '%s\n' "$out" | grep -o 'generated [0-9]* warning' | tail -1 || true)
    echo "clippy: ok, ${summary:-generated 0 warning}(s) - 3 are pre-existing on Windows, judge any beyond that:"
    printf '%s\n' "$out" | grep '^warning:' | grep -v 'generated' | sort | uniq -c
else
    echo "clippy: FAIL"
    printf '%s\n' "$out" | tail -40
    fail=1
fi

if out=$(cargo test --quiet 2>&1); then
    echo "test: ok"
    printf '%s\n' "$out" | grep '^test result:'
else
    echo "test: FAIL - full output of the failing run follows"
    # Strip only the progress dots; everything else may matter for diagnosis.
    printf '%s\n' "$out" | grep -v '^\.*$'
    fail=1
fi

echo "release diff (must touch exactly Cargo.toml, Cargo.lock, CHANGELOG.md):"
git diff --stat 2>/dev/null

exit "$fail"
