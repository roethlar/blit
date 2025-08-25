# RoboSync 3.1.0

Fast local + daemon sync with rsync-style delta (push/pull) and a robocopy-style CLI. Linux and macOS supported; Windows builds are experimental (see Platform Support).

3.1 highlights:
- Async pull small-file TAR bundling to reduce frame overhead and boost throughput.
- Accurate “sent files” and byte counters, including tar-bundled files.
- General polish and clippy cleanups.

RoboSync v3 adds a compact daemon protocol with a manifest handshake so only changed files transfer, server-side mirror deletions, symlink preservation, empty-directory mirroring, and both push and pull modes.

## Key Features

- High throughput: tar streaming for small files, parallel streams for medium, chunked I/O for large.
- Robocopy-style flags: `--mir`, `-l`, `--xf/--xd`, `-s/--subdirs`, `-e/--empty-dirs`.
- Daemon push and pull with rsync-style delta (size+mtime) and mirror deletions.
- Symlink preservation (tar mode and per-file path), timestamps preserved.
- Empty directories mirrored (and implied by `--mir`).

Experimental (opt-in):
- Async I/O server prototype behind `--serve-async` with streaming tar unpack and improved pull performance.

## Quick Start

Build:

```bash
cargo build --release
# Binary: target/release/robosync
```

Local copy:

```bash
robosync /src /dst --mir -v
```

Daemon (push and pull):

```bash
# On server (classic)
robosync --serve --bind 0.0.0.0:9031 --root /srv/root

# On server (experimental async)
robosync --serve-async --bind 0.0.0.0:9031 --root /srv/root

# Push from client to server
robosync /data robosync://server:9031/backup --mir -v

# Pull from server to client
robosync robosync://server:9031/data /backup --mir -v
```

Notes:
- Client and server must both be v3.x.
- `--no-tar` disables tar streaming in daemon push (tar preserves symlinks by default).
- Pull mirrors empty dirs via MkDir frames; push mirrors via manifest.
- Async mode is experimental and currently optimized for pull; keep client and server on the same minor version.

## CLI (common flags)

```text
robosync [OPTIONS] <SOURCE> <DESTINATION>

Options:
  -v, --verbose              Verbose output
      --progress             Show per-file operations
      --mir, --mirror        Mirror mode (copy + delete extras)
      --delete               Delete extras (same as --mir)
  -e, --empty-dirs           Include empty directories (/E)
  -s, --subdirs              Copy subdirectories but skip empty dirs (/S)
      --no-empty-dirs        Alias for skipping empty directories
  -l, --dry-run              List only (no changes)
      --xf <PATTERN>         Exclude files matching pattern(s)
      --xd <PATTERN>         Exclude directories matching pattern(s)
  -c, --checksum             Use checksums instead of size+mtime
      --force-tar            Force tar streaming for small files
      --no-tar               Disable tar streaming (daemon push)
      --serve                Run as daemon (server)
      --bind <ADDR>          Bind address (default 0.0.0.0:9031)
      --root <DIR>           Root directory for --serve

Semantics:
- `--mir` implies including empty directories (robocopy /E).
- Pull uses the same delta protocol in reverse; only needed files transfer.
```

## Platform Support

- Linux/macOS: supported and tested. Daemon push/pull, mirror deletions, symlinks, and small‑file TAR bundling are enabled.
- Windows: experimental. CI publishes MSVC builds and helper scripts are provided. Core copy and daemon modes may work, but full parity (e.g., symlink behavior, attribute mirroring, case‑insensitive paths, service setup) is still in progress.

Notes for Windows:
- Symlink creation may require Developer Mode or elevated privileges; otherwise symlinks fall back or may fail.
- Read‑only attribute is preserved; broader NTFS ACL/attributes are not yet mirrored.
- Case‑insensitive path logic and full mirror semantics are being finalized.
- Use the MSVC artifact or build with `scripts/build-windows.sh --msvc`.

## Build, Test, Lint

Standard cargo:

```bash
cargo build          # debug
cargo build --release
cargo test
cargo clippy
```

OS-specific build scripts (avoid target collisions):

```bash
# macOS (outputs under target/macos)
scripts/build-macos.sh [--release] [--test] [--clippy]

# Linux (outputs under target/linux)
scripts/build-linux.sh [--release] [--test] [--clippy] [--target <triple>]

# MUSL static (outputs under target/musl)
scripts/build-musl.sh [--release] [--test] [--clippy] [--target <musl-triple>]

# Windows (outputs under target/windows)
scripts/build-windows.sh [--release] [--test] [--clippy] [--target <triple>|--msvc]
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

On Linux distributions that use systemd, you can run the RoboSync daemon as a service:

1) Create a dedicated user and directories (recommended):

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin robosync || true
sudo mkdir -p /srv/robosync_root
sudo chown -R robosync:robosync /srv/robosync_root
sudo install -m 0755 target/release/robosync /usr/local/bin/robosync
```

2) Create the unit file at `/etc/systemd/system/robosync.service`:

```ini
[Unit]
Description=RoboSync Daemon (file sync server)
After=network-online.target
Wants=network-online.target

[Service]
User=robosync
Group=robosync
ExecStart=/usr/local/bin/robosync --serve --bind 0.0.0.0:9031 --root /srv/robosync_root
Restart=on-failure
RestartSec=2s
AmbientCapabilities=CAP_NET_BIND_SERVICE
NoNewPrivileges=true
ProtectSystem=full
ProtectHome=true
PrivateTmp=true
WorkingDirectory=/srv/robosync_root

[Install]
WantedBy=multi-user.target
```

3) Enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now robosync.service
sudo systemctl status robosync.service --no-pager
```

Firewall note: open TCP port 9031 (or the port you configure in `--bind`).

## Changelog

See CHANGELOG.md.


### Build Targets & CPU ISA Portability

If you see this when moving a binary between machines:

  robosync: CPU ISA level is lower than required

it means the binary was built with newer CPU features (e.g., x86-64-v3: AVX2/BMI2) than the target machine supports. This is not a bug in RoboSync; it’s how native-optimized builds behave.

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

GitHub Actions builds artifacts for Linux (GNU), Linux (MUSL static), macOS, and Windows (MSVC).

- Workflow: .github/workflows/ci.yml
- Artifacts (per job):
  - Linux (GNU): target/linux/release/robosync
  - Linux (MUSL x86_64): target/musl/x86_64-unknown-linux-musl/release/robosync
  - macOS: target/macos/release/robosync
  - Windows (MSVC): target/windows/x86_64-pc-windows-msvc/release/robosync.exe
