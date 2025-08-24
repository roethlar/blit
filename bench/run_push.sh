#!/usr/bin/env bash
set -euo pipefail
HERE=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "$HERE/common.sh"
check_tools

# Remote RoboSync daemon settings
REMOTE_HOST="${REMOTE_HOST:-localhost}"
REMOTE_PORT="${REMOTE_PORT:-9031}"
REMOTE_ROOT="${REMOTE_ROOT:-/srv/robosync_root}"

# Optional rsync-over-ssh remote (e.g., user@host) for comparison
RSYNC_REMOTE="${RSYNC_REMOTE:-}"

ROOT="$BENCH_ROOT"
OUT="$RESULTS_DIR/push.csv"

push_robo() { # usage: push_robo <src> <name>
  local src="$1" name="$2"
  local bytes files t0 sec rate
  bytes=$(size_bytes "$src")
  files=$(count_files "$src")
  echo "[push] robosync dataset=$name files=$files"
  t0=$(date +%s%N)
  "$ROBOSYNC" "$src" "robosync://$REMOTE_HOST:$REMOTE_PORT/$name" --mir >/dev/null 2>&1 || true
  sec=$(secs_since "$t0")
  rate=$(mbps "$bytes" "$sec")
  log_csv "$OUT" push daemon "$name" "$files" "$bytes" robosync "$sec" "$rate" "host=$REMOTE_HOST"
}

push_rsync() { # usage: push_rsync <src> <name>
  local src="$1" name="$2"
  [ -n "$RSYNC_REMOTE" ] || { echo "[push] skipping rsync; set RSYNC_REMOTE=user@host"; return 0; }
  local dest="$RSYNC_REMOTE:$REMOTE_ROOT/$name/"
  local bytes files t0 sec rate
  bytes=$(size_bytes "$src"); files=$(count_files "$src")
  echo "[push] rsync dataset=$name files=$files"
  t0=$(date +%s%N)
  rsync -a --delete -e ssh "$src/" "$dest" >/dev/null 2>&1 || true
  sec=$(secs_since "$t0")
  rate=$(mbps "$bytes" "$sec")
  log_csv "$OUT" push ssh "$name" "$files" "$bytes" rsync "$sec" "$rate" "remote=$RSYNC_REMOTE"
}

for name in small mixed large; do
  src="$ROOT/src_$name"
  for i in $(seq 1 "$ITER"); do
    push_robo "$src" "$name"
    push_rsync "$src" "$name"
  done
done

echo "Results: $OUT"

