use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OptionsState {
    pub verbose: bool,
    pub progress: bool,
    pub threads: usize,      // 0 = auto
    pub net_workers: usize,  // default 4
    pub net_chunk_mb: usize, // default 4

    pub include_empty: bool, // --empty-dirs vs --no-empty-dirs
    pub update: bool,        // --update
    pub dry_run: bool,       // list-only preview

    pub exclude_files: Vec<String>, // --xf
    pub exclude_dirs: Vec<String>,  // --xd

    pub checksum: bool,   // --checksum
    pub force_tar: bool,  // --force_tar
    pub no_tar: bool,     // --no_tar
    pub no_verify: bool,  // --no-verify
    pub no_restart: bool, // --no-restart

    pub log_file: Option<PathBuf>,

    // Links/symlinks (platform-aware usage in UI)
    pub sl: bool,
    #[cfg(windows)]
    pub sj: bool,
    pub xj: bool,
    pub xjd: bool,
    pub xjf: bool,

    // Presets
    pub ludicrous_speed: bool,        // exposed
    pub never_tell_me_the_odds: bool, // hidden, advanced only
    // Preferred transfer mode (copy|mirror|move). Stored for TUI convenience.
    pub mode: String,
    pub recent_hosts: Vec<RecentHost>,
}

impl OptionsState {
    pub fn with_safe_defaults() -> Self {
        let mut s = Self::default();
        s.include_empty = true;
        // Use auto-tuning by default (0 means auto for performance knobs)
        s.threads = 0;
        s.net_workers = 0;
        s.net_chunk_mb = 0;
        s.mode = "copy".to_string();
        s.recent_hosts = Vec::new();
        s
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentHost {
    pub host: String,
    pub port: u16,
}

/// Build argv for invoking the `blit` binary based on OptionsState.
/// Returns a Vec of arguments (first element is the subcommand: copy|mirror|move).
pub fn build_blit_args(
    mode: super::app::Mode,
    opts: &OptionsState,
    src: &super::ui::PathSpec,
    dest: &super::ui::PathSpec,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();

    // Subcommand
    let sub = match mode {
        super::app::Mode::Mirror => "mirror",
        super::app::Mode::Copy => "copy",
        super::app::Mode::Move => "move",
    };
    args.push(sub.to_string());

    // Safety/verbosity
    if opts.verbose {
        args.push("-v".into());
    }
    // Imply -p unless in unsafe/ludicrous (to keep overhead low)
    let imply_progress = !opts.ludicrous_speed && !opts.never_tell_me_the_odds;
    if opts.progress || imply_progress {
        args.push("-p".into());
    }

    // Performance
    if opts.threads > 0 {
        args.push("-t".into());
        args.push(opts.threads.to_string());
    }
    if opts.net_workers > 0 {
        args.push("--net-workers".into());
        args.push(opts.net_workers.to_string());
    }
    if opts.net_chunk_mb > 0 {
        args.push("--net-chunk-mb".into());
        args.push(opts.net_chunk_mb.to_string());
    }

    // Directories behavior
    if opts.include_empty {
        args.push("--empty-dirs".into());
    } else {
        args.push("--no-empty-dirs".into());
    }
    if opts.update {
        args.push("--update".into());
    }

    // Dry run (preview)
    if opts.dry_run {
        args.push("--list-only".into());
    }

    // Filters
    for xf in &opts.exclude_files {
        args.push("--xf".into());
        args.push(xf.clone());
    }
    for xd in &opts.exclude_dirs {
        args.push("--xd".into());
        args.push(xd.clone());
    }

    // Integrity and transfer tuning
    if opts.checksum {
        args.push("--checksum".into());
    }
    if opts.force_tar {
        args.push("--force-tar".into());
    }
    if opts.no_tar {
        args.push("--no-tar".into());
    }
    if opts.no_verify {
        args.push("--no-verify".into());
    }
    if opts.no_restart {
        args.push("--no-restart".into());
    }

    // Logging
    if let Some(p) = &opts.log_file {
        args.push("--log-file".into());
        args.push(p.display().to_string());
    }

    // Links
    if opts.sl {
        args.push("--sl".into());
    }
    #[cfg(windows)]
    if opts.sj {
        args.push("--sj".into());
    }
    if opts.xj {
        args.push("--xj".into());
    }
    if opts.xjd {
        args.push("--xjd".into());
    }
    if opts.xjf {
        args.push("--xjf".into());
    }

    // Presets
    if opts.ludicrous_speed {
        args.push("--ludicrous-speed".into());
    }
    if opts.never_tell_me_the_odds {
        args.push("--never-tell-me-the-odds".into());
    }

    // Positional arguments
    let src_s = super::ui::pathspec_to_string(src);
    let dest_s = super::ui::pathspec_to_string(dest);
    args.push(src_s);
    args.push(dest_s);

    args
}

pub const OPTIONS_COUNT: usize = 8; // keep in sync with UI list

pub fn toggle_option(opts: &mut OptionsState, idx: usize) {
    match idx {
        0 => opts.verbose = !opts.verbose,
        1 => opts.progress = !opts.progress,
        2 => opts.include_empty = !opts.include_empty,
        3 => opts.update = !opts.update,
        4 => opts.checksum = !opts.checksum,
        5 => opts.no_verify = !opts.no_verify,
        6 => opts.no_restart = !opts.no_restart,
        7 => opts.ludicrous_speed = !opts.ludicrous_speed,
        // Links / symlink handling
        200 => {
            opts.sl = !opts.sl;
        }
        #[cfg(windows)]
        201 => {
            opts.sj = !opts.sj;
        }
        202 => {
            opts.xj = !opts.xj;
        }
        203 => {
            opts.xjd = !opts.xjd;
        }
        204 => {
            opts.xjf = !opts.xjf;
        }
        _ => {}
    }
}

fn config_path() -> PathBuf {
    blit::tls::config_dir().join("blitty.toml")
}

pub fn load_options() -> Result<OptionsState> {
    let p = config_path();
    if let Ok(data) = std::fs::read_to_string(&p) {
        let s: OptionsState = toml::from_str(&data)?;
        Ok(s)
    } else {
        Ok(OptionsState::with_safe_defaults())
    }
}

pub fn save_options(opts: &OptionsState) -> Result<()> {
    let dir = config_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir).ok();
    let p = config_path();
    let data = toml::to_string(opts)?;
    // atomic write
    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    use std::io::Write as _;
    tmp.write_all(data.as_bytes())?;
    tmp.flush()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o600));
    }
    tmp.persist(&p)?;
    Ok(())
}

