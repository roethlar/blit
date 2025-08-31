# Blit — Fast, Secure File Sync (CLI + Daemon + TUI)

# DO NOT USE THIS APP IN PRODUCTION!!! 
There are fundamental bugs that need to be fixed before this will be stable enough for a prod release.
-

Fast local and remote sync with an async-first daemon, rsync-style delta, and robocopy-style ergonomics. Linux and macOS supported; Windows builds are experimental (see Platform Support).

Highlights:
- 
- Direction‑agnostic verbs: `mirror`, `copy`, `move` with URL inference (`blit://host:port/path`)
- Async small-file TAR bundling to reduce frame overhead and boost throughput.
- Accurate file/byte counters including tar-bundled files.
- TUI shell (feature-gated) with Dracula theme preview.

Blit uses a compact daemon protocol with a manifest handshake so only changed files transfer, server-side mirror deletions, symlink preservation, empty-directory mirroring, and both push and pull modes.

## Key Features

- High throughput: tar streaming for small files, parallel streams for medium, chunked I/O for large.
- Daemon push and pull with rsync-style delta (size+mtime) and mirror deletions.

## Quick Start

Build all binaries:

```bash
cargo build --release
# Binaries: target/release/blit, target/release/blitd, target/release/blitty
```

Local copy (direction-agnostic verbs):

```bash
# Mirror: copy + delete extras (includes empty dirs)
blit mirror /src /dst -v

# Copy: copy only, never delete
blit copy /src /dst -v

# Move: mirror, then remove source after confirmation
blit move /src /dst -v
```

Daemon and remote paths:

```bash
# Start daemon (TLS secure by default; generates self‑signed on first run)
target/release/blitd --root /srv/root --bind 0.0.0.0:9031

# Mirror local → remote (TLS)
target/release/blit /data blit://server:9031/backup --mir -v

# Mirror remote → local (TLS)
target/release/blit blit://server:9031/data /backup --mir -v

# Plaintext for trusted LAN benchmarking ONLY
# 1) Start daemon with security disabled
target/release/blitd --root /srv/root --bind 0.0.0.0:9031 --never-tell-me-the-odds
# 2) Use blit:// and optionally --never-tell-me-the-odds on the client
target/release/blit /data blit://server:9031/backup --mir --never-tell-me-the-odds
```

Verify examples:

```bash
# Verify two local trees (size+mtime)
blit verify /src /dst --limit 20

# Verify with checksums (slower, stronger)
blit verify /src /dst --checksum --json > verify.json

# Verify local vs remote and write CSV
blit verify /src blit://server:9031/dst --csv verify.csv --limit 50
```

Common recipes:

```bash
# Mirror local data to a server path
blit mirror /data blit://server:9031/backup -v

# High-throughput LAN push
blit mirror /big blit://server:9031/big --net-workers 8 --net-chunk-mb 8 --ludicrous-speed -v

# Pull and verify quickly (size+mtime)
blit mirror blit://server:9031/dataset /local/dataset -v && \
  blit verify blit://server:9031/dataset /local/dataset --limit 20
```

Notes:
- Client and server must both be v3.x.
- `--no-tar` disables tar streaming in daemon push (tar preserves symlinks by default).
- Pull mirrors empty dirs via MkDir frames; push mirrors via manifest.

## CLI

Subcommands:

```text
blit mirror <SRC> <DEST>
blit copy   <SRC> <DEST>
blit move   <SRC> <DEST>
blit verify <SRC> <DEST> [--checksum] [--json] [--csv <file>] [--limit N]
blitty --remote blit://host:9031/     # optional TUI client
```

Direction inference:
- If either side uses `blit://` or `blit://`, that side is remote.
- Remote→remote is not supported in this release.

