#[cfg(any(feature = "api_client", feature = "server"))]
pub mod cli;
// Blit Library
//
// High-performance file synchronization library with delta algorithm support

// Expose only client API when feature is enabled. By default, lib is minimal
// so the main binary can compile without pulling unused code.
#[cfg(feature = "api_client")]
pub mod net_async; // client+server; bins gate server-only usage
#[cfg(feature = "api_client")]
pub mod protocol;
#[cfg(feature = "api_client")]
pub mod protocol_core;
#[cfg(feature = "api_client")]
pub mod tls;
#[cfg(feature = "api_client")]
pub mod url; // TLS helpers and URL parsing for client
#[cfg(feature = "api_client")]
pub mod buffer;
#[cfg(feature = "api_client")]
pub mod fs_enum;
#[cfg(feature = "api_client")]
pub mod copy;
#[cfg(feature = "api_client")]
pub mod logger;
#[cfg(feature = "api_client")]
pub mod tar_stream;

/// Library argument surface for network client helpers.
/// This decouples library code from the binary's Clap struct.
#[derive(Clone, Debug, Default)]
pub struct Args {
    pub mirror: bool,
    pub delete: bool,
    pub empty_dirs: bool,
    pub ludicrous_speed: bool,
    pub progress: bool,
    pub verbose: bool,
    pub exclude_files: Vec<String>,
    pub exclude_dirs: Vec<String>,
    pub net_workers: usize,
    pub net_chunk_mb: usize,
    pub checksum: bool,
    pub force_tar: bool,
    pub no_tar: bool,
    pub never_tell_me_the_odds: bool,
}
// (win_fs and other internals are not exported by lib)
