# RoboSync 3.0.0

Fast local + daemon sync with rsync-style delta (push/pull) and a robocopy-style CLI. Linux/macOS only.

RoboSync v3 adds a compact daemon protocol with a manifest handshake so only changed files transfer, server-side mirror deletions, symlink preservation, empty-directory mirroring, and both push and pull modes.

## Key Features

- High throughput: tar streaming for small files, parallel streams for medium, chunked I/O for large.
- Robocopy-style flags: `--mir`, `-l`, `--xf/--xd`, `-s/--subdirs`, `-e/--empty-dirs`.
- Daemon push and pull with rsync-style delta (size+mtime) and mirror deletions.
- Symlink preservation (tar mode and per-file path), timestamps preserved.
- Empty directories mirrored (and implied by `--mir`).

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
# On server
robosync --serve --bind 0.0.0.0:9031 --root /srv/root

# Push from client to server
robosync /data robosync://server:9031/backup --mir -v

# Pull from server to client
robosync robosync://server:9031/data /backup --mir -v
```

Notes:
- Client and server must both be v3.x.
- `--no-tar` disables tar streaming in daemon push (tar preserves symlinks by default).
- Pull mirrors empty dirs via MkDir frames; push mirrors via manifest.

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

Linux and macOS. Tested on Ubuntu and TrueNAS SCALE (daemon).

## Build, Test, Lint

```bash
cargo build          # debug
cargo build --release
cargo test
cargo clippy
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
