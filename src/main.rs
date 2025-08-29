//! Blit Rev3 - Simplified, high-performance file synchronization
//!
//! Design goals:
//! - Saturate 10GbE network (1+ GB/s throughput)
//! - Minimal startup overhead
//! - Direct dispatch based on file size
//! - No complex abstractions

mod buffer;
mod copy;
mod fs_enum;
mod logger;
mod net;
mod net_async;
mod protocol;
mod protocol_core;
mod tar_stream;
mod tls;
mod url;
#[cfg(windows)]
mod win_fs;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use parking_lot::Mutex;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::buffer::BufferSizer;
use crate::copy::{
    chunked_copy_file, file_needs_copy, mmap_copy_file, parallel_copy_files, windows_copyfile,
    CopyStats,
};
use crate::fs_enum::{
    categorize_files, enumerate_directory_filtered, CopyJob, FileEntry, FileFilter,
};
use crate::logger::{Logger, NoopLogger, TextLogger};
use crate::tar_stream::{tar_stream_transfer_list, TarConfig};
// TUI removed - use blitty binary instead
use serde::Serialize;

#[derive(Debug, Serialize)]
struct VerifySummary {
    identical: bool,
    changed_count: usize,
    extras_count: usize,
    sample: Vec<VerifyEntry>,
}

#[derive(Debug, Serialize)]
struct VerifyEntry {
    kind: &'static str,
    path: String,
    size_src: u64,
    size_dest: u64,
    mtime_src: i64,
    mtime_dest: i64,
}

/// Command-line arguments
#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Blit — Fast, async-first sync with mirror/copy/move and an optional daemon",
    long_about = "Blit — high-performance local file synchronization.\n\
Local mirror/copy/move operations with optimized algorithms for small and large files.\n\
For network operations, use blitd (daemon server) and blitty (TUI client)."
)]
struct Args {
    /// Source directory or file (for legacy CLI)
    source: Option<PathBuf>,

    /// Destination directory or file (for legacy CLI)
    destination: Option<PathBuf>,

    /// Number of threads (0 = auto)
    #[arg(short = 't', long, default_value_t = 0)]
    threads: usize,
    /// Network workers for async push (parallel large-file streams)
    #[arg(long = "net-workers", default_value_t = 4)]
    net_workers: usize,
    /// Network I/O chunk size in MB (1-32)
    #[arg(long = "net-chunk-mb", default_value_t = 4)]
    net_chunk_mb: usize,

    /// Show processing stages and operations (discovery, categorization, etc.)
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Show individual file operations as they happen
    #[arg(short = 'p', long = "progress", global = true)]
    progress: bool,

    /// Mirror mode - copy and delete extra files (same as --delete)
    #[arg(long = "mir", alias = "mirror")]
    mirror: bool,

    /// Delete extra files in destination
    #[arg(long, alias = "del", alias = "purge")]
    delete: bool,

    /// Update mode: copy only changed files (size+mtime), include empty dirs, do not delete extras
    #[arg(
        long = "update",
        help = "Copy only changed files; include empty dirs; no deletions"
    )]
    update: bool,

    /// Copy subdirectories, but not empty ones (/S)
    #[arg(short = 's', long)]
    subdirs: bool,

    /// Copy subdirectories including empty ones (/E) - default behavior
    #[arg(short = 'e', long)]
    empty_dirs: bool,

    /// Do not include empty directories (alias for /S)
    #[arg(long = "no-empty-dirs")]
    no_empty_dirs: bool,

    /// List only - don't copy files (dry run) (/L)
    #[arg(short = 'l', long, alias = "list-only")]
    dry_run: bool,

    /// Exclude files matching patterns (/XF)
    #[arg(long = "xf", action = clap::ArgAction::Append)]
    exclude_files: Vec<String>,

    /// Exclude directories matching patterns (/XD)
    #[arg(long = "xd", action = clap::ArgAction::Append)]
    exclude_dirs: Vec<String>,

    /// Use checksums for comparison instead of size+timestamp
    #[arg(short = 'c', long)]
    checksum: bool,

    /// Force tar streaming for small files
    #[arg(long)]
    force_tar: bool,

    /// Disable tar streaming
    #[arg(long)]
    no_tar: bool,

    /// Disable post-transfer verification (not recommended)
    #[arg(long = "no-verify")]
    no_verify: bool,

    /// Disable resumable transfers (delta/ranged writes)
    #[arg(long = "no-restart")]
    no_restart: bool,

    // Server arguments removed - use blitd binary instead
    /// Write JSONL log entries to file
    #[arg(long = "log-file")]
    log_file: Option<PathBuf>,

    /// Copy symbolic links as links (do not follow targets)
    #[arg(
        long = "sl",
        help = "Copy symbolic links as links (do not follow targets)"
    )]
    sl: bool,

    /// Copy junctions as junctions (do not follow targets) [Windows only]
    #[cfg(windows)]
    #[arg(
        long = "sj",
        help = "Copy junctions as junctions (do not follow targets) [Windows]"
    )]
    sj: bool,

    /// Exclude all symbolic links and junction points
    #[arg(long = "xj", help = "Exclude all symbolic links and junctions")]
    xj: bool,

    /// Exclude symlinks that point to directories (and junctions)
    #[arg(
        long = "xjd",
        help = "Exclude symlinks that point to directories (and junctions)"
    )]
    xjd: bool,

    /// Exclude symlinks that point to files
    #[arg(long = "xjf", help = "Exclude symlinks that point to files")]
    xjf: bool,

    /// Max throughput preset: increases buffers/workers and disables verify/resume
    #[arg(
        long = "ludicrous-speed",
        help = "Max throughput preset (bigger buffers/workers; no verify/resume)"
    )]
    ludicrous_speed: bool,

    /// Unsafe max speed: also skips metadata/guards. Only for trusted LAN benchmarks.
    #[arg(
        long = "never-tell-me-the-odds",
        help = "Unsafe max speed (skips extra guards/metadata)"
    )]
    never_tell_me_the_odds: bool,

    /// (internal) On-demand remote completion helper
    #[arg(long, hide = true)]
    complete_remote: Option<String>,
    /// New subcommands (preferred)
    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Subcommand, Debug)]
