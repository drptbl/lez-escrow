#!/usr/bin/env bash
# Fails unless the build log's image id is the deployed program's, as the README records it.
# A different one means the source no longer builds the program that is on chain.
# Usage: scripts/check-image-id.sh <make build log>
set -euo pipefail
built=$(grep -o 'ImageID: [0-9a-f]*' "$1" | cut -d' ' -f2)
deployed=$(grep -o '| Image id | `[0-9a-f]*`' README.md | grep -o '[0-9a-f]\{64\}')
echo "built    $built"
echo "deployed $deployed"
test -n "$built" && test "$built" = "$deployed"
