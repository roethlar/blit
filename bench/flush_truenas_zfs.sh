#!/usr/bin/env bash
# TrueNAS (ZFS) cache flush helper. No system config changes; prints and runs safe commands.
# For TrueNAS SCALE (Linux): drop page cache. ZFS ARC cannot be fully flushed without tuning.
# For TrueNAS CORE (FreeBSD): prints guidance; does not attempt ARC changes.

set -euo pipefail
OS=$(uname -s)

echo "=== TrueNAS (ZFS) cache flush helper ==="
if [[ "$OS" == "Linux" ]]; then
  echo "Detected Linux (TrueNAS SCALE)." 
  echo "1) sync"
  sync || true
  echo "2) Drop page cache: echo 3 > /proc/sys/vm/drop_caches (requires root)"
  if [[ $EUID -ne 0 ]]; then
    echo "Attempting sudo..."
    sudo sh -c 'echo 3 > /proc/sys/vm/drop_caches'
  else
    echo 3 > /proc/sys/vm/drop_caches
  fi
  echo "Note: ZFS ARC is not fully flushed by drop_caches; for stricter cold runs, consider a reboot between tests."
elif [[ "$OS" == "FreeBSD" ]]; then
  echo "Detected FreeBSD (TrueNAS CORE)." 
  echo "Run as root on the NAS:"
  echo "  sync"
  echo "  sysctl vfs.zfs.arc_free_target=1048576   # nudge ARC reclaim (value optional)"
  echo "  sysctl vfs.zfs.prefetch_disable=1        # optional during tests"
  echo "Note: Safest way to fully clear ARC is a reboot; TrueNAS often restricts ARC tuning at runtime."
else
  echo "Unknown OS: $OS" >&2
  exit 1
fi

