#!/usr/bin/env bash
# Real-data RoboSync runs (pull and push), with explicit pauses for manual cache flushes.
# Does NOT attempt to flush caches automatically.

set -euo pipefail

REMOTE_HOST="${REMOTE_HOST:-skippy}"
ROBO_PORT="${ROBO_PORT:-9031}"
ROBO_URL="robosync://$REMOTE_HOST:$ROBO_PORT"

SRC_PUSH="${SRC_PUSH:-$HOME/Downloads}"
DEST_PULL_LOCAL="${DEST_PULL_LOCAL:-$HOME/test-dls}"

ROBOSYNC="${ROBOSYNC:-$(pwd)/target/release/robosync}"

if [ ! -x "$ROBOSYNC" ]; then echo "robosync not found at $ROBOSYNC (set ROBOSYNC env)" >&2; exit 1; fi

echo "=== RoboSync real-data (manual cold runs) ==="
echo "Remote:   $ROBO_URL"
echo "Pull to:  $DEST_PULL_LOCAL"
echo "Push from:$SRC_PUSH"
echo
echo "Step 1: Pull (daemon → local)"
echo "Remove local destination and prepare..."
rm -rf -- "$DEST_PULL_LOCAL" && mkdir -p "$DEST_PULL_LOCAL"
echo "Now flush caches on NAS and locally (use provided scripts if desired), then press Enter to continue..."
read -r _
/usr/bin/time -f "RoboSync pull: %e s" "$ROBOSYNC" "$ROBO_URL/downloads" "$DEST_PULL_LOCAL" --mir -v || true

echo
echo "Step 2: Push (local → daemon)"
echo "This will mirror $SRC_PUSH to $ROBO_URL/test-dls"
echo "Ensure remote target is clean (optional), flush caches on NAS and locally, then press Enter to continue..."
read -r _
/usr/bin/time -f "RoboSync push: %e s" "$ROBOSYNC" "$SRC_PUSH" "$ROBO_URL/test-dls" --mir -v || true

echo "Done. Repeat as needed."