enum CliCommand {
    /// Mirror src to dest (copy + delete extras; include empty dirs)
    Mirror { src: PathBuf, dest: PathBuf },
    /// Copy src to dest (no deletions; include empty dirs)
    Copy { src: PathBuf, dest: PathBuf },
    /// Move src to dest (mirror, then remove src after confirmation)
    Move { src: PathBuf, dest: PathBuf },
    /// Verify two trees are identical (no changes applied)
    #[command(hide = true)]
    Verify {
        src: PathBuf,
        dest: PathBuf,
        #[arg(long)]
        checksum: bool, // compare by checksum instead of size+mtime
        #[arg(long)]
        json: bool, // print JSON summary
        #[arg(long)]
        csv: Option<PathBuf>, // write CSV to file
        #[arg(long)]
        limit: Option<usize>, // limit sample lines on stdout
    },
}

fn main() -> Result<()> {
    // Set up Ctrl-C handler
    if let Err(e) = ctrlc::set_handler(move || {
        eprintln!("\nInterrupted by user. Exiting (Ctrl-C)...");
        // Exit immediately with 130 (128 + SIGINT)
        std::process::exit(130);
    }) {
        eprintln!("Failed to set Ctrl-C handler: {}", e);
    }

    let args = Args::parse();

    // Remote completion mode
    if let Some(comp_str) = args.complete_remote {
        return client_complete_remote(&comp_str);
    }

    // Subcommand handling first
    if let Some(cmd) = &args.command {
        match cmd {
            CliCommand::Mirror { src, dest } => {
                return run_copy_like(src, dest, true, true, &args);
            }
            CliCommand::Copy { src, dest } => {
                return run_copy_like(src, dest, false, true, &args);
            }
            CliCommand::Move { src, dest } => {
                // Confirm destructive move
                eprint!("This will remove source after clone. Type 'yes' to confirm: ");
                use std::io::Write;
                std::io::stdout().flush().ok();
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).ok();
                if input.trim() != "yes" {
                    eprintln!("Aborted.");
                    return Ok(());
                }
                run_copy_like(src, dest, true, true, &args)?;
                // Remove source (local or remote)
                if let Some(remote_src) = url::parse_remote_url(src) {
                    // Remote delete via protocol
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .context("build tokio runtime for remove")?;
                    rt.block_on(net_async::client::remove_tree(
                        &remote_src.host,
                        remote_src.port,
                        &remote_src.path,
                    ))?;
                } else {
                    if src.is_file() {
                        let _ = std::fs::remove_file(src);
                    } else {
                        let _ = std::fs::remove_dir_all(src);
                    }
                }
                return Ok(());
            }
            CliCommand::Verify {
                src,
                dest,
                checksum,
                json,
                csv,
                limit,
            } => {
                let summary = verify_trees(src, dest, *checksum)?;
                // Output
                if let Some(csv_path) = csv {
                    let mut w = std::fs::File::create(csv_path).context("open csv")?;
                    use std::io::Write as _;
                    writeln!(w, "type,path,size_src,size_dest,mtime_src,mtime_dest").ok();
                    for e in summary.sample.iter() {
                        writeln!(
                            w,
                            "{},{},{},{},{},{}",
                            e.kind, e.path, e.size_src, e.size_dest, e.mtime_src, e.mtime_dest
                        )
                        .ok();
                    }
                }
                if *json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&summary).unwrap_or("{}".to_string())
                    );
                } else {
                    println!("Identical: {}", summary.identical);
                    println!("Changed/new: {}", summary.changed_count);
                    println!("Extras: {}", summary.extras_count);
                    if let Some(lim) = *limit {
                        for e in summary.sample.iter().take(lim) {
                            println!("  {} {}", e.kind, e.path);
                        }
                    }
                }
                std::process::exit(if summary.identical { 0 } else { 1 });
            } // Shell command removed - use blitty binary instead
        }
    }

    // On Windows, check for symlink creation privilege if --sl is used
    #[cfg(windows)]
    if args.sl {
        if !blit::win_fs::has_symlink_privilege() {
            eprintln!("ERROR: To create symbolic links on Windows, this program must be run as an administrator.");
            eprintln!(
                "Please re-run from an elevated command prompt (e.g., 'Run as administrator')."
            );
            std::process::exit(1);
        }
    }

    // Server mode removed - use blitd binary instead
    if std::env::args().any(|a| a == "--serve" || a == "--serve-legacy") {
        anyhow::bail!("Server mode removed. Use 'blitd' binary for daemon mode.");
    }
    // Choose logger once; zero overhead in hot paths with NoopLogger
    let logger: Arc<dyn Logger + Send + Sync> = if let Some(ref p) = args.log_file {
        match TextLogger::new(p) {
            Ok(l) => Arc::new(l),
            Err(_) => Arc::new(NoopLogger),
        }
    } else {
        // In ludicrous modes, suppress logging overhead by default
        if args.ludicrous_speed || args.never_tell_me_the_odds {
            Arc::new(NoopLogger)
        } else {
            Arc::new(NoopLogger)
        }
    };

    let start = Instant::now();

    // Handle delete/mirror flags (robocopy compatibility)
    let delete_extra = args.delete || args.mirror;

    // Interactive mode: if no paths or subcommand, launch TUI when available
    // No implicit TUI: if no paths provided, fall back to stdin prompts (CLI stays headless)
    let (src_path, dest_path) = match (args.source.clone(), args.destination.clone()) {
        (Some(s), Some(d)) => (s, d),
        _ => {
            eprintln!("Interactive mode: enter source and destination paths.");
            use std::io::Write;
            eprint!("Source: ");
            std::io::stdout().flush().ok();
            let mut s = String::new();
            std::io::stdin().read_line(&mut s).ok();
            eprint!("Destination: ");
            std::io::stdout().flush().ok();
            let mut d = String::new();
            std::io::stdin().read_line(&mut d).ok();
            let s = s.trim();
            let d = d.trim();
            if s.is_empty() || d.is_empty() {
                anyhow::bail!("source and destination required");
            }
            (PathBuf::from(s), PathBuf::from(d))
        }
    };

    // Network operations: support push (remote destination) and pull (remote source)
    if let Some(remote) = url::parse_remote_url(&dest_path) {
        return client_push(remote, &src_path, &args);
    }
    if let Some(remote_src) = url::parse_remote_url(&src_path) {
        return client_pull(remote_src, &dest_path, &args);
    }

    // Detect if this is a network transfer
    let is_network = is_network_path(&dest_path);

    // Simple activity indicator (no performance impact)
    let show_activity = !(args.verbose || args.progress); // Only show simple indicator if not verbose or progress

    // Simple activity indicator with spinner
    let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let mut spinner_index = 0;

    if show_activity {
        print!(
            "{} Blit {}...",
            spinner_chars[spinner_index],
            env!("CARGO_PKG_VERSION")
        );
        std::io::Write::flush(&mut std::io::stdout()).ok();
        spinner_index = (spinner_index + 1) % spinner_chars.len();
    }

    // Dry run mode - just list what would be copied
    if args.dry_run {
        println!("DRY RUN MODE - No files will be copied");
    }

    if args.verbose {
        println!("Blit {} - Linux/macOS Optimized", env!("CARGO_PKG_VERSION"));
        println!("Source: {:?}", src_path);
        println!("Destination: {:?}", dest_path);
        println!("Local operation only");
        if delete_extra {
            println!(
                "Delete mode: enabled (mirror/purge)
"
            );
        }
    }

    // Configure Rayon thread pool for optimal performance
    // Use physical CPU count by default to avoid hyperthreading overhead
    let thread_count = if args.threads > 0 {
        args.threads
    } else {
        // Default to physical CPU count for better performance
        num_cpus::get_physical()
    };

    if let Err(e) = rayon::ThreadPoolBuilder::new()
        .num_threads(thread_count)
        .build_global()
    {
        eprintln!(
            "Rayon pool already initialized ({}); continuing with existing pool",
            e
        );
    }

    if args.verbose {
        eprintln!(
            "Configured thread pool with {} threads (physical CPUs: {})",
            thread_count,
            num_cpus::get_physical()
        );
    }

    // Check if source is a single file
    if src_path.is_file() {
        return copy_single_file(&src_path, &dest_path, false, args.progress);
    }

    // Enumerate files with progress
    if args.verbose {
        println!("Enumerating files...");
    }

    // Decide empty-dir behavior (Robocopy semantics: --mir implies including empties). --update matches this.
    let include_empty_dirs = if delete_extra || args.update {
        if args.subdirs || args.no_empty_dirs {
            if args.verbose {
                println!("Note: --mir implies --empty-dirs; including empty directories.");
            }
        }
        true
    } else if args.subdirs || args.no_empty_dirs {
        false
    } else if args.empty_dirs {
        true
    } else {
        true
    };

    // Build filter from CLI arguments
    let filter = FileFilter {
        exclude_files: args.exclude_files.clone(),
        exclude_dirs: args.exclude_dirs.clone(),
        min_size: None,
        max_size: None,
        include_empty_dirs,
    };

    if args.verbose {
        if !args.exclude_dirs.is_empty() {
            println!("Excluding directories: {:?}", args.exclude_dirs);
        }
        if !args.exclude_files.is_empty() {
            println!("Excluding files: {:?}", args.exclude_files);
        }
    }

    // Determine link policy: default to dereference unless explicitly preserving
    #[cfg(windows)]
    let preserve_links = args.sl || args.sj;
    #[cfg(not(windows))]
    let preserve_links = args.sl;

    let initial_entries = if !preserve_links {
        crate::fs_enum::enumerate_directory_deref_filtered(&src_path, &filter)
    } else {
        enumerate_directory_filtered(&src_path, &filter)
    }
    .context("Failed to enumerate source directory")?;

    // Build copy jobs from enumerated entries
    let copy_jobs: Vec<CopyJob> = initial_entries
        .into_iter()
        .map(|entry| CopyJob {
            entry,
            start_offset: 0,
        })
        .collect();

    let total_files = copy_jobs.len();
    let total_size: u64 = copy_jobs.iter().map(|job| job.entry.size).sum();

    if show_activity {
        print!(
            "\r{} found {}, copying...",
            spinner_chars[spinner_index], total_files
        );
        std::io::Write::flush(&mut std::io::stdout()).ok();
        spinner_index = (spinner_index + 1) % spinner_chars.len();
    } else if args.verbose {
        println!(
            "Found {} files ({:.2} GB)",
            total_files,
            total_size as f64 / 1_073_741_824.0
        );
    }

    // Filter out files that don't need copying when mirroring or in --update mode
    let skip_unchanged = delete_extra || args.update;
    let copy_jobs = if skip_unchanged {
        if show_activity {
            print!("\r{} comparing...", spinner_chars[spinner_index]);
            std::io::Write::flush(&mut std::io::stdout()).ok();
            spinner_index = (spinner_index + 1) % spinner_chars.len();
        }

        use rayon::prelude::*;
        copy_jobs
            .into_par_iter()
            .filter(|job| {
                let src = &job.entry.path;
                let dst = compute_destination(src, &src_path, &dest_path);
                match file_needs_copy(src, &dst, args.checksum) {
                    Ok(needs_copy) => needs_copy,
                    Err(_) => true, // Copy if we can't determine (file might not exist)
                }
            })
            .collect()
    } else {
        copy_jobs
    };

    // Categorize files by size
    let (small, medium, large) = categorize_files(copy_jobs);

    // Handle dry run mode
    if args.dry_run {
        println!("\n=== DRY RUN - Files that would be copied ===");
        println!("Small files (<1MB): {}", small.len());
        println!("Medium files (1-100MB): {}", medium.len());
        println!("Large files (>100MB): {}", large.len());
        println!(
            "Total: {} files ({:.2} GB)",
            total_files,
            total_size as f64 / 1_073_741_824.0
        );

        if args.verbose {
            println!("\n--- Files to copy ---");
            for (i, entry) in small
                .iter()
                .chain(medium.iter())
                .chain(large.iter())
                .enumerate()
            {
                if i < 20 {
                    // Limit output
                    println!(
                        "  {} ({} bytes)",
                        entry.entry.path.display(),
                        entry.entry.size
                    );
                } else if i == 20 {
                    println!("  ... and {} more files", total_files - 20);
                    break;
                }
            }
        }

        // Handle mirror mode deletion in dry run
        if delete_extra {
            println!("\nWould also delete extra files in destination.");
        }

        return Ok(());
    }

    if args.verbose {
        println!("Small files (<1MB): {}", small.len());
        println!("Medium files (1-100MB): {}", medium.len());
        println!("Large files (>100MB): {}", large.len());
    }

    // Track overall progress
    let mut total_stats = CopyStats::default();
    let buffer_sizer = Arc::new(BufferSizer::new());

    // Optional heartbeat spinner to show activity (local mode)
    let mut hb_handle = None;
    let hb_running = Arc::new(std::sync::atomic::AtomicBool::new(false));
    if show_activity {
        hb_running.store(true, std::sync::atomic::Ordering::SeqCst);
        let running = hb_running.clone();
        hb_handle = Some(std::thread::spawn(move || {
            let spinner = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let mut idx = 0usize;
            while running.load(std::sync::atomic::Ordering::SeqCst) {
                print!("\r{} copying...", spinner[idx]);
                let _ = std::io::Write::flush(&mut std::io::stdout());
                idx = (idx + 1) % spinner.len();
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        }));
    }

    // Process all file categories concurrently using separate threads
    use std::sync::mpsc;
    use std::thread;

    let (tx, rx) = mpsc::channel::<(&str, CopyStats)>();
    let mut handles = Vec::new();

    // Thread 1: Process small files with tar streaming (if beneficial)
    if !small.is_empty() {
        let use_tar = !args.no_tar && (args.force_tar || should_use_tar(&small, false));
        let small_files = small.clone();
        let source = src_path.clone();
        let destination = dest_path.clone();
        let buffer_sizer_clone = buffer_sizer.clone();
        let tx_clone = tx.clone();
        let verbose = args.verbose;
        let _show_files = args.progress;
        let logger_clone = logger.clone();

        let handle = thread::spawn(move || {
            let mut stats = CopyStats::default();

            if use_tar {
                if verbose {
                    println!("Using tar streaming for {} small files", small_files.len());
                }

                match process_small_files_tar(
                    &small_files,
                    &source,
                    &destination,
                    false,
                    &*logger_clone,
                ) {
                    Ok((files, bytes)) => {
                        stats.files_copied = files;
                        stats.bytes_copied = bytes;
                    }
                    Err(e) => {
                        stats.add_error(format!("Tar streaming failed: {}", e));
                    }
                }
            } else {
                // Process small files individually
                let small_pairs = prepare_copy_pairs(&small_files, &source, &destination);
                stats = parallel_copy_files(
                    small_pairs,
                    buffer_sizer_clone,
                    false, // Local only
                    &*logger_clone,
                );
            }

            let _ = tx_clone.send(("small", stats));
        });
        handles.push(handle);
    }

    // Thread 2: Process medium files in parallel
    if !medium.is_empty() {
        let medium_files = medium;
        let source = src_path.clone();
        let destination = dest_path.clone();
        let buffer_sizer_clone = buffer_sizer.clone();
        let tx_clone = tx.clone();
        let verbose = args.verbose;
        let _show_files = args.progress;
        let logger_clone = logger.clone();

        let handle = thread::spawn(move || {
            if verbose {
                println!("Processing {} medium files in parallel", medium_files.len());
            }

            let medium_pairs = prepare_copy_pairs(&medium_files, &source, &destination);
            let stats = parallel_copy_files(
                medium_pairs,
                buffer_sizer_clone,
                false, /* local only */
                &*logger_clone,
            );

            let _ = tx_clone.send(("medium", stats));
        });
        handles.push(handle);
    }

    // Thread 3: Process large files with chunked copy
    if !large.is_empty() {
        let large_files = large;
        let source = src_path.clone();
        let destination = dest_path.clone();
        let buffer_sizer_clone = buffer_sizer.clone();
        let tx_clone = tx.clone();
        let verbose = args.verbose;
        let show_files = args.progress;
        let logger_clone = logger.clone();

        let handle = thread::spawn(move || {
            if verbose {
                println!("Processing {} large files", large_files.len());
            }

            let stats = Arc::new(Mutex::new(CopyStats::default()));

            large_files.par_iter().for_each(|entry| {
                let dst = compute_destination(&entry.entry.path, &source, &destination);
                let mut s = stats.lock();

                let copy_result = if cfg!(unix) {
                    // Always local now
                    mmap_copy_file(&entry.entry.path, &dst)
                } else {
                    chunked_copy_file(
                        &entry.entry.path,
                        &dst,
                        &buffer_sizer_clone,
                        false, // Local only
                        None,
                        &*logger_clone,
                    )
                };

                match copy_result {
                    Ok(bytes) => {
                        s.add_file(bytes);
                        if show_files {
                            println!(
                                "  Copied: {} → {} ({} bytes)",
                                entry.entry.path.display(),
                                dst.display(),
                                bytes
                            );
                        }
                    }
                    Err(e) => {
                        s.add_error(format!("Failed to copy {:?}: {}", entry.entry.path, e));
                    }
                }
            });

            let final_stats = Arc::try_unwrap(stats)
                .map(|m| m.into_inner())
                .unwrap_or_else(|_arc| {
                    // Log when Arc is still shared - may indicate thread synchronization issue
                    eprintln!("Warning: Arc<CopyStats> for large files still has references, using default");
                    CopyStats::default()
                });
            let _ = tx_clone.send(("large", final_stats));
        });
        handles.push(handle);
    }

    // Collect results from all threads
    drop(tx); // Close sender so receiver knows when all threads are done

    for handle in handles {
        let _ = handle.join();
    }

    // Collect all stats
    while let Ok((_category, stats)) = rx.recv() {
        merge_stats(&mut total_stats, stats);
    }

    // Handle mirror mode - delete extra files in destination
    if delete_extra {
        if args.verbose || args.progress {
            println!("Scanning destination for extra files...");
        }

        let deletion_stats =
            handle_mirror_deletion(&src_path, &dest_path, &filter, args.progress, args.dry_run)?;

        if args.verbose && (deletion_stats.0 > 0 || deletion_stats.1 > 0) {
            println!(
                "Deleted {} files and {} directories",
                deletion_stats.0, deletion_stats.1
            );
        }
    }

    // Finish heartbeat spinner
    if let Some(h) = hb_handle.take() {
        hb_running.store(false, std::sync::atomic::Ordering::SeqCst);
        let _ = h.join();
    }

    // Finish progress and print results
    // Simple completion indicator
    if show_activity {
        print!(
            "\r{} done!                    \n",
            spinner_chars[spinner_index]
        );
    }

    // Print summary (always show)
    let elapsed = start.elapsed();
    if !args.progress || args.verbose {
        println!();
        println!("=== Copy Complete ===");
        println!("Files copied: {}", total_stats.files_copied);
        println!(
            "Total size: {:.2} GB",
            total_stats.bytes_copied as f64 / 1_073_741_824.0
        );
        println!("Time: {:.2}s", elapsed.as_secs_f64());
        println!(
            "Throughput: {:.2} MB/s",
            (total_stats.bytes_copied as f64 / 1_048_576.0) / elapsed.as_secs_f64()
        );
    }

    if !total_stats.errors.is_empty() {
        println!("\nErrors encountered: {}", total_stats.errors.len());
        if args.verbose || args.progress {
            for error in &total_stats.errors {
                eprintln!("  - {}", error);
            }
        }
    }

    Ok(())
}

