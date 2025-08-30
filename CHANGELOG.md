# Changelog

## 1.0.1 — 2025-08-29
- Network: Fixed TLS pull phase alignment (START→OK→MANIFEST→NEED_LIST/stream→DONE/OK) to remove early EOFs.
- Security: Consistent secure/plaintext selection across all client connect paths (push workers, pull, list, verify, remove_tree) using URL scheme and `--never-tell-me-the-odds`.
- Server: Implemented plaintext pull; TLS/PLAINTEXT handlers consolidated; graceful shutdown now emits TLS close_notify.
- Defaults: mDNS advertisement disabled by default (`--no-mdns` now defaults to true).
- DX: Added `scripts/smoke-tls.sh` and a CI workflow that runs fmt, clippy (deny warnings), build, plaintext + TLS smokes.
- Docs: README updated for separate `blitd` daemon, URL schemes (`blits://` for TLS), and new smokes/CI.

## 1.0.0 — 2025-08-27 (Blit)
- Project rename: RoboSync → Blit.
- New binary names: `blit` (CLI), `blitd` (daemon), `blitty` (TUI).
- New URL scheme: `blit://host:port/path` (replaces `robosync://`).
- TUI env var: `BLIT_ASCII` (replaces `ROBOSYNC_ASCII`).

Notes:
- This is the first release under the Blit name. Previous releases remain tagged as RoboSync (e.g., 3.1.x).
- Breaking change for scripts/integrations: update binary names and URLs.


## Rename Notice (2025-08-27)
The project has been renamed to Blit. Source/binaries now use: blit, blitd, blitty, and blit://. This changelog retains historical "RoboSync" entries; future entries will use the new names.


## v3.1.0 — 2025-08-25
- Async pull: implement small-file TAR bundling (TarStart/TarData/TarEnd) with SetAttr for POSIX modes on Unix.
- Push: fix sent-files accounting to include TAR-bundled files; summary now counts each file exactly once.
- Pull: fix receive counters to include files unpacked from TAR streams.
- Cleanup: address clippy issues in networking paths; minor I/O error mapping fixes.
- Docs: update TODO to reflect completed async pull TAR bundling.
 - macOS: async smoke script added (`scripts/smoke-macos.sh`) and CI workflow (`.github/workflows/macos-async.yml`).
 - macOS: `sendfile` honors preferred chunk size under `--ludicrous-speed`; APFS mtime/readonly preserved in async paths (tar mtimes + POSIX mode via SetAttr).
 - Async server: logs per-connection summary (files_sent, bytes_sent, elapsed_ms) for observability.

All notable changes to this project will be documented in this file.

## 3.0.0 – 2025-08-23

- Daemon rsync-style delta: client/server manifest handshake (path, size, mtime) transfers only changed files.
- Network push and pull: support remote destination and remote source URLs (`robosync://host:port/path`).
- Server-side mirror: deletes extras based on the expected set from the manifest.
- Symlink preservation: via tar (push) and per-file Symlink frames (`--no-tar`), also in pull.
- Timestamps preserved: per-file protocol carries mtime; server/client apply on completion.
- Empty directories: mirrored in push/pull; `--mir` implies including empty dirs (robocopy /E semantics).
- UX polish: clean status line, summary includes tar-bundled file/byte counts.
- Safety: path hardening under root; tar frame draining; improved error logging.

Notes:
- Client and server must both be on v3.x due to protocol changes.
- Windows is not supported.
