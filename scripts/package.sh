#!/usr/bin/env bash
set -euo pipefail

# Build release binaries and package them into a versioned archive under dist/

root_dir=$(cd "$(dirname "$0")/.." && pwd)
cd "$root_dir"

echo "Building release binaries..."
cargo build --release >/dev/null

# Extract version from Cargo.toml
version=$(grep -m1 '^version\s*=\s*"' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
case "$os" in
  darwin) os="macos" ;;
  linux) os="linux" ;;
  msys*|cygwin*|mingw*) os="windows" ;;
esac

dist_dir="dist"
mkdir -p "$dist_dir"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

bin_dir="target/release"
need=(blit blitd blitty)
have=()
for b in "${need[@]}"; do
  if [[ -x "$bin_dir/$b" ]]; then
    have+=("$b")
    cp "$bin_dir/$b" "$tmpdir/"
  fi
done

cp -f README.md "$tmpdir/" 2>/dev/null || true
cp -f LICENSE "$tmpdir/" 2>/dev/null || true

archive_name="blit-v${version}-${os}-${arch}.tar.gz"
tar -C "$tmpdir" -czf "$dist_dir/$archive_name" .
echo "Created: $dist_dir/$archive_name (bins: ${have[*]:-none})"
