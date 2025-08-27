//! Shared CLI helpers and small reusable Clap fragments

use clap::{ArgAction, Parser};
use std::path::PathBuf;

/// Common daemon options used by blitd and (historically) the monolithic binary
#[derive(Clone, Debug, Parser)]
pub struct DaemonOpts {
    /// Bind address (host:port)
    #[arg(long, default_value = "0.0.0.0:9031")]
    pub bind: String,

    /// Root directory to serve
    #[arg(long, default_value = ".")]
    pub root: PathBuf,
}

/// Optional remote URL argument for the TUI shell
#[derive(Clone, Debug, Parser)]
pub struct TuiOpts {
    /// Optional initial remote (blit://host:port[/path])
    #[arg(long)]
    pub remote: Option<String>,
}
