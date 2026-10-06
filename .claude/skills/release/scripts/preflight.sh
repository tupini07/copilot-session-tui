#!/usr/bin/env bash
# Mechanical half of the release preflight: one call, one verdict line per
# check, non-zero exit if anything blocks a release. Exists so an agent reads
# a short summary instead of running eight git commands and eight raw outputs.
#
# BLOCK lines are not errors in this script - they are facts about the
# repository the agent must act on (see SKILL.md step 1).
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1

fail=0

branch=$(git rev-parse --abbrev-ref HEAD)
if [ "$branch" = "main" ]; then
    echo "branch: main"
else
    echo "BLOCK branch: $branch (releases are cut from main)"
    fail=1
fi

git fetch origin --quiet

dirty=$(git status --porcelain --untracked-files=no)
if [ -z "$dirty" ]; then
    echo "tracked files: clean"
else
    echo "BLOCK modified tracked files (the tag would not contain these changes):"
    printf '%s\n' "$dirty"
    fail=1
fi

untracked=$(git status --porcelain | grep '^??' || true)
if [ -z "$untracked" ]; then
    echo "untracked files: none"
else
    echo "untracked files (reported, never block - they cannot end up in the tag):"
    printf '%s\n' "$untracked"
fi

ahead=$(git rev-list --count origin/main..HEAD)
behind=$(git rev-list --count HEAD..origin/main)
if [ "$ahead" -eq 0 ] && [ "$behind" -eq 0 ]; then
    echo "sync with origin/main: equal in both directions"
else
    echo "BLOCK sync: $ahead unpushed, $behind unpulled"
    fail=1
    if [ "$behind" -gt 0 ]; then
        if git merge-tree --write-tree HEAD origin/main >/dev/null 2>&1; then
            echo "  origin/main merges textually cleanly - integrate it, then verify.sh must pass again"
        else
            echo "  origin/main CONFLICTS with HEAD - stop and show the user, do not resolve alone"
        fi
    fi
fi

# --sort=-v:refname, not git describe: describe follows first-parent history
# and this repo merges with merge commits, so it can silently pick a wrong tag.
last=$(git tag --sort=-v:refname | head -1)
echo "last tag: $last"
echo "commits in span (no merges): $(git rev-list --count --no-merges "$last"..HEAD)"

exit "$fail"
