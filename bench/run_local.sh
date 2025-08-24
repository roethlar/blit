#!/usr/bin/env bash
set -euo pipefail
HERE=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "$HERE/common.sh"
check_tools

ROOT="$BENCH_ROOT"
OUT="$RESULTS_DIR/local.csv"

run_case() { # usage: run_case <dataset_path> <dataset_name>
  local src="$1" name="$2"
  local dst_rsync="$ROOT/dst_rsync_${name}"
  local dst_robo="$ROOT/dst_robo_${name}"
  local bytes files
  bytes=$(size_bytes "$src")
  files=$(count_files "$src")
  echo "[local] dataset=$name files=$files bytes=$bytes"

  for i in $(seq 1 "$ITER"); do
    # rsync
    ensure_clean_dir "$dst_rsync"
    local t0=$(date +%s%N)
    rsync -a --delete "$src/" "$dst_rsync/" >/dev/null 2>&1 || true
    local sec=$(secs_since "$t0")
    local rate=$(mbps "$bytes" "$sec")
    log_csv "$OUT" local copy "$name" "$files" "$bytes" rsync "$sec" "$rate" "iter=$i"

    # robosync
    ensure_clean_dir "$dst_robo"
    t0=$(date +%s%N)
    "$ROBOSYNC" "$src" "$dst_robo" --mir >/dev/null 2>&1 || true
    sec=$(secs_since "$t0")
    rate=$(mbps "$bytes" "$sec")
    log_csv "$OUT" local copy "$name" "$files" "$bytes" robosync "$sec" "$rate" "iter=$i"
  done
}

run_case "$ROOT/src_small" small
run_case "$ROOT/src_mixed" mixed
run_case "$ROOT/src_large" large

echo "Results: $OUT"

