use anyhow::{Context, Result};
use clap::Parser;

use blit::cli::DaemonOpts;
use blit::tls;

fn main() -> Result<()> {
    let opts = DaemonOpts::parse();

    // Validate root directory exists and is a directory
    if !opts.root.exists() {
        anyhow::bail!(
            "Error: Root directory does not exist: {}",
            opts.root.display()
        );
    }
    if !opts.root.is_dir() {
        anyhow::bail!(
            "Error: Root path is not a directory: {}",
            opts.root.display()
        );
    }

    // Canonicalize the path for better logging
    let canonical_root = std::fs::canonicalize(&opts.root)
        .with_context(|| format!("Failed to canonicalize root path: {}", opts.root.display()))?;

    println!("Starting Blit daemon:");
    println!("  Root: {}", canonical_root.display());
    println!("  Bind: {}", opts.bind);

    if opts.never_tell_me_the_odds {
        println!("  Security: 🚨 DISABLED (DANGEROUS MODE)");
        // spacing
        eprintln!("🚨 DANGER: --never-tell-me-the-odds DISABLES ALL SECURITY!");
        eprintln!("   • No encryption (all data transmitted in plain text)");
        eprintln!("   • No authentication (anyone can connect)");
        eprintln!("   • No verification (corrupted data may not be detected)");
        eprintln!("   • Only use on completely trusted networks for benchmarks");
        // spacing
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
    }

    // Optional mDNS advertisement (service discovery)
    if !opts.no_mdns {
        if let Err(e) = advertise_mdns(&opts) {
            eprintln!("mDNS advertise error: {}", e);
        }
    } else {
        println!("  mDNS: disabled (enable with '--no-mdns=false' or set '--mdns-name')");
    }

    // Run the async server directly - no more shelling out
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to build tokio runtime")?;

    if opts.never_tell_me_the_odds {
        // DANGEROUS: Completely unencrypted mode for benchmarks only
        eprintln!("🚨 Starting UNENCRYPTED server - no security features enabled");
        use blit::net_async::server::serve;
        rt.block_on(serve(&opts.bind, &canonical_root))
    } else {
        // SECURE BY DEFAULT: Always use TLS
        println!("Setting up TLS configuration...");

        if let Some(ref cert_path) = opts.tls_cert {
            println!("Using custom certificate: {}", cert_path.display());
        } else {
            let config_dir = tls::config_dir();
            println!(
                "Using self-signed certificate at: {}/server-cert.pem",
                config_dir.display()
            );
        }

        let tls_config = tls::load_or_generate_server_config(opts.tls_cert, opts.tls_key)
            .context("Failed to set up TLS configuration")?;

        rt.block_on(blit::net_async::server::serve_with_tls(
            &opts.bind,
            &canonical_root,
            tls_config,
        ))
    }
}

fn advertise_mdns(opts: &DaemonOpts) -> Result<()> {
    use mdns_sd::{ServiceDaemon, ServiceInfo};
    // Parse port from bind
    let port: u16 = opts
        .bind
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9031);
    // Instance name and hostname
    let instance = opts.mdns_name.clone().unwrap_or_else(|| {
        hostname::get()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|_| "blitd".into())
    });
    let host_name = format!("{}.local.", instance.replace(' ', "-"));

    // Determine IP address to advertise: prefer bound IP; if 0.0.0.0, pick primary local IPv4
    fn pick_local_ipv4() -> Option<String> {
        use std::net::{SocketAddr, UdpSocket};
        let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
        let _ = sock.connect("8.8.8.8:80").ok()?;
        let addr: SocketAddr = sock.local_addr().ok()?;
        Some(addr.ip().to_string())
    }
    let addr_s = match opts.bind.rsplit_once(':').map(|(h, _)| h) {
        Some("0.0.0.0") | None => pick_local_ipv4().unwrap_or_else(|| "127.0.0.1".to_string()),
        Some("::") => "::1".to_string(),
        Some(other) => other.to_string(),
    };

    // TXT records (advertise whether TLS is enabled)
    let mut props = std::collections::HashMap::new();
    props.insert("ver".to_string(), env!("CARGO_PKG_VERSION").to_string());
    props.insert(
        "tls".to_string(),
        if opts.never_tell_me_the_odds { "0".to_string() } else { "1".to_string() },
    );

    let mdns = ServiceDaemon::new()?;
    let service_type = "_blit._tcp.local.";
    let info = ServiceInfo::new(
        service_type,
        &instance,
        &host_name,
        addr_s,
        port,
        props,
    )?;
    mdns.register(info)?;
    // Leak the daemon so it stays alive
    Box::leak(Box::new(mdns));
    println!(
        "  mDNS: advertising {} as '{}' on port {}",
        service_type, instance, port
    );
    Ok(())
}
