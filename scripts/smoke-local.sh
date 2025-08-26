#!/usr/bin/env bash
set -euo pipefail

PORT="18031"
ROOT="smoke_tmp/server_root"
SRC="smoke_tmp/client_src"
PULLDST="smoke_tmp/pull_dst"

cleanup() {
  if [[ -f smoke_tmp/daemon.pid ]]; then
    kill "$(cat smoke_tmp/daemon.pid)" 2>/dev/null || true
    rm -f smoke_tmp/daemon.pid
  fi
}
trap cleanup EXIT

echo "[1/6] Building robosync (debug)"
cargo build -q

echo "[2/6] Preparing test data"
rm -rf smoke_tmp
mkdir -p "$ROOT" "$SRC/sub" "$PULLDST"
printf 'hello world\n' > "$SRC/a.txt"
head -c 1048576 </dev/urandom > "$SRC/sub/b.bin"
mkdir -p "$SRC/emptydir"
# Create an extra file in pull destination (will be deleted by mirror)
printf 'extra\n' > "$PULLDST/extra.txt"
# Best-effort symlink on Unix
if [[ "$(uname -s)" != MINGW* && "$(uname -s)" != CYGWIN* && "$(uname -s)" != *NT* ]]; then
  ln -s a.txt "$SRC/link_to_a" || true
fi

echo "[3/6] Starting async daemon on port $PORT"
target/debug/robosync daemon "$ROOT" "$PORT" >/dev/null 2>&1 &
echo $! > smoke_tmp/daemon.pid
sleep 0.7

echo "[4/6] Mirror local → remote"
target/debug/robosync mirror "$SRC" "robosync://127.0.0.1:${PORT}/dst" -v

test -f "$ROOT/dst/a.txt"
test -f "$ROOT/dst/sub/b.bin"
test -d "$ROOT/dst/emptydir"

echo "[5/6] Mirror remote → local (pull back)"
target/debug/robosync mirror "robosync://127.0.0.1:${PORT}/dst" "$PULLDST" -v

# Ensure mirror deleted extras
test ! -f "$PULLDST/extra.txt"

echo "[6/6] Verify trees are identical"
target/debug/robosync verify "$SRC" "$PULLDST" --json --limit 5 | tee smoke_tmp/verify.json
echo "OK: smoke completed"
