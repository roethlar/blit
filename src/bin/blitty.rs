use clap::Parser;
use std::path::PathBuf;

use blit::cli::TuiOpts;

#[cfg(feature = "tui")]
fn main() -> anyhow::Result<()> {
    let opts = TuiOpts::parse();
    let remote = match opts.remote {
        Some(s) => blit::url::parse_remote_url(&PathBuf::from(s)),
        None => None,
    };
    blit::tui::start_shell(remote)
}

#[cfg(not(feature = "tui"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("TUI is not enabled; rebuild with --features tui")
}
