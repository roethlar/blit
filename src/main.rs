//! RoboSync Rev3 - Simplified, Windows-optimized file synchronization
//! 
//! Design goals:
//! - Saturate 10GbE network (1+ GB/s throughput)
//! - Minimal startup overhead
//! - Direct dispatch based on file size
//! - No complex abstractions

mod buffer;
mod copy;
mod tar_stream;
mod windows_enum;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use anyhow::{Result, Context};
use clap::Parser;

use crate::buffer::BufferSizer;
use crate::copy::{CopyStats, chunked_copy_file, parallel_copy_files, windows_copyfile, file_needs_copy};
use crate::tar_stream::{tar_stream_transfer, TarConfig};
use crate::windows_enum::{enumerate_directory_filtered, categorize_files, FileEntry, FileFilter};

/// Command-line arguments
#[derive(Parser, Debug)]
#[command(author, version, about = "RoboSync v2.1 - High-performance file synchronization with robocopy-style CLI")]
struct Args {
    /// Source directory or file
    source: PathBuf,
    
    /// Destination directory or file
    destination: PathBuf,
    
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
}

fn main() -> Result<()> {
    // Set up Ctrl-C handler
    ctrlc::set_handler(|| {
        eprintln!("\nInterrupted by user");
        std::process::exit(1);
    }).expect("Error setting Ctrl-C handler");
    
    let args = Args::parse();
    let start = Instant::now();
    
    // Handle delete/mirror flags (robocopy compatibility)
    let delete_extra = args.delete || args.mirror;
    
    // Detect if this is a network transfer
    let is_network = is_network_path(&args.destination);
    
    // Simple activity indicator (no performance impact)
    let show_activity = !(args.verbose || args.progress); // Only show simple indicator if not verbose or progress
    
    // Simple activity indicator
    if show_activity {
        print!("RoboSync v2.1.11...");
        std::io::Write::flush(&mut std::io::stdout()).ok();
    }
    
    // Dry run mode - just list what would be copied
    if args.dry_run {
        println!("DRY RUN MODE - No files will be copied");
    }
    
    if args.verbose {
        println!("RoboSync v2.1.11 - Linux/macOS Optimized");
        println!("Source: {:?}", args.source);
        println!("Destination: {:?}", args.destination);
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
    if args.source.is_file() {
        return copy_single_file(&args.source, &args.destination, is_network, args.progress);
    }
    
    // Enumerate files with progress
    if args.verbose {
        println!("Enumerating files...");
    }
    
    // Build filter from CLI arguments
    let filter = FileFilter {
        exclude_files: args.exclude_files.clone(),
        exclude_dirs: args.exclude_dirs.clone(),
        min_size: None,
        max_size: None,
        include_empty_dirs: if args.subdirs {
            false  // -s means skip empty dirs
        } else if args.empty_dirs {
            true   // -e means include empty dirs
        } else {
            true   // default is include empty dirs (/E behavior)
        },
    };
    
    if args.verbose {
        if !args.exclude_dirs.is_empty() {
            println!("Excluding directories: {:?}", args.exclude_dirs);
        }
        if !args.exclude_files.is_empty() {
            println!("Excluding files: {:?}", args.exclude_files);
        }
    }
    
    let entries = enumerate_directory_filtered(&args.source, &filter)
        .context("Failed to enumerate source directory")?;
    
    let total_files = entries.len();
    let total_size: u64 = entries.iter().map(|e| e.size).sum();
    
    if show_activity {
        print!(" found {}, copying...", total_files);
        std::io::Write::flush(&mut std::io::stdout()).ok();
    } else if args.verbose {
        println!("Found {} files ({:.2} GB)", total_files, total_size as f64 / 1_073_741_824.0);
    }
    
    // Filter out files that don't need copying (mirror mode optimization)
    let entries = if delete_extra {
        if show_activity {
            print!(" comparing...");
            std::io::Write::flush(&mut std::io::stdout()).ok();
        }
        
        use rayon::prelude::*;
        entries.into_par_iter()
            .filter(|entry| {
                let dst = compute_destination(&entry.path, &args.source, &args.destination);
                match file_needs_copy(&entry.path, &dst, args.checksum) {
                    Ok(needs_copy) => needs_copy,
                    Err(_) => true, // Copy if we can't determine (file might not exist)
                }
            })
            .collect()
    } else {
        entries
    };
    
    // Categorize files by size
    let (small, medium, large) = categorize_files(entries);
    
    // Handle dry run mode
    if args.dry_run {
        println!("\n=== DRY RUN - Files that would be copied ===");
        println!("Small files (<1MB): {}", small.len());
        println!("Medium files (1-100MB): {}", medium.len());
        println!("Large files (>100MB): {}", large.len());
        println!("Total: {} files ({:.2} GB)", total_files, total_size as f64 / 1_073_741_824.0);
        
        if args.verbose {
            println!("\n--- Files to copy ---");
            for (i, entry) in small.iter().chain(medium.iter()).chain(large.iter()).enumerate() {
                if i < 20 { // Limit output
                    println!("  {} ({} bytes)", entry.path.display(), entry.size);
                } else if i == 20 {
                    println!("  ... and {} more files", total_files - 20);
                    break;
                }
            }
            
        }
        
        // Handle mirror mode deletion in dry run
        if delete_extra {
            println!("\nWould also delete extra files in destination.");
            println!("\nWould delete extra files in destination.");
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

    // Process all file categories concurrently using separate threads
    use std::sync::mpsc;
    use std::thread;

    let (tx, rx) = mpsc::channel::<(&str, CopyStats)>();
    let mut handles = Vec::new();

    // Thread 1: Process small files with tar streaming (if beneficial)
    if !small.is_empty() {
        let use_tar = !args.no_tar && (args.force_tar || should_use_tar(&small, is_network));
        let small_files = small;
        let source = args.source.clone();
        let destination = args.destination.clone();
        let buffer_sizer_clone = buffer_sizer.clone();
        let tx_clone = tx.clone();
        let verbose = args.verbose;
        let show_files = args.progress;

        let handle = thread::spawn(move || {
            let mut stats = CopyStats::default();

            if use_tar {
                if verbose {
                    println!("Using tar streaming for {} small files", small_files.len());
                }

                match process_small_files_tar(&small_files, &source, &destination, false) {
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
                stats = parallel_copy_files(small_pairs, buffer_sizer_clone, is_network);
            }

            let _ = tx_clone.send(("small", stats));
        });
        handles.push(handle);
    }

    // Thread 2: Process medium files in parallel
    if !medium.is_empty() {
        let medium_files = medium;
        let source = args.source.clone();
        let destination = args.destination.clone();
        let buffer_sizer_clone = buffer_sizer.clone();
        let tx_clone = tx.clone();
        let verbose = args.verbose;
        let show_files = args.progress;

        let handle = thread::spawn(move || {
            if verbose {
                println!("Processing {} medium files in parallel", medium_files.len());
            }

            let medium_pairs = prepare_copy_pairs(&medium_files, &source, &destination);
            let stats = parallel_copy_files(medium_pairs, buffer_sizer_clone, is_network);

            let _ = tx_clone.send(("medium", stats));
        });
        handles.push(handle);
    }

    // Thread 3: Process large files with chunked copy
    if !large.is_empty() {
        let large_files = large;
        let source = args.source.clone();
        let destination = args.destination.clone();
        let buffer_sizer_clone = buffer_sizer.clone();
        let tx_clone = tx.clone();
        let verbose = args.verbose;
        let show_files = args.progress;

        let handle = thread::spawn(move || {
            if verbose {
                println!("Processing {} large files", large_files.len());
            }

            let mut stats = CopyStats::default();

            for entry in &large_files {
                let dst = compute_destination(&entry.path, &source, &destination);

                // Simple chunked copy for all large files
                match chunked_copy_file(&entry.path, &dst, &buffer_sizer_clone, is_network, None) {
                    Ok(bytes) => {
                        stats.add_file(bytes);

                        if show_files {
                            println!("  Chunked: {} → {} ({} bytes)", 
                                entry.path.display(), dst.display(), bytes);
                        }
                    }
                    Err(e) => {
                        stats.add_error(format!("Failed to copy {:?}: {}", entry.path, e));
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
        
        let deletion_stats = handle_mirror_deletion(&args.source, &args.destination, &filter, args.progress, args.dry_run)?;
        
        if args.verbose && (deletion_stats.0 > 0 || deletion_stats.1 > 0) {
            println!("Deleted {} files and {} directories", deletion_stats.0, deletion_stats.1);
        }
    }
    
    // Finish progress and print results
    // Simple completion indicator
    if show_activity {
        println!(" done!");
    }
    
    // Print summary (always show)
    let elapsed = start.elapsed();
    if !args.progress || args.verbose {
        println!();
        println!("=== Copy Complete ===");
        println!("Files copied: {}", total_stats.files_copied);
        println!("Total size: {:.2} GB", total_stats.bytes_copied as f64 / 1_073_741_824.0);
        println!("Time: {:.2}s", elapsed.as_secs_f64());
        println!("Throughput: {:.2} MB/s", 
            (total_stats.bytes_copied as f64 / 1_048_576.0) / elapsed.as_secs_f64());
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
    #[cfg(windows)]
    {
        if let Some(s) = path.to_str() {
            // UNC paths are network
            if s.starts_with("\\\\") {
                return true;
            }
            // Check if drive is a network drive using Windows API
            if s.len() >= 2 && s.chars().nth(1) == Some(':') {
                use winapi::um::fileapi::GetDriveTypeW;
                use winapi::um::winbase::DRIVE_REMOTE;
                use std::ffi::OsStr;
                use std::os::windows::ffi::OsStrExt;
                
                let drive_root = format!("{}:\\", &s[0..1]);
                let drive_wide: Vec<u16> = OsStr::new(&drive_root)
                    .encode_wide()
                    .chain(Some(0))
                    .collect();
                
                let drive_type = unsafe { GetDriveTypeW(drive_wide.as_ptr()) };
                return drive_type == DRIVE_REMOTE;
            }
        }
    }
    false
}

/// Determine if tar streaming would be beneficial with dynamic threshold
fn should_use_tar(small_files: &[FileEntry], is_network: bool) -> bool {
    let count = small_files.len();
    
    // Quick analysis (O(1) operations only)
    let total_size: u64 = small_files.iter().map(|f| f.size).sum();
    let avg_size = if count > 0 { total_size / count as u64 } else { 0 };
    
    // Dynamic threshold based on file characteristics
    let threshold = if is_network {
        100  // Network always uses lower threshold
    } else {
        // Local dynamic threshold based on average file size
        if avg_size < 1024 {        // Very tiny files (<1KB avg)
            200                     // Lower threshold - tar helps more
        } else if avg_size < 8192 { // Small files (<8KB avg) 
            500                     // Standard threshold
        } else {                    // Larger small files (>8KB avg)
            1000                    // Higher threshold - parallel copy better
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
        copy::copy_file(src, dst, &buffer_sizer, is_network)?
    };
    
    println!("Copied {} bytes", bytes);
    Ok(())
}

/// Process small files using tar streaming
fn process_small_files_tar(
    files: &[FileEntry],
    src_root: &Path,
    dst_root: &Path,
    _show_progress: bool,
) -> Result<(u64, u64)> {
    // Create a temporary directory structure for tar in destination directory
    // This avoids /tmp space issues and puts temp files where we have space
    let temp_src = dst_root.join(format!(".robosync_temp_{}", std::process::id()));
    std::fs::create_dir_all(&temp_src)?;
    
    // Link or copy files to temp structure
    for entry in files {
        let rel_path = entry.path.strip_prefix(src_root).unwrap_or(&entry.path);
        let temp_path = temp_src.join(rel_path);
        
        if let Some(parent) = temp_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        
        #[cfg(windows)]
        {
            // Try hard link first, fall back to copy
            if std::fs::hard_link(&entry.path, &temp_path).is_err() {
                std::fs::copy(&entry.path, &temp_path)?;
            }
        }
        
        #[cfg(not(windows))]
        {
            std::fs::copy(&entry.path, &temp_path)?;
        }
    }
    
    // Stream via tar  
    let config = TarConfig::default();
    let result = tar_stream_transfer(&temp_src, dst_root, &config, false)?;
    
    // Cleanup temp directory
    let _ = std::fs::remove_dir_all(&temp_src);
    
    Ok(result)
}

/// Prepare source-destination pairs for copying
fn prepare_copy_pairs(
    files: &[FileEntry],
    src_root: &Path,
    dst_root: &Path,
) -> Vec<(FileEntry, PathBuf)> {
    files.iter()
        .map(|entry| {
            let dst = compute_destination(&entry.path, src_root, dst_root);
            (entry.clone(), dst)
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
        let rel_path = entry.path.strip_prefix(source)
            .unwrap_or(&entry.path);
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
                    for (i, path) in files_to_delete.iter().enumerate() {
                        if i < 10 {
                            println!("  {}", path.display());
                        } else if i == 10 {
                            println!("  ... and {} more files", files_to_delete.len() - 10);
                            break;
                        }
                    }
                }
                if !dirs_to_delete.is_empty() {
                    println!("\n--- Directories to delete ---");
                    for (i, path) in dirs_to_delete.iter().enumerate() {
                        if i < 10 {
                            println!("  {}", path.display());
                        } else if i == 10 {
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
    for (i, path) in files_to_delete.iter().enumerate() {
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
    
    for (i, path) in dirs_to_delete.iter().enumerate() {
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
                    eprintln!("Failed to delete directory {:?}: {} (may not be empty)", path, e);
                }
            }
        }
    }
    
    Ok((deleted_files, deleted_dirs))
}

/// Merge copy statistics
fn merge_stats(total: &mut CopyStats, other: CopyStats) {
    total.files_copied += other.files_copied;
    total.bytes_copied += other.bytes_copied;
    total.errors.extend(other.errors);
}