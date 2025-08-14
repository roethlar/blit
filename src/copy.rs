//! Optimized copy operations for Windows
//! Focus on 10GbE saturation with minimal overhead

use std::fs::{self, File};
use std::io::{Read, Write, BufReader, BufWriter};
use std::path::{Path, PathBuf};
use anyhow::{Result, Context};
use rayon::prelude::*;
use parking_lot::Mutex;
use std::sync::Arc;
use crate::progress::CargoProgress;

use crate::buffer::BufferSizer;
use crate::windows_enum::FileEntry;

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
pub fn copy_file(src: &Path, dst: &Path, buffer_sizer: &BufferSizer, is_network: bool) -> Result<u64> {
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
    
    // Copy metadata on Windows
    #[cfg(windows)]
    {
        copy_windows_metadata(src, dst)?;
    }
    
    Ok(total_bytes)
}

/// Copy Windows-specific metadata (attributes, timestamps)
#[cfg(windows)]
fn copy_windows_metadata(src: &Path, dst: &Path) -> Result<()> {
    use std::os::windows::fs::MetadataExt;
    use winapi::um::fileapi::SetFileAttributesW;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    
    let metadata = fs::metadata(src)?;
    
    // Set file attributes
    let dst_wide: Vec<u16> = OsStr::new(dst)
        .encode_wide()
        .chain(Some(0))
        .collect();
    
    unsafe {
        SetFileAttributesW(dst_wide.as_ptr(), metadata.file_attributes());
    }
    
    Ok(())
}

#[cfg(not(windows))]
fn copy_windows_metadata(_src: &Path, _dst: &Path) -> Result<()> {
    Ok(())
}

/// Parallel copy for medium-sized files (1-100MB)
pub fn parallel_copy_files(
    files: Vec<(FileEntry, PathBuf)>,
    buffer_sizer: Arc<BufferSizer>,
    is_network: bool,
    progress_display: &Option<CargoProgress>,
) -> CopyStats {
    let stats = Arc::new(Mutex::new(CopyStats::default()));
    
    // Use rayon for parallel copying
    files.par_iter().enumerate().for_each(|(i, (entry, dst))| {
        // Show progress for verbose mode
        // Show progress for verbose mode
        if let Some(ref p) = progress_display {
            if files.len() < 20 || i % (files.len() / 10).max(1) == 0 {
                p.print_file_op("Copying", &entry.path.display().to_string());
            }
        }
        
        match copy_file(&entry.path, dst, &buffer_sizer, is_network) {
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

/// Chunked copy for large files (>100MB) with progress
pub fn chunked_copy_file(
    src: &Path,
    dst: &Path,
    buffer_sizer: &BufferSizer,
    is_network: bool,
    progress: Option<&indicatif::ProgressBar>,
) -> Result<u64> {
    let metadata = fs::metadata(src)?;
    let file_size = metadata.len();
    
    // For very large files, use 16MB chunks
    let chunk_size = if file_size > 1_073_741_824 { // > 1GB
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
}

/// Direct system copy for local-to-local transfers on Windows
#[cfg(windows)]
pub fn windows_copyfile(src: &Path, dst: &Path) -> Result<u64> {
    use winapi::um::winbase::CopyFileW;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    
    // Create parent directory
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    
    let src_wide: Vec<u16> = OsStr::new(src)
        .encode_wide()
        .chain(Some(0))
        .collect();
    
    let dst_wide: Vec<u16> = OsStr::new(dst)
        .encode_wide()
        .chain(Some(0))
        .collect();
    
    let result = unsafe {
        CopyFileW(src_wide.as_ptr(), dst_wide.as_ptr(), 0)
    };
    
    if result == 0 {
        // Fallback to regular copy
        return copy_file(src, dst, &BufferSizer::new(), false);
    }
    
    let metadata = fs::metadata(dst)?;
    Ok(metadata.len())
}

#[cfg(not(windows))]
pub fn windows_copyfile(src: &Path, dst: &Path) -> Result<u64> {
    fs::copy(src, dst).context("Failed to copy file")
}