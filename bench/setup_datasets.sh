#!/usr/bin/env bash
set -euo pipefail
HERE=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "$HERE/common.sh"

# Tunable scales
SCALE_SMALL_FILES="${SCALE_SMALL_FILES:-2000}"   # number of ~4KB files
SCALE_MIXED_DIRS="${SCALE_MIXED_DIRS:-200}"
SCALE_MIXED_FILES_PER_DIR="${SCALE_MIXED_FILES_PER_DIR:-10}"
SCALE_LARGE_FILES="${SCALE_LARGE_FILES:-5}"    # number of ~200MB files

ROOT="$BENCH_ROOT"
SRC_SMALL="$ROOT/src_small"
SRC_MIXED="$ROOT/src_mixed"
SRC_LARGE="$ROOT/src_large"

echo "Creating datasets under $ROOT ..."
mkdir -p "$ROOT"

create_small() {
  ensure_clean_dir "$SRC_SMALL"
  mkdir -p "$SRC_SMALL/s"
  echo "- small: $SCALE_SMALL_FILES files x ~4KB"
  for i in $(seq -w 1 "$SCALE_SMALL_FILES"); do
    head -c 4096 </dev/urandom >"$SRC_SMALL/s/f_$i.bin"
  done
}

create_mixed() {
  ensure_clean_dir "$SRC_MIXED"
  echo "- mixed: $SCALE_MIXED_DIRS dirs x $SCALE_MIXED_FILES_PER_DIR files"
  for d in $(seq -w 1 "$SCALE_MIXED_DIRS"); do
    mkdir -p "$SRC_MIXED/d_$d/empty_$d"
    for f in $(seq -w 1 "$SCALE_MIXED_FILES_PER_DIR"); do
      # random sizes 1–64KB
      sz=$(( (RANDOM % 64 + 1) * 1024 ))
      head -c "$sz" </dev/urandom >"$SRC_MIXED/d_$d/f_${d}_${f}.bin"
    done
  done
  # sprinkle some symlinks
  ln -s "d_001/f_001_01.bin" "$SRC_MIXED/link_one"
}

create_large() {
  ensure_clean_dir "$SRC_LARGE"
  echo "- large: $SCALE_LARGE_FILES files x ~200MB"
  for i in $(seq -w 1 "$SCALE_LARGE_FILES"); do
    dd if=/dev/zero of="$SRC_LARGE/L_$i.bin" bs=1M count=200 status=none
  done
}

create_small
create_mixed
create_large

echo "Datasets ready:"
for d in "$SRC_SMALL" "$SRC_MIXED" "$SRC_LARGE"; do
  echo "  $(basename "$d"): $(count_files "$d") files, $(size_bytes "$d") bytes"
done

