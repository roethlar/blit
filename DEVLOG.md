# Dev Log – RoboSync 3.0.0

Date: 2025-08-25

Summary (async net progress)
- Added experimental Tokio async transport behind `--serve-async` (`src/net_async.rs`).
- Push → async server (receive): implemented Manifest/NeedList, TAR small-file path, parallel file paths, FileRaw (large), VerifyReq/Hash, MkDir, Symlink, POSIX mode via SetAttr, mirror delete.
- Pull ← async server (send): implemented Manifest/NeedList, MkDir, Symlink, FileStart/Data/End, POSIX mode via SetAttr, Done/OK.
- Streaming tar unpack (no tempfile) on async receive path using bounded channel + blocking tar reader.
- Sparse-preserving writes (skip large zero runs) for FileRaw and PFileData paths to reduce allocated blocks on XFS.
- POSIX mode preservation on Unix for both push and pull; Windows readonly preserved via flags.
- Verified content parity via content-only hashes and mode parity on mixed datasets; observed 2.38× speedup for async pull on 10GbE.

Work Items
- Protocol
  - Frames added: ManifestStart/Entry/End, NeedList, Symlink, MkDir (pull), alongside existing TAR_* and PFILE_*.
  - Delta logic: client sends manifest (path,size,mtime); server returns NeedList; client/server transfer only changed/missing items.
  - Pull fix: empty destination now triggers full transfer (send if needed OR not present in client manifest).
  - Async: VerifyReq (31)/VerifyHash (32), FileRawStart (29) supported; NeedRanges handshake stubbed (no ranges requested yet).
- Mirror
  - Push: uses manifest expected set for deletions; avoids deleting valid files when nothing was sent.
  - Pull: client mirrors extras using expected set; MkDir frames ensure empty directories persist.
- Integrity
  - Per‑file mtime preserved via `filetime` crate (applied on FileEnd/PFileEnd and on client after pull writes); POSIX modes applied from SetAttr on Unix.
  - Symlinks: preserved via tar and per‑file Symlink frames (daemon push `--no-tar`, and pull).
  - Path hardening under `--root`; rejects parent components.
- UX
  - Cleaned spinner line; summary shows total files/MB including tar bundles.
  - Minimal progress added for pull.
- CLI & Semantics
  - `--mir` implies including empty dirs (robocopy /E).
  - Added `--no-empty-dirs`; `--no-tar` applies to daemon push.
  - Updated help/about, README, CHANGELOG.
- Bench harness
  - One‑off rsyncd (no /etc changes): `bench/rsyncd_min.sh`.
  - Real‑data scripts split by tool: `bench/run_real_data_robosync.sh`, `bench/run_real_data_rsync.sh`.
  - Manual cache flush helpers: `bench/flush_local.sh`, `bench/flush_truenas_zfs.sh`.

Notes
- Client and server must both be v3.x due to protocol changes.
- TrueNAS rsync testing uses ephemeral config; no system files modified.

Planned Next
- Async pull small-file TAR bundling to reduce frame overhead (symmetry with push).
- Fix push "sent files" counter to count each file once; include tar-bundled counts precisely.
- Optional: async delta ranged writes (NeedRanges + DeltaData apply-in-place) for resumable large files.
- Bench against rsync on real datasets; collect CSV and summarize.
- After evaluation, begin Windows P0 (win_fs layer, symlink/timestamp, case‑insensitive paths).

## 2025-08-28 — Blitty OptionsState (PR1 kickoff)

- Project goals aligned: FAST, SIMPLE, RELIABLE; secure by default (TLS/TOFU), “Ludicrous speed” exposed; `--never-tell-me-the-odds` hidden but supported with warnings.
- TODO updated: Added phased blitty UI plan (Options → Preview → Confirmations → Remaining tabs/Connect).
- Implementation start (PR1):
  - Add `src/bin/blitty/options.rs` with `OptionsState` (safe defaults) and `build_blit_args()` that maps OptionsState + Mode + PathSpec → argv for `blit`.
  - Wire `OptionsState` into `AppState` (no UI usage yet).
  - Unit tests to snapshot some arg combinations (copy/mirror/move; excludes; ludicrous preset).

### Daemon default bind update
- Changed `blitd` default bind from `127.0.0.1:9031` to `0.0.0.0:9031` to meet SIMPLE and functional defaults: the daemon is a network service.
- Safety preserved by default via TLS + TOFU; existing startup warning for 0.0.0.0 remains.

## Build (macOS) — 2025-08-25
- Fixed duplicate `sendfile_to_stream` definitions by scoping the Unix fallback away from macOS.
- Adjusted `sendfile_to_stream` to take `&mut TcpStream`; updated the large-file send call site.
- Debug and release builds succeed on macOS.
- Incremental compilation lock error on networked volume: build with `CARGO_INCREMENTAL=0`, or set `CARGO_TARGET_DIR` to a local path (e.g., `/tmp/robosync-target`).
- To avoid target collisions with Linux builds, use a macOS-specific target dir:
  - `CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=target/macos cargo build [--release]`
  - Artifacts: `target/macos/debug/robosync`, `target/macos/release/robosync`.
- Added helper script: `scripts/build-macos.sh` with options `--release`, `--test`, `--clippy`.
- Added helper scripts for other platforms:
   - Linux: `scripts/build-linux.sh [--release|--test|--clippy|--target <triple>]` → `target/linux/...`
   - MUSL: `scripts/build-musl.sh [--release|--test|--clippy|--target <musl-triple>]` → `target/musl/...`
   - Windows: `scripts/build-windows.sh [--release|--test|--clippy|--target <triple>|--msvc]` → `target/windows/...`