fn run_copy_like(
    src: &Path,
    dest: &Path,
    mirror: bool,
    include_empty: bool,
    base_args: &Args,
) -> Result<()> {
    // Build a minimal Args clone to re-use existing logic
    let mut args = base_args.clone_for_copylike();
    args.source = Some(src.to_path_buf());
    args.destination = Some(dest.to_path_buf());
    if mirror {
        args.mirror = true;
        args.delete = true;
    }
    if include_empty {
        args.empty_dirs = true;
    }
    // Delegate to legacy path by falling through to local copy section
    // Simplest: call main copy pipeline by replicating needed subset
    // For now, we route through network URL handling and local pipeline below
    // Implementation: mimic what main does by re-parsing environment
    // This function will only be used early in main before heavy logic
    // Therefore, just return Ok and let main continue with source/dest in place.
    // In practice, we call this via early return, so instead:
    // We'll perform a small inline copy by invoking client or local copy.

    // Remote URL handling
    if url::parse_remote_url(src).is_some() && url::parse_remote_url(dest).is_some() {
        anyhow::bail!("Remote→remote transfers are not supported in this release");
    }
    if let Some(remote) = url::parse_remote_url(src) {
        return client_pull(remote, dest, &args);
    }
    if let Some(remote) = url::parse_remote_url(dest) {
        return client_push(remote, src, &args);
    }
    // Local single-file or directory copy
    // Reuse existing local code by calling a helper
    run_local(src, dest, mirror, include_empty, &args)
}