Common options:
- `-v, --verbose`: verbose output
- `--progress`: show per-file operations
- `--xf/--xd`: exclude files/dirs by pattern (repeatable)
- `-e/--empty-dirs`: include empty directories
- `-s/--subdirs` or `--no-empty-dirs`: skip empty directories
- `-l/--dry-run`: list only (no changes)
- `-c/--checksum`: compare by checksum instead of size+mtime (verify)
- `--force-tar` / `--no-tar`: control small-file TAR streaming (push)
- `--ludicrous-speed`: favor throughput (bigger buffers, fewer guards)
- `--never-tell-me-the-odds`: DISABLE ALL SECURITY - unencrypted, unsafe mode (trusted LAN benchmarks only)

Daemon options (secure by default):
- `--bind` and `--root`: server binding and directory (default bind: `0.0.0.0:9031`, current dir). TLS with TOFU is enabled by default.
- `--tls-cert` / `--tls-key`: custom TLS certificate (auto-generates self-signed if not provided)
- `--never-tell-me-the-odds`: explicitly disable all security for benchmarks (NOT recommended)

Performance tuning:
- `--net-workers <N>`: number of parallel large-file workers for async push (default: 4; 1–32).
- `--net-chunk-mb <MB>`: network I/O chunk size for large files (default: 4; 1–32 MB).
- `--ludicrous-speed`: also enables low-latency socket mode (TCP_NODELAY) and larger defaults.

## TUI (blitty)

- - Dual‑pane UI (local/local by default). Toggle right pane to remote and connect to `blit://host:9031`.
- Navigation with arrows/Enter; select paths and run transfers (mirror/copy/move). Press `x` to cancel.
- The unsafe `--never-tell-me-the-odds` is CLI‑only — not exposed in the UI.

## Best Practices

- Mirror vs copy: use `mirror` for one-way backups (adds/deletes to match src), `copy` when you never want deletions.
- Direction inference: any `blit://host:port/path` side is remote. Remote→remote is not supported.
- Excludes: use repeated `--xf/--xd` patterns for large trees to avoid unnecessary scans.
- Verify: after first big sync, prefer size+mtime verify; use `--checksum` for spot checks or sensitive data.
- Performance on LAN:
  - Start with defaults; for larger datasets increase parallelism: `--net-workers 6..8` and `--net-chunk-mb 8`.
  - Use `--ludicrous-speed` on trusted networks to reduce latency and boost chunk sizes.
  - Keep source/target on fast local disks; avoid network filesystems on both ends simultaneously.
- Windows:
  - Run daemon as a service and allow port 9031 in the firewall.
  - Symlinks need Developer Mode or elevation; mirror deletions clear read-only automatically.
- TUI:
  - Use R to browse a remote, set src/dest with s/d, and g to run; x cancels.
  - Prefer CLI for scripted or long-running jobs; TUI is a simple interactive front-end.

## Platform Support

- Linux/macOS: supported and tested. Daemon push/pull, mirror deletions, symlinks, and small‑file TAR bundling are enabled.
- Windows: experimental. CI publishes MSVC builds and helper scripts are provided. Core copy and daemon modes may work, but full parity (e.g., symlink behavior, attribute mirroring, case‑insensitive paths, service setup) is still in progress.

Notes for Windows:
- Symlink creation may require Developer Mode or elevated privileges; otherwise symlinks fall back or may fail.
- Read‑only attribute is preserved; broader NTFS ACL/attributes are not yet mirrored.
 - Mirror deletions will attempt to clear the read‑only attribute before removing files/dirs.
 - Case‑insensitive path logic for mirror semantics is under active polish.
- Use the MSVC artifact or build with `scripts/build-windows.sh --msvc`.

## Build, Test, Lint

Standard cargo:

```bash
cargo build          # debug
cargo build --release
cargo test
cargo clippy
```

OS-specific build scripts (default to release; isolate by target):

Artifacts layout with scripts: `target/<triple>/{release|debug}/blit[.exe]`

```bash
# macOS (Bash)
scripts/build-macos.sh [--debug] [--target <triple>]

# Linux (Bash)
scripts/build-linux.sh [--debug] [--target <triple>]

# MUSL static (Bash)
scripts/build-musl.sh --target x86_64-unknown-linux-musl [--debug]

# Windows (Bash)
scripts/build-windows.sh [--debug] [--target <triple>|--msvc]

# Windows (PowerShell)
./scripts/build-windows.ps1 [-Debug] [-Target <triple>] [-MSVC]
```

