# Repository Guidelines

## Project Structure & Module Organization
- `src/main.rs`: Binary entry; CLI and orchestration.
- `src/lib.rs`: Library exports for internal modules.
- Key modules: `algorithm`, `checksum`, `buffer`, `copy`, `tar_stream`, `streaming_delta`, `concurrent_delta`, `windows_enum`, `progress`, `log`.
- Tests: unit tests co-located in modules (e.g., `src/algorithm.rs`, `src/checksum.rs`).

## Build, Test, and Development Commands
- Build (debug): `cargo build`
- Build (release): `cargo build --release`
- Run locally: `cargo run -- /source /dest --mir -v`
- Tests: `cargo test`
- Lint: `cargo clippy` (use `cargo clippy -- -D warnings` before PRs)
- Format: `cargo fmt`

## Coding Style & Naming Conventions
- Rust 2021 edition; 4-space indent; UTF-8 files.
- Use `snake_case` for functions/modules, `CamelCase` for types, `SCREAMING_SNAKE_CASE` for consts.
- Prefer `anyhow::Result<T>` and `.context(...)` for error paths.
- Keep deps lean; favor existing crates already in `Cargo.toml` (clap, rayon, tar, sha2, blake3, etc.).
- Run `cargo fmt && cargo clippy` before pushing.

## Testing Guidelines
- Unit tests live next to code under `#[cfg(test)] mod tests { ... }`.
- Name tests for behavior, not implementation (e.g., `copies_when_size_differs`).
- Run all tests with `cargo test`; add quick benchmarks or assertions for performance-sensitive paths when feasible.
- If adding integration tests, place them in `tests/` and call the binary via `assert_cmd` or exercise library APIs.

## Commit & Pull Request Guidelines
- Commits: concise, imperative subject; keep related changes together.
  - Examples: `Fix mirror deletions on empty dirs`, `v2.1.12: optimize small-file tar path`.
- PRs must include: clear description, rationale, notable perf or memory impact, platforms tested (Linux/macOS/Windows), and any CLI/help updates.
- Link issues and include minimal repros or sample commands when fixing bugs.

## Architecture & Platform Notes
- Strategy by file size: small→tar streaming, medium→parallel copy, large→chunked I/O.
- Cross-platform: Unix uses `libc`; Windows uses Win32 (`winapi`, `windows-sys`). Guard OS-specific code with `cfg(...)`.
- Prefer non-allocating, streaming approaches in hot paths; measure before changing algorithms.

