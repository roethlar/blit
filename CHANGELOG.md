# Changelog

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