Makefile shortcuts:

```bash
make macos          # macOS debug
make macos-release  # macOS release
make linux          # Linux debug
make linux-release  # Linux release
make musl           # MUSL debug (x86_64 musl)
make musl-release   # MUSL release (x86_64 musl)
make windows-gnu    # Windows (MinGW) build
make windows-msvc   # Windows (MSVC) build
```

## Systemd Service (Daemon)

On Linux distributions that use systemd, you can run the Blit daemon as a service:

1) Create a dedicated user and directories (recommended):

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin blit || true
sudo mkdir -p /srv/blit_root
sudo chown -R blit:blit /srv/blit_root
sudo install -m 0755 target/release/blitd /usr/local/bin/blitd
```

2) Create the unit file at `/etc/systemd/system/blit.service`:

```ini
[Unit]
Description=Blit Daemon (file sync server)
After=network-online.target
Wants=network-online.target

[Service]
User=blit
Group=blit
ExecStart=/usr/local/bin/blitd --root /srv/blit_root --bind 0.0.0.0:9031
Restart=on-failure
RestartSec=2s
AmbientCapabilities=CAP_NET_BIND_SERVICE
NoNewPrivileges=true
ProtectSystem=full
ProtectHome=true
PrivateTmp=true
WorkingDirectory=/srv/blit_root

[Install]
WantedBy=multi-user.target
```

3) Enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now blit.service
sudo systemctl status blit.service --no-pager
```

Firewall note: open TCP port 9031 on the server.

Windows firewall note: if running the legacy server on Windows, allow inbound TCP 9031 (or your chosen port). Symlink creation may require Developer Mode or elevated privileges.

## Changelog

See CHANGELOG.md.

## Roadmap

See ROADMAP.md for upcoming high-impact features and milestones.


### Build Targets & CPU ISA Portability

If you see this when moving a binary between machines:

  blit: CPU ISA level is lower than required

it means the binary was built with newer CPU features (e.g., x86-64-v3: AVX2/BMI2) than the target machine supports. This is not a bug in Blit; it's how native-optimized builds behave.

Portable build options (choose one):

- Build on the target machine (recommended):
  cargo build --release

- Force a portable glibc baseline on the build host:
  RUSTFLAGS="-C target-cpu=x86-64-v2" cargo build --release
  # If the target CPU is very old, use v1:
  # RUSTFLAGS="-C target-cpu=x86-64" cargo build --release

- Fully static MUSL build (widely portable across distros):
  cargo build --release --target x86_64-unknown-linux-musl

Tips:
- Ensure your environment isn’t forcing native features (unset RUSTFLAGS, remove any .cargo/config that sets target-cpu=native).
- On the target, check /proc/cpuinfo for avx2/bmi2 support if you’re unsure.
- For maximum performance on a single host, build there (native). For portability, use v2 or MUSL.

## CI

GitHub Actions builds, lints, and runs smokes on every push/PR.
- Workflow: `.github/workflows/ci.yml`
- Steps: cargo fmt (check), clippy (deny warnings), build, plaintext smoke, TLS smoke

## Smoke Tests

- Plaintext (unsafe): `scripts/smoke-linux.sh`
- TLS: `scripts/smoke-tls.sh`

Both scripts build release binaries, start daemons, run push and pull, and verify trees.

GitHub Actions builds artifacts for Linux (GNU), Linux (MUSL static), macOS, and Windows (MSVC).

- Workflow: .github/workflows/ci.yml
- Artifacts (per job):
  - Linux (GNU): target/linux/release/blit
  - Linux (MUSL x86_64): target/musl/x86_64-unknown-linux-musl/release/blit
  - macOS: target/macos/release/blit
  - Windows (MSVC): target/windows/x86_64-pc-windows-msvc/release/blit.exe
