//! Optimized copy operations for Windows
//! Focus on 10GbE saturation with minimal overhead

use crate::logger::Logger;
use anyhow::{Context, Result};
use parking_lot::Mutex;
use rayon::prelude::*;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::buffer::BufferSizer;
use crate::fs_enum::FileEntry;

/// Check if a file needs to be copied (for mirror mode)
pub fn file_needs_copy(src: &Path, dst: &Path, use_checksum: bool) -> Result<bool> {
    // If destination doesn't exist, definitely copy
    if !dst.exists() {
        return Ok(true);
    }

    let src_meta = src.metadata()?;
    let dst_meta = dst.metadata()?;

    // If sizes differ, copy
    if src_meta.len() != dst_meta.len() {
        return Ok(true);
    }

    if use_checksum {
        // Checksum comparison (slower but accurate)
        Ok(files_have_different_content(src, dst)?)
    } else {
        // Fast timestamp comparison (default)
        let src_time = src_meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let dst_time = dst_meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);

        // Copy if source is newer (allow 2 second tolerance for filesystem precision)
        Ok(src_time
            .duration_since(dst_time)
            .is_ok_and(|diff| diff.as_secs() > 2))
    }
}

/// Compare file contents using fast hashing (for --checksum mode)
fn files_have_different_content(src: &Path, dst: &Path) -> Result<bool> {
    let src_hash = hash_file_content(src)?;
    let dst_hash = hash_file_content(dst)?;
    Ok(src_hash != dst_hash)
}

/// Fast file content hashing using BLAKE3
fn hash_file_content(path: &Path) -> Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024]; // 64KB chunks
    let mut file = File::open(path)?;

    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(hasher.finalize().into())
}

/// Statistics for copy operations
#[derive(Debug, Default, Clone)]
pub struct CopyStats {
    pub files_copied: u64,
    pub bytes_copied: u64,
    pub errors: Vec<String>,
}

impl CopyStats {
    pub fn add_file(&mut self, bytes: u64) {
        self.files_copied += 1;
        self.bytes_copied += bytes;
    }

    pub fn add_error(&mut self, error: String) {
        self.errors.push(error);
    }
}

/// Copy a single file with optimal buffer size
pub fn copy_file(
    src: &Path,
    dst: &Path,
    buffer_sizer: &BufferSizer,
    is_network: bool,
    logger: &dyn Logger,
) -> Result<u64> {
    logger.start(src, dst);

    let result: Result<u64> = (|| {
        // Get file size for buffer calculation
        let metadata = fs::metadata(src)?;
        let file_size = metadata.len();

        // Calculate optimal buffer size
        let buffer_size = buffer_sizer.calculate_buffer_size(file_size, is_network);

        // Create parent directory if needed
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }

        // Open files
        let mut reader = BufReader::with_capacity(buffer_size, File::open(src)?);
        let mut writer = BufWriter::with_capacity(buffer_size, File::create(dst)?);

        // Allocate copy buffer
        let mut buffer = vec![0u8; buffer_size];
        let mut total_bytes = 0u64;

        // Copy loop
        loop {
            let bytes_read = reader.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            writer.write_all(&buffer[..bytes_read])?;
            total_bytes += bytes_read as u64;
        }

        writer.flush()?;

        // Preserve basic metadata on Windows if available (stubbed)
        copy_windows_metadata(src, dst)?;

        Ok(total_bytes)
    })();

    match result {
        Ok(bytes) => {
            logger.copy_done(src, dst, bytes);
            Ok(bytes)
        }
        Err(e) => {
            logger.error("copy", src, &e.to_string());
            Err(e)
        }
    }
}

// Minimal stub: on all platforms, do nothing (safe, cross-platform)
fn copy_windows_metadata(_src: &Path, _dst: &Path) -> Result<()> {
    Ok(())
}

/// Parallel copy for medium-sized files (1-100MB)
pub fn parallel_copy_files(
    pairs: Vec<(FileEntry, PathBuf)>,
    buffer_sizer: Arc<BufferSizer>,
    is_network: bool,
    logger: &dyn Logger,
) -> CopyStats {
    let stats = Arc::new(Mutex::new(CopyStats::default()));

    // Use rayon for parallel copying
    pairs.par_iter().for_each(|(entry, dst)| {
        // Show progress for verbose mode
        // No progress display for maximum performance

        match copy_file(&entry.path, dst, &buffer_sizer, is_network, logger) {
            Ok(bytes) => {
                let mut s = stats.lock();
                s.add_file(bytes);
            }
            Err(e) => {
                let mut s = stats.lock();
                s.add_error(format!("Failed to copy {:?}: {}", entry.path, e));
            }
        }
    });

    // Extract the stats from Arc<Mutex<CopyStats>>
    Arc::try_unwrap(stats)
        .map(|mutex| mutex.into_inner())
        .unwrap_or_else(|arc| arc.lock().clone())
}

