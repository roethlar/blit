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
