#!/usr/bin/env bash
set -euo pipefail

mode=${1:-async} # async|classic

echo "Building (release)"
cargo build --release >/dev/null
bin_blit="$(pwd)/target/release/blit"
bin_blitd="$(pwd)/target/release/blitd"

BASE_DIR="scripts/smoketests/macos"
mkdir -p "$BASE_DIR"
tmp="$BASE_DIR/run-$(date +%s)-$$"
mkdir -p "$tmp"
src="$tmp/src"; dst="$tmp/dst"; pull="$tmp/pull"
mkdir -p "$src" "$dst" "$pull"

echo hello >"$src/a.txt"
mkdir -p "$src/sub"
head -c 2048 </dev/zero | tr '\0' 'x' >"$src/sub/b.txt"

# Try a symlink (may fail without privileges)
ln -s a.txt "$src/alink.txt" 2>/dev/null || true

find_port() { for p in {20000..40000}; do if ! (exec 3<>/dev/tcp/127.0.0.1/$p) 2>/dev/null; then echo "$p"; return; fi; done; echo 9031; }
wait_up() { local port=$1; local tries=60; while [ $tries -gt 0 ]; do (exec 3<>/dev/tcp/127.0.0.1/$port) >/dev/null 2>&1 && return 0; sleep 0.1; tries=$((tries-1)); done; return 1; }

# Push: start daemon (TLS default), then push with blit:// (TLS default)
port=$(find_port)
"$bin_blitd" --root "$dst" --bind "127.0.0.1:$port" >"$tmp/daemon_push.log" 2>&1 & spid=$!
trap 'kill $spid 2>/dev/null || true' EXIT
wait_up "$port" || { echo "daemon push failed"; sed -n '1,200p' "$tmp/daemon_push.log"; exit 1; }

"$bin_blit" "$src" "blit://127.0.0.1:$port/" --mir -v >"$tmp/client_push.log" 2>&1 || {
  echo "push failed"; echo "see $tmp/client_push.log and $tmp/daemon_push.log";
  sed -n '1,200p' "$tmp/client_push.log"; sed -n '1,200p' "$tmp/daemon_push.log"; exit 1; }

test -f "$dst/a.txt" && test -f "$dst/sub/b.txt" || { echo "push verification failed"; exit 1; }
kill $spid 2>/dev/null || true

# Pull: start daemon (TLS default), then pull with blit:// (TLS default)
port2=$(find_port)
"$bin_blitd" --root "$src" --bind "127.0.0.1:$port2" >"$tmp/daemon_pull.log" 2>&1 & spid2=$!
trap 'kill $spid2 2>/dev/null || true' EXIT
wait_up "$port2" || { echo "daemon pull failed"; sed -n '1,200p' "$tmp/daemon_pull.log"; exit 1; }

"$bin_blit" "blit://127.0.0.1:$port2/" "$pull" --mir -v >"$tmp/client_pull.log" 2>&1 || {
  echo "pull failed"; echo "see $tmp/client_pull.log and $tmp/daemon_pull.log";
  sed -n '1,200p' "$tmp/client_pull.log"; sed -n '1,200p' "$tmp/daemon_pull.log"; exit 1; }

test -f "$pull/a.txt" && test -f "$pull/sub/b.txt" || { echo "pull verification failed"; exit 1; }
kill $spid2 2>/dev/null || true

echo "${mode^} smoke OK"
