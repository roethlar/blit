//! RoboSync Rev3 - Simplified, Windows-optimized file synchronization
//! 
//! Design goals:
//! - Saturate 10GbE network (1+ GB/s throughput)
//! - Minimal startup overhead
//! - Direct dispatch based on file size
//! - No complex abstractions

mod buffer;
mod copy;
mod progress;
mod tar_stream;
mod windows_enum;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use anyhow::{Result, Context};
use clap::Parser;
use progress::CargoProgress;

use crate::buffer::BufferSizer;
use crate::copy::{CopyStats, chunked_copy_file, parallel_copy_files, windows_copyfile};
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
    
    /// Verbose output
    #[arg(short, long)]
    verbose: bool,
    
    /// Show progress bars
    #[arg(short, long)]
    progress: bool,
    
    /// Mirror mode - copy and delete extra files (same as --delete)
    #[arg(long, alias = "mirror", alias = "mir")]
    mirror: bool,
    
    /// Delete extra files in destination
    #[arg(long, alias = "del", alias = "purge")]
    delete: bool,
    
    /// Copy subdirectories, but not empty ones (/S)
    #[arg(short = 'S', long)]
    subdirs: bool,
    
    /// Copy subdirectories including empty ones (/E) - default behavior
    #[arg(short = 'E', long)]
    empty_dirs: bool,
    
    /// List only - don't copy files (dry run) (/L)
    #[arg(short = 'L', long, alias = "list-only")]
    dry_run: bool,
    
    /// Exclude files matching patterns (/XF)
    #[arg(long = "xf", action = clap::ArgAction::Append)]
    exclude_files: Vec<String>,
    
    /// Exclude directories matching patterns (/XD)
    #[arg(long = "xd", action = clap::ArgAction::Append)]
    exclude_dirs: Vec<String>,
    
    /// Number of retries on failed copies (/R)
    #[arg(short = 'R', long = "retry", default_value_t = 3)]
    retries: u32,
    
    /// Wait time between retries in seconds (/W)
    #[arg(short = 'W', long = "wait", default_value_t = 1)]
    wait_time: u64,
    
    /// Force tar streaming for small files
    #[arg(long)]
    force_tar: bool,
    
    /// Disable tar streaming
    #[arg(long)]
    no_tar: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let start = Instant::now();
    
    // Handle delete/mirror flags (robocopy compatibility)
    let delete_extra = args.delete || args.mirror;
    
    // Detect if this is a network transfer
    let is_network = is_network_path(&args.destination);
    
    // Initialize progress display
    let progress_display = if args.progress {
        Some(CargoProgress::new(args.verbose))
    } else {
        None
    };
    
    // Dry run mode - just list what would be copied
    if args.dry_run {
        if let Some(ref p) = progress_display {
            p.set_status("DRY RUN", 0, 0, Some("no files will be copied"));
        }
        println!("DRY RUN MODE - No files will be copied");
    }
    
    if args.verbose {
        println!("RoboSync v2.1 - Linux/macOS Optimized");
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
        return copy_single_file(&args.source, &args.destination, is_network, args.verbose);
    }
    
    // Enumerate files with progress
    if let Some(ref p) = progress_display {
        p.set_status("Scanning", 0, 0, Some("discovering files..."));
    } else if args.verbose {
        println!("Enumerating files...");
    }
    
    // Build file filter from command line args
    let filter = FileFilter {
        exclude_files: args.exclude_files.clone(),
        exclude_dirs: args.exclude_dirs.clone(),
        min_size: None,
        max_size: None,
        include_empty_dirs: !args.subdirs, // /S means no empty dirs
    };
    
    let entries = enumerate_directory_filtered(&args.source, &filter)
        .context("Failed to enumerate source directory")?;
    
    let total_files = entries.len();
    let total_size: u64 = entries.iter().map(|e| e.size).sum();
    
    if let Some(ref p) = progress_display {
        p.set_status("Categorizing", total_files as u64, 0, Some(&format!("{:.1} GB", total_size as f64 / 1_073_741_824.0)));
    } else if args.verbose {
        println!("Found {} files ({:.2} GB)", total_files, total_size as f64 / 1_073_741_824.0);
    }
    
    // Separate directories from files
    let (files, directories): (Vec<_>, Vec<_>) = entries.into_iter()
        .partition(|entry| !entry.is_directory);
    
    // Categorize files by size
    let (small, medium, large) = categorize_files(files);
    
    // Handle dry run mode
    if args.dry_run {
        println!("\n=== DRY RUN - Files that would be copied ===");
        println!("Small files (<1MB): {}", small.len());
        println!("Medium files (1-100MB): {}", medium.len());
        println!("Large files (>100MB): {}", large.len());
        println!("Directories: {}", directories.len());
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
            
            if !directories.is_empty() {
                println!("\n--- Directories to create ---");
                for (i, entry) in directories.iter().enumerate() {
                    if i < 10 {
                        println!("  {}", entry.path.display());
                    } else if i == 10 {
                        println!("  ... and {} more directories", directories.len() - 10);
                        break;
                    }
                }
            }
        }
        
        // Handle mirror mode deletion in dry run
        if delete_extra {
            println!("\nWould also delete extra files in destination.");
            let _deletion_stats = handle_mirror_deletion(&args.source, &args.destination, &filter, &progress_display, args.verbose, true)?;
        }
        
        return Ok(());
    }
    
    if let Some(ref p) = progress_display {
        p.set_status("Processing", 0, total_files as u64, Some(&format!("S:{} M:{} L:{}", small.len(), medium.len(), large.len())));
    } else if args.verbose {
        println!("Small files (<1MB): {}", small.len());
        println!("Medium files (1-100MB): {}", medium.len());
        println!("Large files (>100MB): {}", large.len());
    }
    
    // Track overall progress
    let mut files_processed = 0u64;
    let mut bytes_processed = 0u64;
    
    let mut total_stats = CopyStats::default();
    let buffer_sizer = Arc::new(BufferSizer::new());
    
    // Process small files with tar streaming (if beneficial)
    let use_tar = !args.no_tar && (args.force_tar || should_use_tar(&small, is_network));
    
    if use_tar && !small.is_empty() {
        if let Some(ref p) = progress_display {
            p.set_status("Streaming", 0, small.len() as u64, Some("tar batch"));
        } else if args.verbose {
            println!("Using tar streaming for {} small files", small.len());
        }
        
        let tar_result = process_small_files_tar(
            &small,
            &args.source,
            &args.destination,
            &progress_display,
        )?;
        
        files_processed += tar_result.0;
        bytes_processed += tar_result.1;
        total_stats.files_copied += tar_result.0;
        total_stats.bytes_copied += tar_result.1;
        
        if let Some(ref p) = progress_display {
            p.set_status_with_throughput("Streamed", files_processed, bytes_processed);
        }
    } else if !small.is_empty() {
        // Process small files individually
        if let Some(ref p) = progress_display {
            p.set_status("Copying", 0, small.len() as u64, Some("small files"));
        }
        
        let small_pairs = prepare_copy_pairs(&small, &args.source, &args.destination);
        let small_stats = parallel_copy_files(small_pairs, buffer_sizer.clone(), is_network, &progress_display);
        
        files_processed += small_stats.files_copied;
        bytes_processed += small_stats.bytes_copied;
        merge_stats(&mut total_stats, small_stats);
    }
    
    // Process medium files in parallel
    if !medium.is_empty() {
        if let Some(ref p) = progress_display {
            p.set_status("Copying", files_processed, total_files as u64, Some("medium files"));
        } else if args.verbose {
            println!("Processing {} medium files in parallel", medium.len());
        }
        
        let medium_pairs = prepare_copy_pairs(&medium, &args.source, &args.destination);
        let medium_stats = parallel_copy_files(medium_pairs, buffer_sizer.clone(), is_network, &progress_display);
        
        files_processed += medium_stats.files_copied;
        bytes_processed += medium_stats.bytes_copied;
        merge_stats(&mut total_stats, medium_stats);
    }
    
    // Process large files with chunked copy
    if !large.is_empty() {
        if let Some(ref p) = progress_display {
            p.set_status("Copying", files_processed, total_files as u64, Some("large files"));
        } else if args.verbose {
            println!("Processing {} large files", large.len());
        }
        
        for (i, entry) in large.iter().enumerate() {
            let dst = compute_destination(&entry.path, &args.source, &args.destination);
            
            if let Some(ref p) = progress_display {
                p.print_file_op("Copying", &entry.path.display().to_string());
                p.set_status("Copying", files_processed + i as u64 + 1, total_files as u64, 
                    Some(&format!("large file {}/{}", i + 1, large.len())));
            }
            
            match chunked_copy_file(&entry.path, &dst, &buffer_sizer, is_network, None) {
                Ok(bytes) => {
                    files_processed += 1;
                    bytes_processed += bytes;
                    total_stats.add_file(bytes);
                }
                Err(e) => {
                    total_stats.add_error(format!("Failed to copy {:?}: {}", entry.path, e));
                }
            }
        }
    }
    
    // Create empty directories if needed
    if !directories.is_empty() {
        if let Some(ref p) = progress_display {
            p.set_status("Creating", files_processed, total_files as u64, Some(&format!("{} directories", directories.len())));
        } else if args.verbose {
            println!("Creating {} directories", directories.len());
        }
        
        for entry in &directories {
            let dst = compute_destination(&entry.path, &args.source, &args.destination);
            
            if let Some(ref p) = progress_display {
                p.print_file_op("Creating", &entry.path.display().to_string());
            }
            
            match std::fs::create_dir_all(&dst) {
                Ok(_) => {
                    files_processed += 1;
                    total_stats.files_copied += 1;
                }
                Err(e) => {
                    total_stats.add_error(format!("Failed to create directory {:?}: {}", entry.path, e));
                }
            }
        }
    }
    
    // Handle mirror mode - delete extra files in destination
    if delete_extra {
        if let Some(ref p) = progress_display {
            p.set_status("Scanning", 0, 0, Some("destination for extra files"));
        } else if args.verbose {
            println!("Scanning destination for extra files...");
        }
        
        let deletion_stats = handle_mirror_deletion(&args.source, &args.destination, &filter, &progress_display, args.verbose, args.dry_run)?;
        
        if args.verbose && (deletion_stats.0 > 0 || deletion_stats.1 > 0) {
            println!("Deleted {} files and {} directories", deletion_stats.0, deletion_stats.1);
        }
    }
    
    // Finish progress and print results
    if let Some(ref p) = progress_display {
        if total_stats.errors.is_empty() {
            p.finish_success(total_stats.files_copied, total_stats.bytes_copied);
        } else {
            p.finish_error(&format!("{} errors occurred", total_stats.errors.len()));
        }
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
        if args.verbose {
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

/// Determine if tar streaming would be beneficial
fn should_use_tar(small_files: &[FileEntry], is_network: bool) -> bool {
    // Use tar if we have many small files
    // Thresholds tuned for Windows performance
    if is_network {
        small_files.len() > 100  // Lower threshold for network
    } else {
        small_files.len() > 500  // Higher threshold for local
    }
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
    progress_display: &Option<CargoProgress>,
) -> Result<(u64, u64)> {
    // Create a temporary directory structure for tar
    let temp_src = std::env::temp_dir().join(format!("robosync_{}", std::process::id()));
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
    let result = tar_stream_transfer(&temp_src, dst_root, &config, progress_display.is_some())?;
    
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
    progress_display: &Option<CargoProgress>,
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
        if let Some(ref p) = progress_display {
            p.print_file_op("Deleting", &path.display().to_string());
            p.set_status("Deleting", i as u64 + 1, total_deletions as u64, Some("extra files"));
        }
        
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
        if let Some(ref p) = progress_display {
            p.print_file_op("Deleting", &path.display().to_string());
            p.set_status("Deleting", files_to_delete.len() as u64 + i as u64 + 1, total_deletions as u64, Some("extra directories"));
        }
        
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