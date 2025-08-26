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

# Parity check: logical file count and total bytes should match src
src_count=$(find "${src}" -type f | wc -l | awk '{print $1}')
pull_count=$(find "${pull}" -type f | wc -l | awk '{print $1}')
if [ "${src_count}" -ne "${pull_count}" ]; then
  echo "parity failed: file count mismatch src=${src_count} pull=${pull_count}"; exit 1
fi
# Sum sizes (GNU find -printf is available on Linux runner); fallback to du -b if needed
if find --version >/dev/null 2>&1; then
  src_bytes=$(find "${src}" -type f -printf '%s
' | awk '{s+=$1} END{print s+0}')
  pull_bytes=$(find "${pull}" -type f -printf '%s
' | awk '{s+=$1} END{print s+0}')
else
  src_bytes=$(du -b -c "${src}" | tail -n1 | awk '{print $1}')
  pull_bytes=$(du -b -c "${pull}" | tail -n1 | awk '{print $1}')
fi
if [ "${src_bytes}" != "${pull_bytes}" ]; then
  echo "parity failed: byte total mismatch src=${src_bytes} pull=${pull_bytes}"; exit 1
fi

echo "${mode^} smoke OK"

