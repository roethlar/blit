//! Blit Library
//!
//! High-performance file synchronization library with delta algorithm support

pub mod buffer;
pub mod checksum;
pub mod copy;
pub mod fs_enum;
pub mod logger;
pub mod tar_stream;

pub mod protocol;
pub mod protocol_core;
pub mod url;
pub mod cli;

/// Minimal argument surface used by library client helpers.
/// This decouples library code from the binary's Clap struct.
#[derive(Clone, Debug, Default)]
pub struct Args {
    pub mirror: bool,
    pub delete: bool,
    pub empty_dirs: bool,
    pub ludicrous_speed: bool,
    pub progress: bool,
}
#[cfg(windows)]
pub mod win_fs;

#[cfg(feature = "tui")]
pub mod tui;
