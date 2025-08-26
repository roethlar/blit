# RoboSync Roadmap (Customer-Facing)

This roadmap focuses on high-impact performance and stability items customers care about. Developer task details live in `TODO.md`.

## Vision

Deliver a fast, reliable sync tool that:
- Matches or exceeds robocopy on Windows and rsync on Linux/macOS.
- Provides efficient small-file handling, robust mirror semantics, and predictable performance over 1/10GbE.
- Offers a simple CLI and an optional daemon for push/pull.

## 3.1: Performance & Stability Parity

Goal: Core feature parity across Linux/macOS and Windows, with production-ready performance.

- Core transfers
  - Push/pull over compact daemon protocol with delta (size+mtime).
  - Mirror delete on server; safe and predictable.
  - Small-file TAR streaming path to reduce per-file overhead.

- Windows parity
  - Symlink preserve (with privilege detection) in per-file and TAR paths.
  - Timestamp (mtime) preservation for all transfer modes.
  - Read-only attribute propagation; case-insensitive relpath handling.
  - Copy acceleration using native APIs (CopyFileExW/TransmitFile where applicable).

- Reliability & CI
  - Windows CI: build + smoke tests (push/pull) on localhost datasets.
  - Consistent counters and status reporting, including tar-bundled files.

Outcome: “Supported on Windows” across core scenarios once CI passes.

## 3.1.x: Quality-of-Life & Packaging

- Packaging
  - Windows zip artifact, basic firewall notes (open TCP 9031).
  - Quick-start snippets for push/pull and mirror mode.

- Documentation
  - Windows setup (symlink privileges, Developer Mode notes).
  - Service how-to (SC.exe) and systemd examples.

- Stability polish
  - Targeted retries/backoff on transient IO/network errors.
  - Minor counters/UX refinements.

## 3.2: Throughput & Transport Options

- Transport
  - Optional QUIC via `quinn` for high-RTT scenarios.
  - Stream multiplexing improvements for large parallelism on a single connection.

- Performance tuning
  - Socket tuning presets; smarter buffer sizing under contention.
  - Optional TLS (rustls) with minimal performance impact.

## Nice-to-Have (Future)

- Windows-specific
  - NTFS attribute/ACL mirroring (documented as out-of-scope for 3.1 parity).
  - Overlapped I/O (IOCP) exploration for further NTFS throughput.

- Advanced reliability/observability
  - More verbose diagnostics for delta mismatches.
  - Optional checksum-verify mode for daemon transfers.

## Future Plans

- TUI “Quick Share” (ephemeral inbound server)
  - Purpose: ad‑hoc LAN sharing directly from the TUI.
  - Default bind: primary RFC1918 LAN IP + random high port; visible banner and one‑key stop.
  - Guardrails: single active connection, tight timeouts, bounded directory listings/payloads, short prompt (y/N) warning.
  - No tokens/approvals by default to keep UX fast; warning before enabling.
  - Not a replacement for a long‑running service; session‑scoped only.

- Dedicated daemon binary (robosyncd)
  - Headless, long‑lived service for systemd/launchd/Windows Service.
  - Clear config for bind address/port, auth/tokens, TLS/QUIC, logging/metrics.
  - Recommended for always‑on servers and scheduled backups; distinct from TUI Quick Share.

---

Status tracking for engineers lives in `TODO.md` (dev-facing worklist). Customers can follow this document for feature-level progress and expectations.
