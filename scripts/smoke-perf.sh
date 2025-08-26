#!/usr/bin/env bash
set -euo pipefail

PORT="18041"
ROOT="perf_tmp/server_root"
SRC="perf_tmp/src"
SIZE_MB="2048" # 2 GiB test (adjust)

cleanup() {
  if [[ -f perf_tmp/daemon.pid ]]; then
    kill "$(cat perf_tmp/daemon.pid)" 2>/dev/null || true
    rm -f perf_tmp/daemon.pid
  fi
}
trap cleanup EXIT

echo "[1/5] Building robosync (release recommended)"
cargo build -q

echo "[2/5] Preparing ${SIZE_MB}MB test file"
rm -rf perf_tmp
mkdir -p "$ROOT" "$SRC"
dd if=/dev/zero of="$SRC/large.bin" bs=1M count="$SIZE_MB" status=none

echo "[3/5] Starting async daemon on port $PORT"
target/debug/robosync daemon "$ROOT" "$PORT" >/dev/null 2>&1 &
echo $! > perf_tmp/daemon.pid
sleep 0.7

echo "[4/5] Pushing with 8 workers and 8MB chunks"
ts=$(date +%s)
target/debug/robosync mirror "$SRC" "robosync://127.0.0.1:${PORT}/dst" --net-workers 8 --net-chunk-mb 8 --ludicrous-speed -v
te=$(date +%s)
dur=$((te-ts))
bytes=$((SIZE_MB*1024*1024))
mbps=$(awk -v b=$bytes -v d=$dur 'BEGIN{if(d==0) d=1; print (b/1024/1024)/d}')
echo "Throughput: ${mbps} MB/s over ${dur}s"

echo "[5/5] Done"
