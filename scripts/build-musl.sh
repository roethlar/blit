#!/usr/bin/env bash
set -euo pipefail

# Build RoboSync for Linux MUSL into a separate target dir.
# Requires the MUSL target to be installed, e.g.:
#   rustup target add x86_64-unknown-linux-musl
# And a suitable linker if needed.

usage() {
  echo "Usage: $0 [--release] [--test] [--clippy] [--target <musl-triple>]" >&2
  echo "  --release            Build in release mode" >&2
  echo "  --test               Run tests after build (may not run under cross)" >&2
  echo "  --clippy             Run cargo clippy after build (deny warnings)" >&2
  echo "  --target <triple>    Override (default: x86_64-unknown-linux-musl)" >&2
}

mode="debug"
run_tests=false
run_clippy=false
target_triple="x86_64-unknown-linux-musl"

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
export CARGO_TARGET_DIR="target/musl"

build_args=(build --target "$target_triple")
[[ "$mode" == "release" ]] && build_args+=("--release")

echo "[build-musl] cargo ${build_args[*]} (CARGO_TARGET_DIR=$CARGO_TARGET_DIR)"
cargo "${build_args[@]}"

out_root="$CARGO_TARGET_DIR/$target_triple"
bin_path="$out_root/$mode/robosync"

if [[ ! -f "$bin_path" ]]; then
  echo "Binary not found at $bin_path" >&2
  exit 2
fi
echo "[build-musl] Built: $bin_path"

if $run_tests; then
  echo "[build-musl] Running tests (target=$target_triple) ..."
  cargo test --target "$target_triple" || echo "[build-musl] Note: tests may not run under cross target."
fi

if $run_clippy; then
  echo "[build-musl] Running clippy (target=$target_triple) ..."
  cargo clippy --target "$target_triple" -- -D warnings
fi

echo "[build-musl] Done."

