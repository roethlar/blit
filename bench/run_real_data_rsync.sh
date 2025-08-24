#!/usr/bin/env bash
# Real-data rsync runs (pull and push) against a one-off rsyncd. Pauses for manual cache flushes.

set -euo pipefail

REMOTE_HOST="${REMOTE_HOST:-skippy}"
RSYNC_PORT="${RSYNC_PORT:-9032}"
RSYNC_URL="rsync://$REMOTE_HOST:$RSYNC_PORT"

SRC_PUSH="${SRC_PUSH:-$HOME/Downloads}"
DEST_PULL_LOCAL="${DEST_PULL_LOCAL:-$HOME/test-dls_rsync}"

command -v rsync >/dev/null || { echo "rsync not found" >&2; exit 1; }

echo "=== rsync real-data (manual cold runs) ==="
echo "Remote:   $RSYNC_URL"
echo "Pull to:  $DEST_PULL_LOCAL"
echo "Push from:$SRC_PUSH"
echo
echo "Step 1: Pull (daemon → local)"
rm -rf -- "$DEST_PULL_LOCAL" && mkdir -p "$DEST_PULL_LOCAL"
echo "Flush caches on NAS and locally, then press Enter to continue..."
read -r _
/usr/bin/time -f "rsync pull: %e s" rsync -a --delete "$RSYNC_URL/downloads/" "$DEST_PULL_LOCAL/" || true

echo
echo "Step 2: Push (local → daemon)"
echo "Flush caches on NAS and locally, then press Enter to continue..."
read -r _
/usr/bin/time -f "rsync push: %e s" rsync -a --delete "$SRC_PUSH/" "$RSYNC_URL/test-dls/" || true

echo "Done."

