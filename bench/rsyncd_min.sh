#!/usr/bin/env bash
# Minimal one-off rsync daemon for TrueNAS (no /etc changes).
# Modules:
#  - downloads -> /mnt/specific-pool/home/downloads
#  - test-dls  -> /mnt/specific-pool/home/test-dls

set -euo pipefail

PORT="${RSYNC_PORT:-9032}"
ROOT="${RSYNC_ROOT:-/mnt/specific-pool/home}"
CONF="/tmp/rsyncd_min.conf"

cat >"$CONF" <<EOF
use chroot = no
max connections = 16
uid = root
gid = root

[downloads]
  path = $ROOT/downloads
  read only = false

[test-dls]
  path = $ROOT/test-dls
  read only = false
EOF

echo "rsyncd: config=$CONF port=$PORT"
exec rsync --daemon --no-detach --port "$PORT" --config "$CONF"

