#!/usr/bin/env bash
# One-off rsync daemon runner (no /etc changes). Intended for TrueNAS or any host
# where you can't edit system config. Runs in the foreground so you can Ctrl-C.

set -euo pipefail

# Configurable via env vars or flags
RSYNC_PORT="${RSYNC_PORT:-9032}"
RSYNC_ROOT="${RSYNC_ROOT:-/mnt/specific-pool/home}"
TMPDIR_BASE="${TMPDIR_BASE:-/tmp}"

while [[ ${1-} =~ ^- ]]; do
  case "$1" in
    --port) RSYNC_PORT="$2"; shift 2;;
    --root) RSYNC_ROOT="$2"; shift 2;;
    *) echo "Unknown option: $1" >&2; exit 1;;
  esac
done

CONF_DIR="$TMPDIR_BASE/rsyncd_$$"
CONF="$CONF_DIR/rsyncd.conf"
LOG="$CONF_DIR/rsyncd.log"
PIDF="$CONF_DIR/rsyncd.pid"

mkdir -p "$CONF_DIR"

DOWNLOADS="$RSYNC_ROOT/downloads"
TEST_DLS="$RSYNC_ROOT/test-dls"

cat >"$CONF" <<EOF
use chroot = no
uid = root
gid = root
max connections = 16
pid file = $PIDF
log file = $LOG

[downloads]
    path = $DOWNLOADS
    read only = false
    list = yes
    comment = downloads module

[test-dls]
    path = $TEST_DLS
    read only = false
    list = yes
    comment = test-dls module
EOF

echo "=== rsyncd (one-off) ==="
echo "Root:   $RSYNC_ROOT"
echo "Modules: downloads -> $DOWNLOADS; test-dls -> $TEST_DLS"
echo "Port:   $RSYNC_PORT"
echo "Config: $CONF"
echo "Logs:   $LOG"
echo
echo "Command: rsync --daemon --no-detach --port $RSYNC_PORT --config $CONF"
echo "Press Ctrl-C to stop."

exec rsync --daemon --no-detach --port "$RSYNC_PORT" --config "$CONF"