// Minimal wrapper to reuse existing local flow from main
fn run_local(
    src_path: &Path,
    dest_path: &Path,
    mirror: bool,
    include_empty: bool,
    args: &Args,
) -> Result<()> {
    // The main function already implements the full local copy pipeline.
    // To avoid duplicating, we call into that pipeline by reproducing its steps here.
    // For brevity and to avoid code duplication, we will just return an error that instructs to use core path.
    // However, we implement direct fallback: if it's a file, copy_single_file; otherwise continue with enumerate path below.
    if src_path.is_file() {
        return copy_single_file(src_path, dest_path, false, args.verbose);
    }
    // Build FileFilter
    let filter = FileFilter {
        exclude_files: vec![],
        exclude_dirs: vec![],
        min_size: None,
        max_size: None,
        include_empty_dirs: include_empty,
    };
    let preserve_links = args.sl;
    let initial_entries = if !preserve_links {
        crate::fs_enum::enumerate_directory_deref_filtered(src_path, &filter)
    } else {
        enumerate_directory_filtered(src_path, &filter)
    }?;
    let copy_jobs: Vec<CopyJob> = initial_entries
        .into_iter()
        .map(|entry| CopyJob {
            entry,
            start_offset: 0,
        })
        .collect();
    let (small, medium, large) = categorize_files(copy_jobs);
    let buffer_sizer = Arc::new(BufferSizer::new());
    let logger: Arc<dyn Logger + Send + Sync> = Arc::new(NoopLogger);
    // Small files via tar
    let mut total_files_copied = 0u64;
    let mut total_bytes = 0u64;
    if !small.is_empty() {
        match process_small_files_tar(&small, src_path, dest_path, false, &*logger) {
            Ok((f, b)) => {
                total_files_copied += f;
                total_bytes += b;
            }
            Err(e) => {
                eprintln!("Error processing small files via TAR: {}", e);
            }
        }
    }
    // Medium files in parallel
    if !medium.is_empty() {
        let pairs = prepare_copy_pairs(&medium, src_path, dest_path);
        let stats = parallel_copy_files(pairs, buffer_sizer.clone(), false, &*logger);
        total_files_copied += stats.files_copied;
        total_bytes += stats.bytes_copied;
    }
    // Large files chunked or mmap
    for job in &large {
        let dst = compute_destination(&job.entry.path, src_path, dest_path);
        let bytes = if !false && cfg!(unix) {
            mmap_copy_file(&job.entry.path, &dst)?
        } else {
            chunked_copy_file(
                &job.entry.path,
                &dst,
                &BufferSizer::new(),
                false,
                None,
                &*logger,
            )?
        };
        total_files_copied += 1;
        total_bytes += bytes;
    }
    // Mirror deletions
    if mirror {
        let _ = handle_mirror_deletion(src_path, dest_path, &filter, args.verbose, args.dry_run)?;
    }
    println!(
        "Copied {} files ({:.2} MB)",
        total_files_copied,
        total_bytes as f64 / 1_048_576.0
    );
    Ok(())
}

