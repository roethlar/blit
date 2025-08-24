#!/usr/bin/env bash
# Local cache flush helper (manual). Requires privileges.

set -euo pipefail
OS=$(uname -s)

echo "=== Local cache flush ==="
if [[ "$OS" == "Linux" ]]; then
  echo "Running: sync; echo 3 > /proc/sys/vm/drop_caches (requires root)"
  sync || true
  if [[ $EUID -ne 0 ]]; then
    echo "Attempting sudo..."
    sudo sh -c 'echo 3 > /proc/sys/vm/drop_caches'
  else
    echo 3 > /proc/sys/vm/drop_caches
  fi
  echo "Done."
elif [[ "$OS" == "Darwin" ]]; then
  echo "Running: sync; sudo purge"
  sync || true
  sudo purge || true
  echo "Done."
else
  echo "Unsupported OS: $OS" >&2
  exit 1
fi

