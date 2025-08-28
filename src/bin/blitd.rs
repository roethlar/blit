use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

use blit::cli::DaemonOpts;
use blit::tls;

fn main() -> Result<()> {
    let opts = DaemonOpts::parse();
    
    // Validate root directory exists and is a directory
    if !opts.root.exists() {
        anyhow::bail!("Error: Root directory does not exist: {}", opts.root.display());
    }
    if !opts.root.is_dir() {
        anyhow::bail!("Error: Root path is not a directory: {}", opts.root.display());
    }
    
    // Canonicalize the path for better logging
    let canonical_root = std::fs::canonicalize(&opts.root)
        .with_context(|| format!("Failed to canonicalize root path: {}", opts.root.display()))?;
    
    println!("Starting Blit daemon:");
    println!("  Root: {}", canonical_root.display());
    println!("  Bind: {}", opts.bind);
    
    if opts.never_tell_me_the_odds {
        println!("  Security: 🚨 DISABLED (DANGEROUS MODE)");
        eprintln!("");
        eprintln!("🚨 DANGER: --never-tell-me-the-odds DISABLES ALL SECURITY!");
        eprintln!("   • No encryption (all data transmitted in plain text)");
        eprintln!("   • No authentication (anyone can connect)");
        eprintln!("   • No verification (corrupted data may not be detected)");
        eprintln!("   • Only use on completely trusted networks for benchmarks");
        eprintln!("");
    } else {
        println!("  Security: 🔒 TLS enabled (secure by default)");
    }
    
    // Security warning for 0.0.0.0 binding
    if opts.bind.starts_with("0.0.0.0") {
        eprintln!("⚠️  WARNING: Binding to 0.0.0.0 exposes daemon to all network interfaces");
        eprintln!("   Consider binding to specific interface (e.g., 192.168.1.100:9031)");
        if opts.never_tell_me_the_odds {
            eprintln!("   This protocol is UNENCRYPTED and UNAUTHENTICATED - HIGH RISK!");
        }
        eprintln!("   Only use on trusted networks (LAN)");
        eprintln!("");
    }
    
    // Run the async server directly - no more shelling out
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to build tokio runtime")?;
    
    if opts.never_tell_me_the_odds {
        // DANGEROUS: Completely unencrypted mode for benchmarks only
        eprintln!("🚨 Starting UNENCRYPTED server - no security features enabled");
        rt.block_on(blit::net_async::server::serve(&opts.bind, &canonical_root))
    } else {
        // SECURE BY DEFAULT: Always use TLS
        println!("Setting up TLS configuration...");
        
        if let Some(ref cert_path) = opts.tls_cert {
            println!("Using custom certificate: {}", cert_path.display());
        } else {
            let config_dir = tls::config_dir();
            println!("Using self-signed certificate at: {}/server-cert.pem", config_dir.display());
        }
        
        let tls_config = tls::load_or_generate_server_config(opts.tls_cert, opts.tls_key)
            .context("Failed to set up TLS configuration")?;
        
        rt.block_on(blit::net_async::server::serve_with_tls(&opts.bind, &canonical_root, tls_config))
    }
}