impl Args {
    fn clone_for_copylike(&self) -> Self {
        Self {
            ..self.clone_shallow()
        }
    }
    fn clone_shallow(&self) -> Self {
        Args {
            source: None,
            destination: None,
            threads: self.threads,
            net_workers: self.net_workers,
            net_chunk_mb: self.net_chunk_mb,
            verbose: self.verbose,
            progress: self.progress,
            mirror: false,
            delete: false,
            update: false,
            subdirs: self.subdirs,
            empty_dirs: self.empty_dirs,
            no_empty_dirs: self.no_empty_dirs,
            dry_run: self.dry_run,
            exclude_files: self.exclude_files.clone(),
            exclude_dirs: self.exclude_dirs.clone(),
            checksum: self.checksum,
            force_tar: self.force_tar,
            no_tar: self.no_tar,
            no_verify: self.no_verify,
            no_restart: self.no_restart,
            // serve_legacy, bind, root removed
            log_file: self.log_file.clone(),
            sl: self.sl,
            #[cfg(windows)]
            sj: self.sj,
            xj: self.xj,
            xjd: self.xjd,
            xjf: self.xjf,
            ludicrous_speed: self.ludicrous_speed,
            never_tell_me_the_odds: self.never_tell_me_the_odds,
            complete_remote: None,
            command: None,
        }
    }
}

