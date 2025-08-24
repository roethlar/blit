#!/usr/bin/env bash
# Simple, single-run comparison using your real data and directories.
# Uses RoboSync daemon on port 9031 and a one-off rsyncd on port 9032.

set -euo pipefail

REMOTE_HOST="${REMOTE_HOST:-skippy}"
ROBO_PORT="${ROBO_PORT:-9031}"
RSYNC_PORT="${RSYNC_PORT:-9032}"
# Optional remote SSH (e.g., root@skippy) for cache flush and dest cleanup
REMOTE_SSH="${REMOTE_SSH:-}"

ROBO_URL="robosync://$REMOTE_HOST:$ROBO_PORT"
RSYNC_URL="rsync://$REMOTE_HOST:$RSYNC_PORT"

SRC_PUSH="${SRC_PUSH:-$HOME/Downloads}"
DEST_PULL_LOCAL="${DEST_PULL_LOCAL:-$HOME/test-dls}"

ROBOSYNC="${ROBOSYNC:-$(pwd)/target/release/robosync}"

if ! command -v rsync >/dev/null; then echo "rsync not found" >&2; exit 1; fi
if [ ! -x "$ROBOSYNC" ]; then echo "robosync not found at $ROBOSYNC (set ROBOSYNC env)" >&2; exit 1; fi

echo "=== Real-data comparison ==="
echo "Remote host:   $REMOTE_HOST"
echo "RoboSync port: $ROBO_PORT"
echo "rsyncd port:   $RSYNC_PORT"
echo "Push source:   $SRC_PUSH"
echo "Pull dest:     $DEST_PULL_LOCAL"
if [[ -n "$REMOTE_SSH" ]]; then echo "Remote SSH:    $REMOTE_SSH (for cache flush/cleanup)"; else echo "Remote SSH:    not set (skipping remote cache flush/cleanup)"; fi
echo

secs() { awk -v s="$1" -v e="$2" 'BEGIN{printf "%.3f", e-s}'; }

clean_local_dir() { rm -rf -- "$1"; mkdir -p "$1"; }

run_pull() {
  echo "-- Pull (daemon → local) --"
  # RoboSync pull (cold path if REMOTE_SSH set)
  if [[ "${COLD:-1}" -eq 1 ]]; then flush_local_cache; flush_remote_cache "$REMOTE_SSH"; fi
  rm -rf -- "$DEST_PULL_LOCAL"; mkdir -p "$DEST_PULL_LOCAL"
  echo "RoboSync pull: $ROBO_URL/downloads -> $DEST_PULL_LOCAL"
  /usr/bin/time -f "RoboSync pull: %e s" "$ROBOSYNC" "$ROBO_URL/downloads" "$DEST_PULL_LOCAL" --mir -v || true
  echo
  # rsync pull (cold path)
  if [[ "${COLD:-1}" -eq 1 ]]; then flush_local_cache; flush_remote_cache "$REMOTE_SSH"; fi
  rm -rf -- "${DEST_PULL_LOCAL}_rsync"; mkdir -p "${DEST_PULL_LOCAL}_rsync"
  echo "rsync pull:    $RSYNC_URL/downloads/ -> ${DEST_PULL_LOCAL}_rsync/"
  /usr/bin/time -f "rsync pull: %e s" rsync -a --delete "$RSYNC_URL/downloads/" "${DEST_PULL_LOCAL}_rsync/" || true
  echo
}

run_push() {
  echo "-- Push (local → daemon) --"
  # RoboSync push (cold path)
  if [[ -n "$REMOTE_SSH" ]]; then remote_rmrf "$REMOTE_SSH" "/mnt/specific-pool/home/test-dls"; fi
  if [[ "${COLD:-1}" -eq 1 ]]; then flush_local_cache; flush_remote_cache "$REMOTE_SSH"; fi
  echo "RoboSync push: $SRC_PUSH -> $ROBO_URL/test-dls"
  /usr/bin/time -f "RoboSync push: %e s" "$ROBOSYNC" "$SRC_PUSH" "$ROBO_URL/test-dls" --mir -v || true
  echo
  # rsync push (cold path)
  if [[ -n "$REMOTE_SSH" ]]; then remote_rmrf "$REMOTE_SSH" "/mnt/specific-pool/home/test-dls"; fi
  if [[ "${COLD:-1}" -eq 1 ]]; then flush_local_cache; flush_remote_cache "$REMOTE_SSH"; fi
  echo "rsync push:    $SRC_PUSH/ -> $RSYNC_URL/test-dls/"
  /usr/bin/time -f "rsync push: %e s" rsync -a --delete "$SRC_PUSH/" "$RSYNC_URL/test-dls/" || true
  echo
}

run_pull
run_push

echo "Done. Review timings above. (Ensure rsyncd is running on the NAS: bench/rsyncd_once.sh)"