- Added Makefile shortcuts: `make macos|macos-release|linux|linux-release|musl|musl-release|windows-gnu|windows-msvc`.
 - Added GitHub Actions CI matrix for Linux (GNU/MUSL), macOS, and Windows (MSVC) with artifact upload.
## 2025-08-26 — Async Client Implementation Complete

Summary: Completed the full asynchronous client implementation (`net_async::client`), enabling high-performance push and pull operations.

- Implemented `net_async::client::push` for asynchronous file uploads:
  - Handles manifest exchange and server-side need list processing.
  - Uses TAR streaming for efficient transfer of small files.
  - Implements parallel raw file transfers for large files via multiple worker connections.
- Implemented `net_async::client::pull` for asynchronous file downloads:
  - Handles manifest exchange and server-side delta calculation.
  - Supports TAR unpacking for small files.
  - Manages individual file reception, symlink creation, and attribute setting.
  - Includes mirror deletion logic for destination cleanup.
- Refactored URL parsing logic into a shared `src/url.rs` module, improving code organization and reusability.
- Integrated the new async client into `src/main.rs`, making it the default for network operations.
- Added `net_async::client::complete_remote` as a client-side helper for remote tab completion.

## 2025-08-26 — Async-first sprint status (Bunny)

- Protocol/versioning:
  - Added protocol version handshake to classic and async paths; mixed client/server builds fail fast with a clear error.
- Async reliability:
  - Implemented size-aware read timeouts (header/payload) in async server.
  - Added timed write wrapper (write_frame_timed + write_frame) and applied across key async writes.
  - Added per-connection counters (files/bytes sent/received) and a summary log at Done with elapsed_ms.
- Speed profiles:
  - Client hints speed via Start flags; async server parses and raises TAR chunk and file send buffers when set.
  - Classic TransmitFile chunk raised to 16MB under --ludicrous-speed.
- CI + smokes:
  - macOS async: scripts/smoke-macos.sh and workflow (.github/workflows/macos-async.yml).
  - Linux async: scripts/smoke-linux.sh and workflow (.github/workflows/linux-async.yml).
  - Windows async: workflow added; smoke-windows.ps1 supports -Async and now asserts read-only + mtime on push/pull.
- In progress:
  - Windows async parity validation (CreateSymbolicLinkW path; mtime/readonly checks).
  - Ensure all async writes go through the timed wrapper.
  - Wire async CI to green across macOS/Linux/Windows; fix any flakes.
 - Implemented async pull small-file TAR bundling to reduce frame overhead.
 - Fixed push 'sent files' counter to count each file once, including tar-bundled counts.
## 2025-08-26 — Async Default, CLI Refresh, Async Delta

- Async daemon is now default; classic server available via `--serve-legacy`.
- Added protocol module (`src/protocol.rs`) with shared MAGIC/VERSION/frame IDs.
- Finished async large-file delta (DELTA_* + NeedRanges), with ranged writes and mtime set.
- TAR counters on async server now reflect logical bytes/files on unpack (not transport frame sizes).
- Robustness: size-aware timeouts on framed I/O and raw body chunks; safe path normalization; mirror delete via expected set.
- Windows: local mirror deletions compare relpaths case-insensitively.
- CLI: added subcommands `daemon <root> <port>`, `mirror <src> <dest>`, `copy <src> <dest>`, `move <src> <dest>`; direction inferred by presence of `robosync://`.
- Designed `verify <src> <dest> [--checksum] [--json] [--csv <file>]` (read-only) and remote delete frames for move: RemoveTreeReq/Resp (42/43).
- Planned interactive TUI: `robosync` (local/local) and `robosync shell robosync://server:port` (right pane remote) with Dracula theme; ListReq/Resp for remote with tight deadlines + caching.
- Deferred: TUI “Quick Share” (ephemeral inbound mode) to a future release; will use a simple y/N warning and bind to a LAN IP + random port.

## 2025-08-26 — Finalize async default, TUI execution, Windows mirror polish, tuning + cleanup

- TUI execution + progress
  - Wired `g` to execute mirror/copy/move by spawning the CLI; streams stdout/stderr into a small log pane; spinner on status line.
  - Added `x` to cancel a running transfer.
  - Added `h` overlay for quick key help.

- Windows reliability
  - Async server mirror delete uses case-insensitive path comparison on Windows to align with filesystem semantics.
  - Local mirror deletion clears read-only attribute and retries before erroring.

- Performance tuning (FAST)
  - New flags: `--net-workers` (1–32) and `--net-chunk-mb` (1–32 MB) to tune async push.
  - Auto‑tune: if not overridden, scale workers with CPU and workload; bump chunks under `--ludicrous-speed`.
  - Enabled TCP_NODELAY for client/accepted sockets.

- Simplicity + docs
  - README: performance tuning, best practices, common recipes; TUI keys updated (g/x).
  - Explicitly disallow remote→remote transfers with a clear error.

- Cleanup
  - Removed unused modules: `streaming_delta.rs`, `concurrent_delta.rs`, `algorithm.rs`, and `progress.rs`.
  - Kept classic server (`src/net.rs`) for `--serve-legacy`.

- Smoke scripts
  - `scripts/smoke-local.sh`: push/pull mirror, verify JSON, extra-file deletion check, best-effort symlink on Unix.
  - `scripts/smoke-perf.sh`: quick large-file throughput estimate with configurable size and tuning flags.

- Session context
  - Chat context captured in `agent_bus.ndjson` at repo root for auditability (saved prior to reboot).
