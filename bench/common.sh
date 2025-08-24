#!/usr/bin/env bash
set -euo pipefail

# Common helpers for RoboSync vs rsync benchmarks

ROBOSYNC="${ROBOSYNC:-$(pwd)/target/release/robosync}"
BENCH_ROOT="${BENCH_ROOT:-/tmp/robosync_bench}"
RESULTS_DIR="${RESULTS_DIR:-bench/results}"
ITER="${ITER:-1}"

mkdir -p "$RESULTS_DIR"

ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }

size_bytes() { # usage: size_bytes <path>
  if du -sb . >/dev/null 2>&1; then du -sb "$1" | awk '{print $1}'; else du -sk "$1" | awk '{print $1*1024}'; fi
}

count_files() { find "$1" -type f | wc -l | tr -d '[:space:]'; }

secs_since() { # usage: secs_since <nanostamp>
  local start_ns=$1
  local end_ns
  end_ns=$(date +%s%N)
  awk -v s="$start_ns" -v e="$end_ns" 'BEGIN{printf "%.3f", (e-s)/1000000000}'
}

mbps() { # usage: mbps <bytes> <seconds>
  awk -v b="$1" -v s="$2" 'BEGIN{if(s>0) printf "%.2f", (b/1048576.0)/s; else print "0.00"}'
}

ensure_clean_dir() { # usage: ensure_clean_dir <dir>
  local d="$1"
  rm -rf "$d"
  mkdir -p "$d"
}

log_csv() { # usage: log_csv <file> <test> <mode> <dataset> <files> <bytes> <tool> <seconds> <mbps> <notes>
  local csv="$1"; shift
  if [ ! -f "$csv" ]; then echo "timestamp,test,mode,dataset,files,bytes,tool,seconds,mbps,notes" > "$csv"; fi
  echo "$(ts),$*" >> "$csv"
}

check_tools() {
  command -v rsync >/dev/null || { echo "rsync not found" >&2; exit 1; }
  [ -x "$ROBOSYNC" ] || { echo "RoboSync not found at $ROBOSYNC (set ROBOSYNC env)" >&2; exit 1; }
}

# Cache flushing (best-effort). Requires privileges on Linux.
flush_local_cache() {
  if [[ "$(uname -s)" == "Linux" ]]; then
    sync || true
    if [[ $EUID -ne 0 ]]; then
      echo "[warn] drop_caches needs root; attempting sudo..." >&2
      sudo sh -c 'echo 3 > /proc/sys/vm/drop_caches' || echo "[warn] sudo drop_caches failed" >&2
    else
      echo 3 > /proc/sys/vm/drop_caches || true
    fi
  elif [[ "$(uname -s)" == "Darwin" ]]; then
    sync || true
    sudo purge || true
  fi
}

flush_remote_cache() { # usage: flush_remote_cache user@host
  local remote="${1:-}"
  [[ -n "$remote" ]] || { echo "[warn] remote SSH not set; skipping remote cache flush" >&2; return 0; }
  ssh -o BatchMode=yes "$remote" 'sync; (echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null 2>&1) || true' || echo "[warn] remote flush failed" >&2
}

remote_rmrf() { # usage: remote_rmrf user@host /path
  local remote="$1" path="$2"
  [[ -n "$remote" ]] || { echo "[warn] remote SSH not set; skipping remote rm -rf $path" >&2; return 0; }
  ssh -o BatchMode=yes "$remote" "rm -rf -- '$(printf %q "$path")'" || echo "[warn] remote rm failed: $path" >&2
}

