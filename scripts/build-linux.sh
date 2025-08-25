#!/usr/bin/env bash
set -euo pipefail

# Build RoboSync for Linux into a separate target dir to avoid collisions
# with other OS builds. Run this on a Linux host, or pass --target for
# cross-compilation if your toolchain supports it.

usage() {
  echo "Usage: $0 [--release] [--test] [--clippy] [--target <triple>]" >&2
  echo "  --release        Build in release mode" >&2
  echo "  --test           Run cargo test after build" >&2
  echo "  --clippy         Run cargo clippy after build (deny warnings)" >&2
  echo "  --target <triple>  Override target triple (e.g., x86_64-unknown-linux-gnu)" >&2
}

mode="debug"
run_tests=false
run_clippy=false
target_triple=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release) mode="release"; shift ;;
    --test) run_tests=true; shift ;;
    --clippy) run_clippy=true; shift ;;
    --target) target_triple="${2:-}"; [[ -n "$target_triple" ]] || { echo "--target requires a value" >&2; exit 1; }; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage; exit 1 ;;
  esac
done

export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="target/linux"

build_args=(build)
[[ "$mode" == "release" ]] && build_args+=("--release")
if [[ -n "$target_triple" ]]; then
  build_args+=("--target" "$target_triple")
fi

echo "[build-linux] cargo ${build_args[*]} (CARGO_TARGET_DIR=$CARGO_TARGET_DIR)"
cargo "${build_args[@]}"

out_root="$CARGO_TARGET_DIR"
[[ -n "$target_triple" ]] && out_root="$out_root/$target_triple"
bin_path="$out_root/$mode/robosync"

if [[ ! -f "$bin_path" ]]; then
  echo "Binary not found at $bin_path" >&2
  exit 2
fi
echo "[build-linux] Built: $bin_path"

if $run_tests; then
  echo "[build-linux] Running tests ..."
  if [[ -n "$target_triple" ]]; then
    cargo test --target "$target_triple"
  else
    cargo test
  fi
fi

if $run_clippy; then
  echo "[build-linux] Running clippy ..."
  if [[ -n "$target_triple" ]]; then
    cargo clippy --target "$target_triple" -- -D warnings
  else
    cargo clippy -- -D warnings
  fi
fi

echo "[build-linux] Done."

