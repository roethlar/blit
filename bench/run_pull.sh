#!/usr/bin/env bash
set -euo pipefail
HERE=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "$HERE/common.sh"
check_tools

REMOTE_HOST="${REMOTE_HOST:-localhost}"
REMOTE_PORT="${REMOTE_PORT:-9031}"
REMOTE_ROOT="${REMOTE_ROOT:-/srv/robosync_root}"

RSYNC_REMOTE="${RSYNC_REMOTE:-}"

ROOT="$BENCH_ROOT"
OUT="$RESULTS_DIR/pull.csv"

pull_robo() {
  local name="$1" dst="$ROOT/pull_robo_${name}"
  ensure_clean_dir "$dst"
  local src_url="robosync://$REMOTE_HOST:$REMOTE_PORT/$name"
  echo "[pull] robosync dataset=$name"
  local t0=$(date +%s%N)
  "$ROBOSYNC" "$src_url" "$dst" --mir >/dev/null 2>&1 || true
  local sec=$(secs_since "$t0")
  local bytes=$(size_bytes "$dst")
  local files=$(count_files "$dst")
  local rate=$(mbps "$bytes" "$sec")
  log_csv "$OUT" pull daemon "$name" "$files" "$bytes" robosync "$sec" "$rate" "host=$REMOTE_HOST"
}

pull_rsync() {
  local name="$1" dst="$ROOT/pull_rsync_${name}"
  ensure_clean_dir "$dst"
  [ -n "$RSYNC_REMOTE" ] || { echo "[pull] skipping rsync; set RSYNC_REMOTE=user@host"; return 0; }
  local src_path="$REMOTE_ROOT/$name/"
  echo "[pull] rsync dataset=$name"
  local t0=$(date +%s%N)
  rsync -a --delete -e ssh "$RSYNC_REMOTE:$src_path" "$dst/" >/dev/null 2>&1 || true
  local sec=$(secs_since "$t0")
  local bytes=$(size_bytes "$dst")
  local files=$(count_files "$dst")
  local rate=$(mbps "$bytes" "$sec")
  log_csv "$OUT" pull ssh "$name" "$files" "$bytes" rsync "$sec" "$rate" "remote=$RSYNC_REMOTE"
}

for name in small mixed large; do
  for i in $(seq 1 "$ITER"); do
    pull_robo "$name"
    pull_rsync "$name"
  done
done

echo "Results: $OUT"

