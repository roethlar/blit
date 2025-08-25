#!/usr/bin/env bash
set -euo pipefail

# Build RoboSync for macOS into a separate target dir to avoid collisions
# with Linux builds and to sidestep incremental lock issues on network volumes.

usage() {
  echo "Usage: $0 [--release] [--test] [--clippy]" >&2
  echo "  --release  Build in release mode" >&2
  echo "  --test     Run cargo test after build" >&2
  echo "  --clippy   Run cargo clippy after build (deny warnings)" >&2
}

mode="debug"
run_tests=false
run_clippy=false

for arg in "$@"; do
  case "$arg" in
    --release) mode="release" ;;
    --test) run_tests=true ;;
    --clippy) run_clippy=true ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $arg" >&2; usage; exit 1 ;;
  esac
done

export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="target/macos"

if [[ "$mode" == "release" ]]; then
  echo "[build-macos] Building release to $CARGO_TARGET_DIR/release ..."
  cargo build --release
  bin_path="$CARGO_TARGET_DIR/release/robosync"
else
  echo "[build-macos] Building debug to $CARGO_TARGET_DIR/debug ..."
  cargo build
  bin_path="$CARGO_TARGET_DIR/debug/robosync"
fi

if [[ ! -f "$bin_path" ]]; then
  echo "Build finished but binary not found at $bin_path" >&2
  exit 2
fi

echo "[build-macos] Built: $bin_path"

if $run_tests; then
  echo "[build-macos] Running tests ..."
  cargo test -q
fi

if $run_clippy; then
  echo "[build-macos] Running clippy ..."
  cargo clippy -- -D warnings
fi

echo "[build-macos] Done."

