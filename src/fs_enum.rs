use anyhow::Result;
use std::path::{Path, PathBuf};
// Filesystem enumeration and categorization (Unix focus)

/// Entry with size information for categorization
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
    pub is_directory: bool,
}

/// Copy job with optional resume offset
#[derive(Debug, Clone)]
pub struct CopyJob {
    pub entry: FileEntry,
    pub start_offset: u64,
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
        for pattern in &self.exclude_dirs {
            // Check if any path component matches the pattern (like rsync/robocopy)
            for component in path.components() {
                if let Some(component_str) = component.as_os_str().to_str() {
                    if glob_match(pattern, component_str) {
                        // Debug: uncomment to see what's being excluded
                        // eprintln!("DEBUG: Excluding {} (matched pattern '{}')", path.display(), pattern);
                        return false;
                    }
                }
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
            let middle = &pattern[1..pattern.len() - 1];
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

// All Windows-specific code removed.

/// Fast directory enumeration with filtering for non-Windows platforms
#[cfg(not(windows))]
pub fn enumerate_directory_filtered(root: &Path, filter: &FileFilter) -> Result<Vec<FileEntry>> {
    use walkdir::WalkDir;

    let mut entries = Vec::new();

    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Skip excluded directories entirely - this prevents walking into them
            if e.file_type().is_dir() {
                filter.should_include_dir(e.path())
            } else {
                true // Always walk files, filter them later
            }
        })
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        if entry.file_type().is_file() {
            if let Ok(metadata) = entry.metadata() {
                let size = metadata.len();
                // Apply file filtering
                if filter.should_include_file(path, size) {
                    entries.push(FileEntry {
                        path: path.to_path_buf(),
                        size,
                        is_directory: false,
                    });
                }
            } // else: skip unreadable entries
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
pub fn categorize_files(entries: Vec<CopyJob>) -> (Vec<CopyJob>, Vec<CopyJob>, Vec<CopyJob>) {
    let mut small = Vec::new(); // < 1MB - tar streaming candidates
    let mut medium = Vec::new(); // 1-100MB - parallel copy
    let mut large = Vec::new(); // > 100MB - chunked copy

    for job in entries {
        if job.entry.size < 1_048_576 {
            small.push(job);
        } else if job.entry.size < 104_857_600 {
            medium.push(job);
        } else {
            large.push(job);
        }
    }

    (small, medium, large)
}
