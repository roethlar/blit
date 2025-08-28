#!/usr/bin/env bash
set -euo pipefail

PORT="18041"
ROOT="scripts/smoketests/perf/server_root"
SRC="scripts/smoketests/perf/src"
SIZE_MB="2048" # 2 GiB test (adjust)

cleanup() {
  if [[ -f scripts/smoketests/perf/daemon.pid ]]; then
    kill "$(cat scripts/smoketests/perf/daemon.pid)" 2>/dev/null || true
    rm -f scripts/smoketests/perf/daemon.pid
  fi
}
trap cleanup EXIT

echo "[1/5] Building blit (release recommended)"
cargo build -q

echo "[2/5] Preparing ${SIZE_MB}MB test file"
rm -rf scripts/smoketests/perf
mkdir -p "$ROOT" "$SRC"
dd if=/dev/zero of="$SRC/large.bin" bs=1M count="$SIZE_MB" status=none

echo "[3/5] Starting async daemon on port $PORT"
target/debug/blit daemon "$ROOT" "$PORT" >/dev/null 2>&1 &
echo $! > scripts/smoketests/perf/daemon.pid
sleep 0.7

echo "[4/5] Pushing with 8 workers and 8MB chunks"
ts=$(date +%s)
target/debug/blit mirror "$SRC" "blit://127.0.0.1:${PORT}/dst" --net-workers 8 --net-chunk-mb 8 --ludicrous-speed -v
te=$(date +%s)
dur=$((te-ts))
bytes=$((SIZE_MB*1024*1024))
mbps=$(awk -v b=$bytes -v d=$dur 'BEGIN{if(d==0) d=1; print (b/1024/1024)/d}')
echo "Throughput: ${mbps} MB/s over ${dur}s"

echo "[5/5] Done"
