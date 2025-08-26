#!/usr/bin/env bash
set -euo pipefail

mode=${1:-async} # async|classic

echo "Building (release)"
cargo build --release >/dev/null
bin="$(pwd)/target/release/robosync"

tmp=$(mktemp -d -t robosmoke.XXXX)
src="$tmp/src"; dst="$tmp/dst"; pull="$tmp/pull"
mkdir -p "$src" "$dst" "$pull"

echo hello >"$src/a.txt"
mkdir -p "$src/sub"
head -c 2048 </dev/zero | tr '\0' 'x' >"$src/sub/b.txt"

# Try a symlink (may fail without privileges)
ln -s a.txt "$src/alink.txt" 2>/dev/null || true

serve_flag="--serve-async"; [[ "$mode" == classic ]] && serve_flag="--serve"

port=9031
"$bin" $serve_flag --bind 127.0.0.1:$port --root "$dst" & spid=$!
sleep 1
trap 'kill $spid 2>/dev/null || true' EXIT

"$bin" "$src" "robosync://127.0.0.1:$port/" --mir >/dev/null
test -f "$dst/a.txt" && test -f "$dst/sub/b.txt" || { echo "push failed"; exit 1; }
kill $spid 2>/dev/null || true

port2=9032
"$bin" $serve_flag --bind 127.0.0.1:$port2 --root "$src" & spid2=$!
sleep 1
trap 'kill $spid2 2>/dev/null || true' EXIT

"$bin" "robosync://127.0.0.1:$port2/" "$pull" --mir >/dev/null
test -f "$pull/a.txt" && test -f "$pull/sub/b.txt" || { echo "pull failed"; exit 1; }
kill $spid2 2>/dev/null || true

echo "${mode^} smoke OK"

