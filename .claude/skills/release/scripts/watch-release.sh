#!/usr/bin/env bash
# Waits for the release workflow triggered by a tag push and prints only the
# conclusion, then the published release and its assets. Replaces `gh run
# watch`, which repaints the whole job tree every few seconds and floods an
# agent transcript with hundreds of near-identical lines.
#
# Usage: watch-release.sh vX.Y.Z   (run it in the background; it blocks until
# the workflow completes, typically several minutes)
set -eu
tag=${1:?usage: watch-release.sh vX.Y.Z}
cd "$(git rev-parse --show-toplevel)"

# Tag-triggered runs report the tag name as their branch.
run_id=""
for _ in $(seq 1 24); do
    run_id=$(gh run list --branch "$tag" --limit 1 --json databaseId --jq '.[0].databaseId' 2>/dev/null || true)
    if [ -n "$run_id" ] && [ "$run_id" != "null" ]; then
        break
    fi
    sleep 5
done
if [ -z "$run_id" ] || [ "$run_id" = "null" ]; then
    echo "no workflow run found for $tag after two minutes - check gh auth and the tag push"
    exit 1
fi
echo "watching run $run_id for $tag"

while :; do
    status=$(gh run view "$run_id" --json status --jq .status)
    if [ "$status" = "completed" ]; then
        break
    fi
    sleep 20
done

conclusion=$(gh run view "$run_id" --json conclusion --jq .conclusion)
echo "run $run_id: $conclusion"
if [ "$conclusion" != "success" ]; then
    gh run view "$run_id" --log-failed | tail -60
    exit 1
fi

gh release view "$tag" --json url,isDraft,assets \
    --jq '"\(.url) (draft: \(.isDraft))", (.assets[] | "  \(.name)  \(.size) bytes")'
