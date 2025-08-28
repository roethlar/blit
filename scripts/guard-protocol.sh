#!/usr/bin/env bash
set -euo pipefail

# Fails if MAGIC or VERSION protocol constants are defined outside src/protocol.rs
cd "$(dirname "$0")/.."

bad=$(rg -n "\bconst\s+(MAGIC|VERSION)\b" src | rg -v '^src/protocol.rs:' || true)
if [[ -n "$bad" ]]; then
  echo "Protocol constants must be defined only in src/protocol.rs" >&2
  echo "$bad" >&2
  exit 1
fi
echo "Protocol constants guard: OK"

