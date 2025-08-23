//! RoboSync Rev3 - Simplified, high-performance file synchronization
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
mod tar_stream;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::buffer::BufferSizer;
use crate::copy::{
    chunked_copy_file, file_needs_copy, parallel_copy_files, windows_copyfile, CopyStats,
};
use crate::fs_enum::{
    categorize_files, enumerate_directory_filtered, CopyJob, FileEntry, FileFilter,
};
use crate::logger::{Logger, NoopLogger, TextLogger};
use crate::tar_stream::{tar_stream_transfer_list, TarConfig};

/// Command-line arguments
#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "RoboSync - Fast local + daemon sync with rsync-style delta (push/pull) and robocopy-style CLI"
)]
struct Args {
    /// Source directory or file (not required with --serve)
    #[arg(required_unless_present = "serve")]
    source: Option<PathBuf>,

    /// Destination directory or file (not required with --serve)
    #[arg(required_unless_present = "serve")]
    destination: Option<PathBuf>,

    /// Number of threads (0 = auto)
    #[arg(short = 't', long, default_value_t = 0)]
    threads: usize,

    /// Show processing stages and operations (discovery, categorization, etc.)
    #[arg(short, long)]
    verbose: bool,

    /// Show individual file operations as they happen
    #[arg(short, long)]
    progress: bool,

    /// Mirror mode - copy and delete extra files (same as --delete)
    #[arg(long = "mir", alias = "mirror")]
    mirror: bool,

    /// Delete extra files in destination
    #[arg(long, alias = "del", alias = "purge")]
    delete: bool,

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

    /// Run as daemon (server mode)
    #[arg(long)]
    serve: bool,

    /// Bind address for --serve
    #[arg(long, default_value = "0.0.0.0:9031")]
    bind: String,

    /// Root directory for --serve
    #[arg(long, default_value = "/")]
    root: PathBuf,
    /// Write JSONL log entries to file
    #[arg(long = "log-file")]
    log_file: Option<PathBuf>,
}

