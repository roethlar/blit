#!/usr/bin/env bash
set -euo pipefail

# macOS build helper (defaults to release)
# Use --debug (script flag) to build debug

BUILD_MODE=release
TARGET=""
OTHER_ARGS=()

while (( "$#" )); do
  case "$1" in
    --debug)
      BUILD_MODE=debug; shift ;;
    --release)
      shift ;;
    --target)
      TARGET=${2:-}; shift 2 ;;
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