pub fn add_recent_host(opts: &mut OptionsState, host: &str, port: u16) {
    let entry = RecentHost {
        host: host.to_string(),
        port,
    };
    // Remove if exists
    opts.recent_hosts
        .retain(|h| !(h.host == entry.host && h.port == entry.port));
    // Push front
    opts.recent_hosts.insert(0, entry);
    // Cap list
    if opts.recent_hosts.len() > 10 {
        opts.recent_hosts.truncate(10);
    }
}

pub fn adjust_option(opts: &mut OptionsState, idx: usize, delta: i32) {
    match idx {
        // Performance indices handled by UI mapping
        100 => {
            // threads
            let v = (opts.threads as i32 + delta).clamp(0, 512);
            opts.threads = v as usize;
        }
        101 => {
            // net_workers
            let v = (opts.net_workers as i32 + delta).clamp(1, 64);
            opts.net_workers = v as usize;
        }
        102 => {
            // net_chunk_mb
            let v = (opts.net_chunk_mb as i32 + delta).clamp(1, 32);
            opts.net_chunk_mb = v as usize;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lp(p: &str) -> super::super::ui::PathSpec {
        super::super::ui::PathSpec::Local(PathBuf::from(p))
    }

    #[test]
    fn args_copy_defaults() {
        let opts = OptionsState::with_safe_defaults();
        let args = build_blit_args(
            super::super::app::Mode::Copy,
            &opts,
            &lp("/src"),
            &lp("/dst"),
        );
        assert_eq!(args[0], "copy");
        assert!(args.contains(&"--empty-dirs".to_string()));
        assert!(args.ends_with(&vec!["/src".to_string(), "/dst".to_string()]));
    }

    #[test]
    fn args_mirror_filters_ludicrous() {
        let mut opts = OptionsState::with_safe_defaults();
        opts.exclude_files = vec!["*.tmp".into(), "*.bak".into()];
        opts.exclude_dirs = vec!["node_modules".into()];
        opts.ludicrous_speed = true;
        let args = build_blit_args(super::super::app::Mode::Mirror, &opts, &lp("/a"), &lp("/b"));
        assert_eq!(args[0], "mirror");
        assert!(args.windows(2).any(|w| w == ["--xf", "*.tmp"]));
        assert!(args.iter().any(|a| a == "--ludicrous-speed"));
    }

    #[test]
    fn args_move_advanced() {
        let mut opts = OptionsState::with_safe_defaults();
        opts.verbose = true;
        opts.progress = true;
        opts.threads = 8;
        opts.net_workers = 12;
        opts.net_chunk_mb = 16;
        opts.no_verify = true;
        opts.no_restart = false;
        let args = build_blit_args(super::super::app::Mode::Move, &opts, &lp("/x"), &lp("/y"));
        assert_eq!(args[0], "move");
        assert!(args.contains(&"-v".to_string()));
        assert!(args.windows(2).any(|w| w == ["-t", "8"]));
        assert!(args.windows(2).any(|w| w == ["--net-workers", "12"]));
        assert!(args.windows(2).any(|w| w == ["--net-chunk-mb", "16"]));
        assert!(args.iter().any(|a| a == "--no-verify"));
    }
}
