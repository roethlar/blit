#!/usr/bin/env bash
set -euo pipefail

mode=${1:-async} # async|classic

echo "Building (release)"
cargo build --release >/dev/null
bin="$(pwd)/target/release/blit"

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

port=$(jot -r 1 20000 40000)
"$bin" daemon "$dst" "$port" >"$tmp/daemon_push.log" 2>&1 & spid=$!
retries=40; while [ $retries -gt 0 ]; do (exec 3<>/dev/tcp/127.0.0.1/$port) >/dev/null 2>&1 && break; sleep 0.1; retries=$((retries-1)); done
trap 'kill $spid 2>/dev/null || true' EXIT

"$bin" "$src" "blit://127.0.0.1:$port/" --mir -v >"$tmp/client_push.log" 2>&1 || { echo "push failed"; echo "see $tmp/client_push.log and $tmp/daemon_push.log"; exit 1; }
test -f "$dst/a.txt" && test -f "$dst/sub/b.txt" || { echo "push failed"; exit 1; }
kill $spid 2>/dev/null || true

port2=$(jot -r 1 20000 40000)
"$bin" daemon "$src" "$port2" >"$tmp/daemon_pull.log" 2>&1 & spid2=$!
retries=40; while [ $retries -gt 0 ]; do (exec 3<>/dev/tcp/127.0.0.1/$port2) >/dev/null 2>&1 && break; sleep 0.1; retries=$((retries-1)); done
trap 'kill $spid2 2>/dev/null || true' EXIT

"$bin" "blit://127.0.0.1:$port2/" "$pull" --mir -v >"$tmp/client_pull.log" 2>&1 || { echo "pull failed"; echo "see $tmp/client_pull.log and $tmp/daemon_pull.log"; exit 1; }
test -f "$pull/a.txt" && test -f "$pull/sub/b.txt" || { echo "pull failed"; exit 1; }
kill $spid2 2>/dev/null || true

echo "${mode^} smoke OK"

