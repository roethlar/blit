//! Simplified Windows-specific fast file enumeration
//! Adapted from windows_fast_enum.rs for rev3

#[cfg(windows)]
use std::ffi::OsStr;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::mem;
use anyhow::Result;

#[cfg(windows)]
use winapi::um::fileapi::{FindFirstFileExW, FindNextFileW, FindClose};
#[cfg(windows)]
use winapi::um::minwinbase::{FindExInfoBasic, FindExSearchNameMatch, WIN32_FIND_DATAW};
#[cfg(windows)]
use winapi::um::handleapi::INVALID_HANDLE_VALUE;
#[cfg(windows)]
use winapi::um::winnt::FILE_ATTRIBUTE_DIRECTORY;
#[cfg(windows)]
use winapi::shared::winerror::ERROR_NO_MORE_FILES;

// Optimization flag for NTFS large fetch
#[cfg(windows)]
const FIND_FIRST_EX_LARGE_FETCH: u32 = 0x00000002;

/// Entry with size information for categorization
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
    pub is_directory: bool,
}

/// File filter options (robocopy compatibility)
pub struct FileFilter {
    pub exclude_files: Vec<String>,
    pub exclude_dirs: Vec<String>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub include_empty_dirs: bool,
}

impl Default for FileFilter {
    fn default() -> Self {
        Self {
            exclude_files: Vec::new(),
            exclude_dirs: Vec::new(),
            min_size: None,
            max_size: None,
            include_empty_dirs: true, // Default to /E behavior
        }
    }
}

impl FileFilter {
    /// Check if a file should be included
    fn should_include_file(&self, path: &Path, size: u64) -> bool {
        // Check file patterns
        let filename = path.file_name().unwrap_or_default().to_string_lossy();
        for pattern in &self.exclude_files {
            if glob_match(pattern, &filename) {
                return false;
            }
        }
        
        // Check size limits
        if let Some(min) = self.min_size {
            if size < min {
                return false;
            }
        }
        if let Some(max) = self.max_size {
            if size > max {
                return false;
            }
        }
        
        true
    }
    
    /// Check if a directory should be included
    fn should_include_dir(&self, path: &Path) -> bool {
        let dirname = path.file_name().unwrap_or_default().to_string_lossy();
        for pattern in &self.exclude_dirs {
            if glob_match(pattern, &dirname) {
                return false;
            }
        }
        true
    }
}

/// Simple glob matching (supports * wildcards)
fn glob_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    
    // Simple wildcard matching
    if pattern.contains('*') {
        if pattern.starts_with('*') && pattern.ends_with('*') {
            let middle = &pattern[1..pattern.len()-1];
            return text.contains(middle);
        } else if let Some(suffix) = pattern.strip_prefix('*') {
            return text.ends_with(suffix);
        } else if let Some(prefix) = pattern.strip_suffix('*') {
            return text.starts_with(prefix);
        }
    }
    
    // Exact match
    pattern == text
}

/// Fast directory enumeration with filtering (robocopy-style) - Windows version
#[cfg(windows)]
pub fn enumerate_directory_filtered(root: &Path, filter: &FileFilter) -> Result<Vec<FileEntry>> {
    let mut entries = Vec::with_capacity(10000);
    let mut dirs_to_process = vec![root.to_path_buf()];
    
    while let Some(dir) = dirs_to_process.pop() {
        // Check if directory should be included
        if dir != root && !filter.should_include_dir(&dir) {
            continue;
        }
        
        match scan_single_directory(&dir) {
            Ok((files, subdirs)) => {
                // Filter files
                let filtered_files: Vec<FileEntry> = files.into_iter()
                    .filter(|entry| filter.should_include_file(&entry.path, entry.size))
                    .collect();
                entries.extend(filtered_files);
                dirs_to_process.extend(subdirs);
            }
            Err(_) => continue, // Skip inaccessible directories
        }
    }
    
    Ok(entries)
}

