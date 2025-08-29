#!/usr/bin/env bash
set -euo pipefail

mode=${1:-async} # async|classic

echo "Building (release)"
cargo build --release >/dev/null
bin_blit="$(pwd)/target/release/blit"
bin_blitd="$(pwd)/target/release/blitd"

BASE_DIR="scripts/smoketests/linux"
mkdir -p "$BASE_DIR"
tmp="$BASE_DIR/run-$(date +%s)-$$"
mkdir -p "$tmp"
src="$tmp/src"; dst="$tmp/dst"; pull="$tmp/pull"
logdir="$tmp/logs"
mkdir -p "$src" "$dst" "$pull" "$logdir"

echo hello >"$src/a.txt"
mkdir -p "$src/sub"
head -c 2048 </dev/zero | tr '\0' 'x' >"$src/sub/b.txt"

# Skip symlink creation for now - focus on file transfer stability
# ln -s a.txt "$src/alink.txt" 2>/dev/null || true

# Function to find an available port
find_free_port() {
    local port
    for port in {9030..9100}; do
        # Try to bind to the port using bash's /dev/tcp
        if ! (exec 2>/dev/null; echo > /dev/tcp/localhost/$port); then
            echo "$port"
            return
        fi
    done
    echo "9031"  # fallback
}

# Function to wait for daemon to be ready
wait_for_daemon() {
    local logfile=$1
    local port=$2
    local max_tries=50
    local tries=0
    while [ $tries -lt $max_tries ]; do
        # Check if daemon logged that it's listening
        if grep -q "listening on" "$logfile" 2>/dev/null; then
            # Try to connect to the port
            if (exec 2>/dev/null; echo > /dev/tcp/localhost/$port); then
                return 0
            fi
        fi
        sleep 0.1
        tries=$((tries + 1))
    done
    return 1
}

# Test 1: Push to daemon
port=$(find_free_port)
echo "Starting daemon on port $port for push test..."
"$bin_blitd" --root "$dst" --bind "127.0.0.1:$port" --never-tell-me-the-odds >"$logdir/daemon1.log" 2>&1 & spid=$!
trap 'kill $spid 2>/dev/null || true' EXIT

if ! wait_for_daemon "$logdir/daemon1.log" "$port"; then
    echo "ERROR: Daemon failed to start on port $port"
    cat "$logdir/daemon1.log" >&2
    exit 1
fi

echo "Pushing files to daemon..."
"$bin_blit" "$src" "blit://127.0.0.1:$port/" --mir --never-tell-me-the-odds >"$logdir/push.log" 2>&1 || {
    echo "ERROR: Push failed"
    cat "$logdir/push.log" >&2
    exit 1
}

test -f "$dst/a.txt" && test -f "$dst/sub/b.txt" || { 
    echo "ERROR: Push verification failed - expected files not found"
    ls -la "$dst/" >&2
    exit 1
}
kill $spid 2>/dev/null || true
wait $spid 2>/dev/null || true

# Test 2: Pull from daemon
# Use a different port range to avoid conflicts
port2=$(($(find_free_port) + 10))
echo "Starting daemon on port $port2 for pull test..."
"$bin_blitd" --root "$src" --bind "127.0.0.1:$port2" --never-tell-me-the-odds >"$logdir/daemon2.log" 2>&1 & spid2=$!
trap 'kill $spid2 2>/dev/null || true' EXIT

if ! wait_for_daemon "$logdir/daemon2.log" "$port2"; then
    echo "ERROR: Daemon failed to start on port $port2"
    cat "$logdir/daemon2.log" >&2
    exit 1
fi

echo "Pulling files from daemon..."
"$bin_blit" "blit://127.0.0.1:$port2/" "$pull" --mir --never-tell-me-the-odds >"$logdir/pull.log" 2>&1 || {
    echo "ERROR: Pull failed"
    cat "$logdir/pull.log" >&2
    exit 1
}

test -f "$pull/a.txt" && test -f "$pull/sub/b.txt" || { 
    echo "ERROR: Pull verification failed - expected files not found"
    ls -la "$pull/" >&2
    exit 1
}
kill $spid2 2>/dev/null || true
wait $spid2 2>/dev/null || true

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

echo "${mode^} smoke tests PASSED"

# Clean up temp directory
rm -rf "$tmp" 2>/dev/null || true