/// Memory-mapped copy for very large files (>100MB)
#[cfg(unix)]
pub fn mmap_copy_file(src: &Path, dst: &Path) -> Result<u64> {
    use std::os::unix::io::AsRawFd;

    let src_file = File::open(src)?;
    let file_size = src_file.metadata()?.len();

    // Create parent directory
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }

    let dst_file = File::create(dst)?;
    dst_file.set_len(file_size)?; // Pre-allocate space

    // For very large files, use copy_file_range or sendfile on Linux
    #[cfg(target_os = "linux")]
    {
        let src_fd = src_file.as_raw_fd();
        let dst_fd = dst_file.as_raw_fd();

        // Try copy_file_range first (Linux 4.5+, most efficient)
        let result = unsafe {
            libc::copy_file_range(
                src_fd,
                std::ptr::null_mut(),
                dst_fd,
                std::ptr::null_mut(),
                file_size as usize,
                0,
            )
        };

        if result > 0 {
            return Ok(result as u64);
        }

        // Fall back to sendfile (older Linux)
        let result =
            unsafe { libc::sendfile(dst_fd, src_fd, std::ptr::null_mut(), file_size as usize) };

        if result > 0 {
            return Ok(result as u64);
        }
    }

    // Fall back to regular copy if system calls fail
    std::fs::copy(src, dst).context("Memory-mapped copy fallback failed")
}

#[cfg(not(unix))]
pub fn mmap_copy_file(src: &Path, dst: &Path) -> Result<u64> {
    // Fall back to regular copy on non-Unix systems
    std::fs::copy(src, dst).context("Copy failed")
}

/// Chunked copy for large files (>10MB) with progress
pub fn chunked_copy_file(
    src: &Path,
    dst: &Path,
    buffer_sizer: &BufferSizer,
    is_network: bool,
    progress: Option<&indicatif::ProgressBar>,
    logger: &dyn Logger,
) -> Result<u64> {
    logger.start(src, dst);

    let result: Result<u64> = (|| {
        let metadata = fs::metadata(src)?;
        let file_size = metadata.len();

        // For very large files, use 16MB chunks
        let chunk_size = if file_size > 1_073_741_824 {
            // > 1GB
            16 * 1024 * 1024
        } else {
            buffer_sizer.calculate_buffer_size(file_size, is_network)
        };

        // Create parent directory
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut reader = File::open(src)?;
        let mut writer = File::create(dst)?;
        let mut buffer = vec![0u8; chunk_size];
        let mut total_bytes = 0u64;

        loop {
            let bytes_read = reader.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }

            writer.write_all(&buffer[..bytes_read])?;
            total_bytes += bytes_read as u64;

            if let Some(pb) = progress {
                pb.set_position(total_bytes);
            }
        }

        #[cfg(windows)]
        copy_windows_metadata(src, dst)?;

        Ok(total_bytes)
    })();

    match result {
        Ok(bytes) => {
            logger.copy_done(src, dst, bytes);
            Ok(bytes)
        }
        Err(e) => {
            logger.error("chunked_copy", src, &e.to_string());
            Err(e)
        }
    }
}

/// Direct system copy for local-to-local transfers on Windows
#[cfg(windows)]
pub fn windows_copyfile(src: &Path, dst: &Path) -> Result<u64> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::Storage::FileSystem::CopyFileExW;

    // Ensure destination directory exists
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let to_wide = |s: &OsStr| -> Vec<u16> { s.encode_wide().chain(std::iter::once(0)).collect() };
    let src_w = to_wide(src.as_os_str());
    let dst_w = to_wide(dst.as_os_str());
    let ok = unsafe { CopyFileExW(src_w.as_ptr(), dst_w.as_ptr(), None, None, None, 0).as_bool() };
    if ok {
        let bytes = std::fs::metadata(dst)?.len();
        Ok(bytes)
    } else {
        // Fall back to Rust copy if API not available/failed
        std::fs::copy(src, dst).context("Failed to copy file via CopyFileExW (fallback)")
    }
}

#[cfg(not(windows))]
pub fn windows_copyfile(src: &Path, dst: &Path) -> Result<u64> {
    fs::copy(src, dst).context("Failed to copy file")
}