/// Check if path is a network location
fn is_network_path(_path: &Path) -> bool {
    false
}

/// Determine if tar streaming would be beneficial with dynamic threshold
fn should_use_tar(small_files: &[CopyJob], is_network: bool) -> bool {
    let count = small_files.len();

    // Quick analysis (O(1) operations only)
    let total_size: u64 = small_files.iter().map(|j| j.entry.size).sum();
    let avg_size = if count > 0 {
        total_size / count as u64
    } else {
        0
    };

    // Dynamic threshold based on file characteristics
    let threshold = if false
    /* local only */
    {
        100 // Network always uses lower threshold
    } else {
        // Local dynamic threshold based on average file size
        if avg_size < 1024 {
            // Very tiny files (<1KB avg)
            200 // Lower threshold - tar helps more
        } else if avg_size < 8192 {
            // Small files (<8KB avg)
            500 // Standard threshold
        } else {
            // Larger small files (>8KB avg)
            1000 // Higher threshold - parallel copy better
        }
    };

    count > threshold
}

/// Copy a single file
fn copy_single_file(src: &Path, dst: &Path, is_network: bool, verbose: bool) -> Result<()> {
    if verbose {
        println!("Copying single file...");
    }

    let buffer_sizer = BufferSizer::new();
    let bytes = if !false
    /* local only */
    {
        // Use Windows CopyFileW for local copies
        windows_copyfile(src, dst)?
    } else {
        copy::copy_file(
            src,
            dst,
            &buffer_sizer,
            false, /* local only */
            &crate::logger::NoopLogger,
        )?
    };

    println!("Copied {} bytes", bytes);
    Ok(())
}

/// Process small files using tar streaming
fn process_small_files_tar(
    jobs: &[CopyJob],
    src_root: &Path,
    dst_root: &Path,
    _show_progress: bool,
    logger: &dyn Logger,
) -> Result<(u64, u64)> {
    logger.start(src_root, dst_root);
    // Build explicit file list: (source_path, tar_relative_path)
    let mut file_list: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(jobs.len());
    for job in jobs {
        let rel_path = job
            .entry
            .path
            .strip_prefix(src_root)
            .unwrap_or(&job.entry.path)
            .to_path_buf();
        file_list.push((job.entry.path.clone(), rel_path));
    }
    let config = TarConfig::default();
    let result = tar_stream_transfer_list(&file_list, dst_root, &config, false)?;
    logger.done(result.0, result.1, 0.0);
    Ok(result)
}

/// Prepare source-destination pairs for copying
fn prepare_copy_pairs(
    files: &[CopyJob],
    src_root: &Path,
    dst_root: &Path,
) -> Vec<(FileEntry, PathBuf)> {
    files
        .iter()
        .map(|entry| {
            let dst = compute_destination(&entry.entry.path, src_root, dst_root);
            (entry.entry.clone(), dst)
        })
        .collect()
}

/// Compute destination path for a file
fn compute_destination(src_file: &Path, src_root: &Path, dst_root: &Path) -> PathBuf {
    if let Ok(rel_path) = src_file.strip_prefix(src_root) {
        dst_root.join(rel_path)
    } else {
        dst_root.join(src_file.file_name().unwrap_or_default())
    }
}

/// Handle mirror mode deletion (delete extra files in destination)
fn handle_mirror_deletion(
    source: &Path,
    destination: &Path,
    filter: &FileFilter,
    verbose: bool,
    dry_run: bool,
) -> Result<(u64, u64)> {
    use std::collections::HashSet;

    // Get all files that should exist (from source)
    let source_entries = enumerate_directory_filtered(source, filter)?;
    #[cfg(windows)]
    fn keyify(p: &Path) -> String {
        p.to_string_lossy().to_ascii_lowercase()
    }
    #[cfg(not(windows))]
    fn keyify(p: &Path) -> String {
        p.to_string_lossy().to_string()
    }

    let mut source_files: HashSet<String> = HashSet::new();
    let mut source_dirs: HashSet<String> = HashSet::new();

    for entry in &source_entries {
        let rel_path = entry.path.strip_prefix(source).unwrap_or(&entry.path);
        let dest_path = destination.join(rel_path);

        if entry.is_directory {
            source_dirs.insert(keyify(&dest_path));
        } else {
            source_files.insert(keyify(&dest_path));
            // Also track the parent directories
            if let Some(parent) = dest_path.parent() {
                let mut current = parent;
                while current != destination && current.parent().is_some() {
                    source_dirs.insert(keyify(current));
                    current = current.parent().context("Failed to get parent directory")?;
                }
            }
        }
    }

    // Scan destination to find extra files
    if !destination.exists() {
        return Ok((0, 0)); // Nothing to delete
    }

    let dest_entries = enumerate_directory_filtered(destination, &FileFilter::default())?;
    let mut files_to_delete = Vec::new();
    let mut dirs_to_delete = Vec::new();

    for entry in &dest_entries {
        if entry.is_directory {
            if !source_dirs.contains(&keyify(&entry.path)) {
                dirs_to_delete.push(entry.path.clone());
            }
        } else if !source_files.contains(&keyify(&entry.path)) {
            files_to_delete.push(entry.path.clone());
        }
    }

    let total_deletions = files_to_delete.len() + dirs_to_delete.len();

    if dry_run {
        if total_deletions > 0 {
            println!("\n=== Mirror Mode - Would Delete ===");
            println!("Extra files: {}", files_to_delete.len());
            println!("Extra directories: {}", dirs_to_delete.len());

            if verbose {
                if !files_to_delete.is_empty() {
                    println!("\n--- Files to delete ---");
                    for (_i, path) in files_to_delete.iter().enumerate() {
                        if _i < 10 {
                            println!("  {}", path.display());
                        } else if _i == 10 {
                            println!("  ... and {} more files", files_to_delete.len() - 10);
                            break;
                        }
                    }
                }
                if !dirs_to_delete.is_empty() {
                    println!("\n--- Directories to delete ---");
                    for (_i, path) in dirs_to_delete.iter().enumerate() {
                        if _i < 10 {
                            println!("  {}", path.display());
                        } else if _i == 10 {
                            println!("  ... and {} more directories", dirs_to_delete.len() - 10);
                            break;
                        }
                    }
                }
            }
        } else {
            println!("\n=== Mirror Mode - No extra files to delete ===");
        }
        return Ok((files_to_delete.len() as u64, dirs_to_delete.len() as u64));
    }

    // Actually delete files and directories
    let mut deleted_files = 0u64;
    let mut deleted_dirs = 0u64;

    // Delete files first
    for (_i, path) in files_to_delete.iter().enumerate() {
        // Simple deletion without progress display

        // Clear read-only recursively on Windows before attempting deletion
        #[cfg(windows)]
        blit::win_fs::clear_readonly_recursive(path);

        match std::fs::remove_file(path) {
            Ok(_) => {
                deleted_files += 1;
                if verbose {
                    println!("Deleted file: {}", path.display());
                }
            }
            Err(e) => {
                eprintln!("Failed to delete file {:?}: {}", path, e);
            }
        }
    }

    // Delete directories (in reverse order to handle nested dirs)
    dirs_to_delete.sort();
    dirs_to_delete.reverse(); // Delete deepest first

    for (_i, path) in dirs_to_delete.iter().enumerate() {
        // Simple deletion without progress display

        // Clear read-only recursively on Windows before attempting deletion
        #[cfg(windows)]
        blit::win_fs::clear_readonly_recursive(path);

        match std::fs::remove_dir(path) {
            Ok(_) => {
                deleted_dirs += 1;
                if verbose {
                    println!("Deleted directory: {}", path.display());
                }
            }
            Err(e) => {
                if verbose {
                    eprintln!(
                        "Failed to delete directory {:?}: {} (may not be empty)",
                        path, e
                    );
                }
            }
        }
    }

    Ok((deleted_files, deleted_dirs))
}

