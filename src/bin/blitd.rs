use clap::Parser;
use std::path::PathBuf;

use blit::cli::DaemonOpts;

fn main() -> anyhow::Result<()> {
    let opts = DaemonOpts::parse();
    let root: PathBuf = opts.root.clone();
    println!("Blit async daemon\n  Bind: {}\n  Root: {}", opts.bind, root.display());
    let exe_path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("blit"));
    let exe_dir = exe_path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    #[cfg(windows)]
    let candidates = [exe_dir.join("blit.exe")];
    #[cfg(not(windows))]
    let candidates = [exe_dir.join("blit")];
    let driver = candidates.iter().find(|p| p.exists()).cloned().unwrap_or_else(|| PathBuf::from("blit"));
    let port = opts.bind.split(':').last().unwrap_or("9031");
    let status = std::process::Command::new(driver)
        .arg("daemon")
        .arg("--root")
        .arg(&root)
        .arg("--port")
        .arg(port)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("blit daemon exited with status {:?}", status.code())
    }
}
