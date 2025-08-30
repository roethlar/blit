//! Shared CLI helpers and small reusable Clap fragments

use clap::Parser;
use std::path::PathBuf;

/// Common daemon options used by blitd and (historically) the monolithic binary
#[derive(Clone, Debug, Parser)]
pub struct DaemonOpts {
    /// Bind address (host:port)
    #[arg(long, default_value = "0.0.0.0:9031")]
    // Default: listen on all interfaces; TLS/TOFU enforces safety
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

    /// Disable mDNS/DNS-SD advertisement of the service
    #[arg(long = "no-mdns", default_value_t = true)]
    pub no_mdns: bool,

    /// Friendly mDNS instance name (defaults to hostname)
    #[arg(long = "mdns-name")]
    pub mdns_name: Option<String>,
}

/// Optional remote URL argument for the TUI shell
#[derive(Clone, Debug, Parser)]
pub struct TuiOpts {
    /// Optional initial remote (blit://host:port[/path])
    #[arg(long)]
    pub remote: Option<String>,

    /// UNSAFE: Disable TLS and all security (trusted LAN only). Not shown in UI.
    #[arg(
        long = "never-tell-me-the-odds",
        help = "DISABLE ALL SECURITY - unencrypted, unsafe mode for trusted LAN benchmarks only"
    )]
    pub never_tell_me_the_odds: bool,
}