// Interactivity removed: previous resume/restart logic deleted for non-interactive behavior

/// Merge copy statistics
fn merge_stats(total: &mut CopyStats, other: CopyStats) {
    total.files_copied += other.files_copied;
    total.bytes_copied += other.bytes_copied;
    total.errors.extend(other.errors);
}

// Server/daemon hosting code moved to blitd binary
// This binary (blit) is the client sync tool (local and network operations)

fn client_push(remote: url::RemoteDest, src_root: &Path, args: &crate::Args) -> Result<()> {
    if !src_root.exists() {
        anyhow::bail!("Source does not exist: {:?}", src_root);
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for client push")?;
    rt.block_on(net_async::client::push(
        &remote.host,
        remote.port,
        &remote.path,
        src_root,
        args,
    ))
}

fn client_pull(remote: url::RemoteDest, dest_root: &Path, args: &crate::Args) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for client pull")?;
    rt.block_on(net_async::client::pull(
        &remote.host,
        remote.port,
        &remote.path,
        dest_root,
        args,
    ))
}

fn verify_trees(src: &Path, dest: &Path, checksum: bool) -> Result<VerifySummary> {
    // Direction inference: if dest is remote, do push-verify; if src is remote, do pull-verify
    if let Some(remote) = url::parse_remote_url(dest) {
        verify_local_vs_remote(src, &remote.host, remote.port, &remote.path, checksum)
    } else if let Some(remote_src) = url::parse_remote_url(src) {
        verify_remote_vs_local(
            &remote_src.host,
            remote_src.port,
            &remote_src.path,
            dest,
            checksum,
        )
    } else {
        verify_local_vs_local(src, dest, checksum)
    }
}

fn verify_local_vs_local(src: &Path, dest: &Path, checksum: bool) -> Result<VerifySummary> {
    use crate::fs_enum::enumerate_directory_filtered;
    use std::collections::{HashMap, HashSet};
    let filter = FileFilter {
        exclude_files: vec![],
        exclude_dirs: vec![],
        min_size: None,
        max_size: None,
        include_empty_dirs: true,
    };
    let left = enumerate_directory_filtered(src, &filter)?;
    let right = enumerate_directory_filtered(dest, &filter)?;
    let mut left_map: HashMap<String, &FileEntry> = HashMap::new();
    for e in &left {
        if !e.is_directory {
            let rel = e
                .path
                .strip_prefix(src)
                .unwrap_or(&e.path)
                .to_string_lossy()
                .to_string();
            left_map.insert(rel, e);
        }
    }
    let mut right_map: HashMap<String, &FileEntry> = HashMap::new();
    for e in &right {
        if !e.is_directory {
            let rel = e
                .path
                .strip_prefix(dest)
                .unwrap_or(&e.path)
                .to_string_lossy()
                .to_string();
            right_map.insert(rel, e);
        }
    }
    let mut changed = 0usize;
    let mut extras = 0usize; // extras in dest
    let mut sample: Vec<VerifyEntry> = Vec::new();
    let keys: HashSet<_> = left_map
        .keys()
        .cloned()
        .chain(right_map.keys().cloned())
        .collect();
    for k in keys {
        match (left_map.get(&k), right_map.get(&k)) {
            (Some(l), Some(r)) => {
                let mut differs = false;
                if checksum {
                    let lh = hash_file(&l.path)?;
                    let rh = hash_file(&r.path)?;
                    differs = lh != rh;
                } else {
                    differs = l.size != r.size;
                }
                if differs {
                    changed += 1;
                    if sample.len() < 50 {
                        sample.push(VerifyEntry {
                            kind: "changed",
                            path: k.clone(),
                            size_src: l.size,
                            size_dest: r.size,
                            mtime_src: 0,
                            mtime_dest: 0,
                        });
                    }
                }
            }
            (Some(l), None) => {
                changed += 1;
                if sample.len() < 50 {
                    sample.push(VerifyEntry {
                        kind: "missing_dest",
                        path: k.clone(),
                        size_src: l.size,
                        size_dest: 0,
                        mtime_src: 0,
                        mtime_dest: 0,
                    });
                }
            }
            (None, Some(r)) => {
                extras += 1;
                if sample.len() < 50 {
                    sample.push(VerifyEntry {
                        kind: "extra_dest",
                        path: k.clone(),
                        size_src: 0,
                        size_dest: r.size,
                        mtime_src: 0,
                        mtime_dest: 0,
                    });
                }
            }
            _ => {}
        }
    }
    Ok(VerifySummary {
        identical: changed == 0 && extras == 0,
        changed_count: changed,
        extras_count: extras,
        sample,
    })
}