fn main() -> Result<()> {
    // Set up Ctrl-C handler
    ctrlc::set_handler(move || {
        eprintln!("\nInterrupted by user. Exiting (Ctrl-C)...");
        // Exit immediately with 130 (128 + SIGINT)
        std::process::exit(130);
    })
    .expect("Error setting Ctrl-C handler");

    let args = Args::parse();

    // Server mode
    if args.serve {
        return server_main(&args.bind, &args.root);
    }
    // Choose logger once; zero overhead in hot paths with NoopLogger
    let logger: Arc<dyn Logger + Send + Sync> = if let Some(ref p) = args.log_file {
        match TextLogger::new(p) {
            Ok(l) => Arc::new(l),
            Err(_) => Arc::new(NoopLogger),
        }
    } else {
        Arc::new(NoopLogger)
    };

    let start = Instant::now();

    // Handle delete/mirror flags (robocopy compatibility)
    let delete_extra = args.delete || args.mirror;

    // Extract required positional args in non-serve mode
    let dest_path = args
        .destination
        .clone()
        .ok_or_else(|| anyhow::anyhow!("destination required unless --serve"))?;
    let src_path = args
        .source
        .clone()
        .ok_or_else(|| anyhow::anyhow!("source required unless --serve"))?;

    // Network paths: support push (remote destination) and pull (remote source)
    if let Some(remote) = parse_remote_url(&dest_path) {
        return client_push(remote, &src_path, &args);
    }
    if let Some(remote_src) = parse_remote_url(&src_path) {
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
            "{} RoboSync {}...",
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
        println!(
            "RoboSync {} - Linux/macOS Optimized",
            env!("CARGO_PKG_VERSION")
        );
        println!("Source: {:?}", src_path);
        println!("Destination: {:?}", dest_path);
        println!("Network transfer: {}", is_network);
        if delete_extra {
            println!("Delete mode: enabled (mirror/purge)");
        }
    }

    // Set thread count for rayon
    if args.threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(args.threads)
            .build_global()
            .context("Failed to set thread count")?;
    }

    // Check if source is a single file
    if src_path.is_file() {
        return copy_single_file(&src_path, &dest_path, is_network, args.progress);
    }

    // Enumerate files with progress
    if args.verbose {
        println!("Enumerating files...");
    }

    // Decide empty-dir behavior (Robocopy semantics: --mir implies including empties)
    let include_empty_dirs = if delete_extra {
        if args.subdirs || args.no_empty_dirs {
            if args.verbose { println!("Note: --mir implies --empty-dirs; including empty directories."); }
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

    let initial_entries = enumerate_directory_filtered(&src_path, &filter)
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

    // Filter out files that don't need copying (mirror mode optimization)
    let copy_jobs = if delete_extra {
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
        let use_tar = !args.no_tar && (args.force_tar || should_use_tar(&small, is_network));
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
                    is_network,
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
            let stats =
                parallel_copy_files(medium_pairs, buffer_sizer_clone, is_network, &*logger_clone);

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

            let mut stats = CopyStats::default();

            for entry in &large_files {
                let dst = compute_destination(&entry.entry.path, &source, &destination);

                // Simple chunked copy for all large files
                match chunked_copy_file(
                    &entry.entry.path,
                    &dst,
                    &buffer_sizer_clone,
                    is_network,
                    None,
                    &*logger_clone,
                ) {
                    Ok(bytes) => {
                        stats.add_file(bytes);

                        if show_files {
                            println!(
                                "  Chunked: {} → {} ({} bytes)",
                                entry.entry.path.display(),
                                dst.display(),
                                bytes
                            );
                        }
                    }
                    Err(e) => {
                        stats.add_error(format!("Failed to copy {:?}: {}", entry.entry.path, e));
                    }
                }
            }

            let _ = tx_clone.send(("large", stats));
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
    let threshold = if is_network {
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
    let bytes = if !is_network {
        // Use Windows CopyFileW for local copies
        windows_copyfile(src, dst)?
    } else {
        copy::copy_file(
            src,
            dst,
            &buffer_sizer,
            is_network,
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
    let mut source_files: HashSet<PathBuf> = HashSet::new();
    let mut source_dirs: HashSet<PathBuf> = HashSet::new();

    for entry in &source_entries {
        let rel_path = entry.path.strip_prefix(source).unwrap_or(&entry.path);
        let dest_path = destination.join(rel_path);

        if entry.is_directory {
            source_dirs.insert(dest_path);
        } else {
            source_files.insert(dest_path.clone());
            // Also track the parent directories
            if let Some(parent) = dest_path.parent() {
                let mut current = parent;
                while current != destination && current.parent().is_some() {
                    source_dirs.insert(current.to_path_buf());
                    current = current.parent().unwrap();
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
            if !source_dirs.contains(&entry.path) {
                dirs_to_delete.push(entry.path.clone());
            }
        } else if !source_files.contains(&entry.path) {
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
/// Remove leftover `.robosync_temp_*` directories from previous runs (best-effort)
// --- Daemon and client URL helpers ---

#[derive(Debug, Clone)]
struct RemoteDest {
    host: String,
    port: u16,
    path: PathBuf,
}

fn parse_remote_url(path: &Path) -> Option<RemoteDest> {
    let s = path.to_string_lossy();
    let prefix = "robosync://";
    if !s.starts_with(prefix) {
        return None;
    }
    let rest = &s[prefix.len()..];
    let (hp, p) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = match hp.split_once(':') {
        Some((h, pr)) => (h.to_string(), pr.parse().unwrap_or(9031)),
        None => (hp.to_string(), 9031),
    };
    Some(RemoteDest {
        host,
        port,
        path: PathBuf::from("/".to_string() + p),
    })
}

fn server_main(bind: &str, root: &Path) -> Result<()> {
    net::serve(bind, root)
}

fn client_push(remote: RemoteDest, src_root: &Path, args: &Args) -> Result<()> {
    if !src_root.exists() {
        anyhow::bail!("Source does not exist: {:?}", src_root);
    }
    net::client_start(&remote.host, remote.port, &remote.path, src_root, args)
}

fn client_pull(remote: RemoteDest, dest_root: &Path, args: &Args) -> Result<()> {
    net::client_pull(&remote.host, remote.port, &remote.path, dest_root, args)
}