/// Backward compatibility - enumerate directory without filtering
#[cfg(windows)]
pub fn enumerate_directory(root: &Path) -> Result<Vec<FileEntry>> {
    enumerate_directory_filtered(root, &FileFilter::default())
}

#[cfg(windows)]
fn scan_single_directory(dir: &Path) -> Result<(Vec<FileEntry>, Vec<PathBuf>)> {
    let search_path = dir.join("*");
    let search_path_wide: Vec<u16> = OsStr::new(&search_path)
        .encode_wide()
        .chain(Some(0))
        .collect();
    
    let mut find_data: WIN32_FIND_DATAW = unsafe { mem::zeroed() };
    let mut files = Vec::with_capacity(1000);
    let mut dirs = Vec::with_capacity(100);
    
    // Use optimized enumeration with large fetch buffer
    let handle = unsafe {
        FindFirstFileExW(
            search_path_wide.as_ptr(),
            FindExInfoBasic,
            &mut find_data as *mut _ as *mut _,
            FindExSearchNameMatch,
            std::ptr::null_mut(),
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    
    if handle == INVALID_HANDLE_VALUE {
        return Ok((files, dirs));
    }
    
    loop {
        // Convert wide string to PathBuf
        let file_name_wide = &find_data.cFileName;
        let len = file_name_wide.iter().position(|&c| c == 0).unwrap_or(260);
        let file_name = String::from_utf16_lossy(&file_name_wide[..len]);
        
        // Skip . and ..
        if file_name != "." && file_name != ".." {
            let full_path = dir.join(&file_name);
            
            if find_data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
                dirs.push(full_path);
            } else {
                // Calculate file size from WIN32_FIND_DATAW
                let size = ((find_data.nFileSizeHigh as u64) << 32) | (find_data.nFileSizeLow as u64);
                files.push(FileEntry {
                    path: full_path,
                    size,
                    is_directory: false,
                });
            }
        }
        
        // Get next file
        if unsafe { FindNextFileW(handle, &mut find_data) } == 0 {
            let error = unsafe { winapi::um::errhandlingapi::GetLastError() };
            if error != ERROR_NO_MORE_FILES {
                unsafe { FindClose(handle) };
                return Err(anyhow::anyhow!("FindNextFileW failed: {}", error));
            }
            break;
        }
    }
    
    unsafe { FindClose(handle) };
    Ok((files, dirs))
}

/// Fast directory enumeration with filtering for non-Windows platforms
#[cfg(not(windows))]
pub fn enumerate_directory_filtered(root: &Path, filter: &FileFilter) -> Result<Vec<FileEntry>> {
    use walkdir::WalkDir;
    
    let mut entries = Vec::new();
    
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        let path = entry.path();
        
        // Check directory filtering
        if entry.file_type().is_dir() {
            if !filter.should_include_dir(path) {
                continue;
            }
        }
        
        if entry.file_type().is_file() {
            let metadata = entry.metadata()?;
            let size = metadata.len();
            
            // Apply file filtering
            if filter.should_include_file(path, size) {
                entries.push(FileEntry {
                    path: path.to_path_buf(),
                    size,
                    is_directory: false,
                });
            }
        }
    }
    
    Ok(entries)
}

/// Backward compatibility - enumerate directory without filtering
#[cfg(not(windows))]
pub fn enumerate_directory(root: &Path) -> Result<Vec<FileEntry>> {
    enumerate_directory_filtered(root, &FileFilter::default())
}

/// Categorize files by size for optimal copy strategy
pub fn categorize_files(entries: Vec<FileEntry>) -> (Vec<FileEntry>, Vec<FileEntry>, Vec<FileEntry>) {
    let mut small = Vec::new();   // < 1MB - tar streaming candidates
    let mut medium = Vec::new();  // 1-100MB - parallel copy
    let mut large = Vec::new();   // > 100MB - chunked copy
    
    for entry in entries {
        if entry.size < 1_048_576 {
            small.push(entry);
        } else if entry.size < 104_857_600 {
            medium.push(entry);
        } else {
            large.push(entry);
        }
    }
    
    (small, medium, large)
}