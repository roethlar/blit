# Dev Log – RoboSync 3.0.0

Date: 2025-08-23

Summary
- Cut 3.0.0 with a functional daemon protocol supporting push and pull with rsync‑style delta.
- Implemented server‑side mirror delete, robust path normalization, symlink + mtime preservation, and empty‑dir mirroring.
- Added simple benchmark scripts and a one‑off rsyncd runner for TrueNAS.

Work Items
- Protocol
  - Frames added: ManifestStart/Entry/End, NeedList, Symlink, MkDir (pull), alongside existing TAR_* and PFILE_*.
  - Delta logic: client sends manifest (path,size,mtime); server returns NeedList; client/server transfer only changed/missing items.
  - Pull fix: empty destination now triggers full transfer (send if needed OR not present in client manifest).
- Mirror
  - Push: uses manifest expected set for deletions; avoids deleting valid files when nothing was sent.
  - Pull: client mirrors extras using expected set; MkDir frames ensure empty directories persist.
- Integrity
  - Per‑file mtime preserved via `filetime` crate (applied on FileEnd/PFileEnd and on client after pull writes).
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
- Bench against rsync on real datasets; collect CSV and summarize.
- After evaluation, begin Windows P0 (win_fs layer, symlink/timestamp, case‑insensitive paths).

