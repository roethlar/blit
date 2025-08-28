#!/usr/bin/env bash
set -euo pipefail

# Windows build helper (GNU/MSVC)
# Defaults to release, can opt-in to debug with --debug (script flag)
# Examples:
#   scripts/build-windows.sh                 # release, host target
#   scripts/build-windows.sh --target x86_64-pc-windows-gnu
#   scripts/build-windows.sh --debug         # debug, host target
#   scripts/build-windows.sh --msvc          # release, MSVC target

BUILD_MODE=release
TARGET=""
OTHER_ARGS=()

while (( "$#" )); do
  case "$1" in
    --debug)
      BUILD_MODE=debug; shift ;;
    --release)
      # Accept but ignore; we default to release
      shift ;;
    --target)
      TARGET=${2:-}; shift 2 ;;
    --msvc)
      # Prefer MSVC default target when requested
      TARGET=${TARGET:-x86_64-pc-windows-msvc}; shift ;;
    *)
      OTHER_ARGS+=("$1"); shift ;;
  esac
done

if [[ -z "$TARGET" ]]; then
  TARGET=$(rustc -vV | awk '/^host: /{print $2}')
fi

TARGET_DIR="$(pwd)/target/${TARGET}"

set -x
cargo build \
  ${BUILD_MODE:+--release} \
  --target "$TARGET" \
  --target-dir "$TARGET_DIR" \
  "${OTHER_ARGS[@]}"
set +x
