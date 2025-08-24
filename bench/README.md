# RoboSync Benchmarks

This folder contains simple, reproducible shell scripts to benchmark RoboSync 3.0.0 against rsync for local copies and daemon push/pull over the network.

Results are written as CSV files to `bench/results/*.csv` for easy diffing and spreadsheet import.

Requirements
- Linux or macOS for local tests; Linux recommended for daemon tests.
- `bash`, `awk`, `stat`, `du`, `rsync`, and a built RoboSync binary.
- For network tests: a RoboSync daemon running on a remote host, or run it locally on a different path.

Quick Start
1) Build RoboSync in release mode:
   - `cargo build --release`
2) Create benchmark datasets (in `/tmp/robosync_bench/src_*`):
   - `bash bench/setup_datasets.sh`
3) Run local benchmark vs rsync:
   - `bash bench/run_local.sh`
4) Run daemon push benchmark (client → server):
   - Start daemon on server: `robosync --serve --bind 0.0.0.0:9031 --root /srv/robosync_root`
   - Edit `bench/run_push.sh` REMOTE_HOST/REMOTE_ROOT or export env vars
   - `bash bench/run_push.sh`
5) Run daemon pull benchmark (server → client):
   - Daemon as above; configure REMOTE_HOST/REMOTE_ROOT
   - `bash bench/run_pull.sh`

Outputs
- CSV files under `bench/results/` with columns:
  - `timestamp,test,mode,dataset,files,bytes,tool,seconds,mbps,notes`

Configuration
- All scripts support environment variables to customize paths:
  - `ROBOSYNC` – path to RoboSync binary (default: `target/release/robosync`)
  - `BENCH_ROOT` – base folder for datasets and temps (default: `/tmp/robosync_bench`)
  - `REMOTE_HOST` – hostname/IP for daemon tests (default: `localhost`)
  - `REMOTE_PORT` – daemon port (default: `9031`)
  - `REMOTE_ROOT` – daemon root path (default: `/srv/robosync_root`)
  - `RSYNC_REMOTE` – rsync remote (e.g., `user@host`) for rsync-over-ssh comparisons (optional)
  - `ITER` – iterations per dataset (default: 1)

Notes
- Datasets are moderate by default to keep runtimes sensible. Adjust `SCALE_*` variables in `setup_datasets.sh` as needed.
- These scripts use mirror semantics for fair comparisons: RoboSync `--mir` and rsync `-a --delete`.
- For network rsync comparisons you’ll need SSH access (`rsync -e ssh`). RoboSync daemon is TCP on the configured port.