fn verify_local_vs_remote(
    src: &Path,
    host: &str,
    port: u16,
    remote_path: &Path,
    _checksum: bool,
) -> Result<VerifySummary> {
    use std::collections::{HashMap, HashSet};
    // Enumerate local files
    let filter = FileFilter {
        exclude_files: vec![],
        exclude_dirs: vec![],
        min_size: None,
        max_size: None,
        include_empty_dirs: true,
    };
    let left = enumerate_directory_filtered(src, &filter)?;
    let mut local_map: HashMap<String, FileEntry> = HashMap::new();
    for e in left {
        if !e.is_directory {
            let rel = e
                .path
                .strip_prefix(src)
                .unwrap_or(&e.path)
                .to_string_lossy()
                .to_string();
            local_map.insert(rel, e);
        }
    }
    // Enumerate remote files recursively and hash remotely
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for verify")?;
    let remote_files = rt.block_on(net_async::client::list_files_recursive(
        host,
        port,
        remote_path,
    ))?;
    let remote_hashes = rt.block_on(net_async::client::remote_hashes(
        host,
        port,
        remote_path,
        &remote_files,
    ))?;
    let mut changed = 0usize;
    let mut extras = 0usize;
    let mut sample: Vec<VerifyEntry> = Vec::new();
    let keys: HashSet<_> = local_map
        .keys()
        .cloned()
        .chain(remote_files.iter().map(|p| p.to_string_lossy().to_string()))
        .collect();
    for k in keys {
        match (local_map.get(&k), remote_hashes.get(&k)) {
            (Some(l), Some(rh)) => {
                let lh = hash_file(&l.path)?;
                if &lh != rh {
                    changed += 1;
                    if sample.len() < 50 {
                        sample.push(VerifyEntry {
                            kind: "changed",
                            path: k.clone(),
                            size_src: l.size,
                            size_dest: l.size,
                            mtime_src: 0,
                            mtime_dest: 0,
                        });
                    }
                }
            }
            (Some(l), None) => {
                changed += 1;
                if sample.len() < 50 {
                    sample.push(VerifyEntry {
                        kind: "missing_remote",
                        path: k.clone(),
                        size_src: l.size,
                        size_dest: 0,
                        mtime_src: 0,
                        mtime_dest: 0,
                    });
                }
            }
            (None, Some(_)) => {
                extras += 1;
                if sample.len() < 50 {
                    sample.push(VerifyEntry {
                        kind: "extra_remote",
                        path: k.clone(),
                        size_src: 0,
                        size_dest: 0,
                        mtime_src: 0,
                        mtime_dest: 0,
                    });
                }
            }
            (None, None) => {}
        }
    }
    Ok(VerifySummary {
        identical: changed == 0 && extras == 0,
        changed_count: changed,
        extras_count: extras,
        sample,
    })
}

fn verify_remote_vs_local(
    host: &str,
    port: u16,
    remote_path: &Path,
    dest: &Path,
    _checksum: bool,
) -> Result<VerifySummary> {
    use std::collections::{HashMap, HashSet};
    // Enumerate remote files and local files
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for verify")?;
    let remote_files = rt.block_on(net_async::client::list_files_recursive(
        host,
        port,
        remote_path,
    ))?;
    let remote_hashes = rt.block_on(net_async::client::remote_hashes(
        host,
        port,
        remote_path,
        &remote_files,
    ))?;
    let filter = FileFilter {
        exclude_files: vec![],
        exclude_dirs: vec![],
        min_size: None,
        max_size: None,
        include_empty_dirs: true,
    };
    let right = enumerate_directory_filtered(dest, &filter)?;
    let mut local_map: HashMap<String, FileEntry> = HashMap::new();
    for e in right {
        if !e.is_directory {
            let rel = e
                .path
                .strip_prefix(dest)
                .unwrap_or(&e.path)
                .to_string_lossy()
                .to_string();
            local_map.insert(rel, e);
        }
    }
    let mut changed = 0usize;
    let mut extras = 0usize;
    let mut sample: Vec<VerifyEntry> = Vec::new();
    let keys: HashSet<_> = remote_files
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .chain(local_map.keys().cloned())
        .collect();
    for k in keys {
        match (remote_hashes.get(&k), local_map.get(&k)) {
            (Some(rh), Some(l)) => {
                let lh = hash_file(&l.path)?;
                if &lh != rh {
                    changed += 1;
                    if sample.len() < 50 {
                        sample.push(VerifyEntry {
                            kind: "changed",
                            path: k.clone(),
                            size_src: l.size,
                            size_dest: l.size,
                            mtime_src: 0,
                            mtime_dest: 0,
                        });
                    }
                }
            }
            (Some(_), None) => {
                extras += 1;
                if sample.len() < 50 {
                    sample.push(VerifyEntry {
                        kind: "extra_local",
                        path: k.clone(),
                        size_src: 0,
                        size_dest: 0,
                        mtime_src: 0,
                        mtime_dest: 0,
                    });
                }
            }
            (None, Some(l)) => {
                changed += 1;
                if sample.len() < 50 {
                    sample.push(VerifyEntry {
                        kind: "missing_local",
                        path: k.clone(),
                        size_src: l.size,
                        size_dest: 0,
                        mtime_src: 0,
                        mtime_dest: 0,
                    });
                }
            }
            (None, None) => {}
        }
    }
    Ok(VerifySummary {
        identical: changed == 0 && extras == 0,
        changed_count: changed,
        extras_count: extras,
        sample,
    })
}

fn client_complete_remote(comp_str: &str) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for completion")?;
    rt.block_on(net_async::client::complete_remote(comp_str))
}

fn hash_file(path: &Path) -> Result<[u8; 32]> {
    use std::io::Read as _;
    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(hasher.finalize().as_bytes());
    Ok(out)
}
