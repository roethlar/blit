//! Blit Library
//!
//! High-performance file synchronization library with delta algorithm support

pub mod buffer;
pub mod checksum;
pub mod copy;
pub mod fs_enum;
pub mod logger;
pub mod tar_stream;

pub mod cli;
pub mod net_async; // For blitd daemon server
pub mod protocol;
pub mod protocol_core;
pub mod tls;
pub mod url; // TLS encryption and TOFU verification

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
#[cfg(windows)]
pub mod win_fs;
