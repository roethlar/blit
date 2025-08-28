//! Shared CLI helpers and small reusable Clap fragments

use clap::{ArgAction, Parser};
use std::path::PathBuf;

/// Common daemon options used by blitd and (historically) the monolithic binary
#[derive(Clone, Debug, Parser)]
pub struct DaemonOpts {
    /// Bind address (host:port)
    #[arg(long, default_value = "127.0.0.1:9031")]  // SECURITY: Bind to localhost by default
    pub bind: String,

    /// Root directory to serve
    #[arg(long, default_value = ".")]
    pub root: PathBuf,

    /// Path to TLS certificate file (PEM format, auto-generated if not specified)
    #[arg(long)]
    pub tls_cert: Option<PathBuf>,

    /// Path to TLS private key file (PEM format, auto-generated if not specified)
    #[arg(long)]
    pub tls_key: Option<PathBuf>,

    /// UNSAFE: Disable TLS and all security features for maximum speed (trusted LAN only)
    #[arg(
        long = "never-tell-me-the-odds", 
        help = "DISABLE ALL SECURITY - unencrypted, unsafe mode for trusted LAN benchmarks only"
    )]
    pub never_tell_me_the_odds: bool,
}

/// Optional remote URL argument for the TUI shell
#[derive(Clone, Debug, Parser)]
pub struct TuiOpts {
    /// Optional initial remote (blit://host:port[/path])
    #[arg(long)]
    pub remote: Option<String>,
}
