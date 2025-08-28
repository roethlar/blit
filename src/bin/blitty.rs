use clap::Parser;
use std::path::PathBuf;

use blit::cli::TuiOpts;

#[path = "blitty/app.rs"] mod app;
#[path = "blitty/ui.rs"] mod ui;
#[path = "blitty/theme.rs"] mod theme;
#[path = "blitty/remote.rs"] mod remote;

fn resolve_blit_path() -> std::path::PathBuf {
    // Allow override via env
    if let Ok(p) = std::env::var("BLIT_CLI") {
        let pb = std::path::PathBuf::from(p);
        if pb.exists() { return pb; }
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("blitty"));
    let dir = exe.parent().unwrap_or(std::path::Path::new(".")).to_path_buf();
    #[cfg(windows)]
    let candidates = [dir.join("blit.exe")];
    #[cfg(not(windows))]
    let candidates = [dir.join("blit")];
    for c in candidates {
        if c.exists() { return c; }
    }
    // Last resort, rely on PATH
    std::path::PathBuf::from("blit")
}


fn main() -> anyhow::Result<()> {
    let opts = TuiOpts::parse();
    let remote = match opts.remote {
        Some(s) => blit::url::parse_remote_url(&PathBuf::from(s)),
        None => None,
    };
    app::run(remote)
}
