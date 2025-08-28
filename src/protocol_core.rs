//! Shared protocol logic for both sync and async network implementations
//! 
//! This module provides protocol-agnostic functions that can be used
//! by both net.rs (sync) and net_async.rs (async) to reduce code duplication.

use anyhow::{anyhow, bail, Result};
#[cfg(windows)]
use crate::win_fs;
use std::path::{Path, PathBuf, Component};

/// Normalize a path to be safely under a root directory.
/// This prevents path traversal attacks by:
/// 1. Rejecting absolute paths, parent directory components, and root/prefix components
/// 2. Rejecting NUL bytes in path
/// 3. On Windows, rejecting ':' in path components (ADS defense)
/// 4. Canonicalizing the final path to resolve symlinks
/// 5. Ensuring the result is under the root
pub fn normalize_under_root(root: &Path, p: &Path) -> Result<PathBuf> {
    use Component::{CurDir, Normal, ParentDir, Prefix, RootDir};
    
    // Reject paths containing NUL
    let path_str = p.to_string_lossy();
    if path_str.contains('\0') {
        bail!("path contains NUL byte");
    }
    
    // Build safe relative path
    let mut safe = PathBuf::new();
    for component in p.components() {
        match component {
            CurDir => {} // Skip "."
            Normal(s) => {
                // On Windows, reject components with ':' (ADS defense)
                #[cfg(windows)]
                if s.to_string_lossy().contains(':') {
                    bail!("path component contains colon (potential ADS attack)");
                }
                safe.push(s);
            }
            ParentDir | RootDir | Prefix(_) => {
                bail!("path contains disallowed component: {:?}", component);
            }
        }
    }
    
    // Join with root
    let joined = root.join(&safe);
    
    // For existing paths, canonicalize to resolve symlinks
    // For new files, canonicalize parent then append filename
    let final_path = if joined.exists() {
        joined.canonicalize()
            .map_err(|e| anyhow!("failed to canonicalize {:?}: {}", joined, e))?
    } else if let Some(parent) = joined.parent() {
        if parent.exists() {
            let canonical_parent = parent.canonicalize()
                .map_err(|e| anyhow!("failed to canonicalize parent {:?}: {}", parent, e))?;
            if let Some(filename) = joined.file_name() {
                canonical_parent.join(filename)
            } else {
                canonical_parent
            }
        } else {
            joined
        }
    } else {
        joined
    };
    
    // Ensure final path is under root
    if !final_path.starts_with(root) {
        bail!("path {:?} escapes root {:?}", p, root);
    }
    
    Ok(final_path)
}

/// Frame validation constants
pub const MIN_FRAME_SIZE: usize = 0;

/// Validate frame payload size using protocol::MAX_FRAME_SIZE directly
pub fn validate_frame_size(size: usize) -> Result<()> {
    if size > crate::protocol::MAX_FRAME_SIZE {
        bail!("frame payload too large: {} bytes (max: {})", size, crate::protocol::MAX_FRAME_SIZE);
    }
    Ok(())
}

/// Build frame header (11 bytes)
/// Format: MAGIC (4) | VERSION (2) | TYPE (1) | LENGTH (4)
pub fn build_frame_header(frame_type: u8, payload_len: u32) -> [u8; 11] {
    use crate::protocol::{MAGIC, VERSION};
    
    let mut header = [0u8; 11];
    header[0..4].copy_from_slice(MAGIC);
    header[4..6].copy_from_slice(&VERSION.to_le_bytes());
    header[6] = frame_type;
    header[7..11].copy_from_slice(&payload_len.to_le_bytes());
    header
}

/// Parse frame header
/// Returns: (frame_type, payload_length)
pub fn parse_frame_header(header: &[u8; 11]) -> Result<(u8, u32)> {
    use crate::protocol::{MAGIC, VERSION};
    
    // Verify magic
    if &header[0..4] != MAGIC {
        bail!("invalid magic in frame header");
    }
    
    // Check version
    let version = u16::from_le_bytes([header[4], header[5]]);
    if version != VERSION {
        bail!("protocol version mismatch: got {}, expected {}", version, VERSION);
    }
    
    // Extract type and length
    let frame_type = header[6];
    let payload_len = u32::from_le_bytes([header[7], header[8], header[9], header[10]]);
    
    Ok((frame_type, payload_len))
}

/// Helper for Windows: recursively clear read-only attribute
/// Delegates to the canonical implementation in win_fs module
#[cfg(windows)]
pub fn clear_readonly_recursive(path: &Path) {
    win_fs::clear_readonly_recursive(path);
}

/// Create directory with parent creation
pub fn ensure_dir_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        std::fs::create_dir_all(path)?;
    }
    Ok(())
}

/// Create parent directory if needed
pub fn ensure_parent_exists(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir_exists(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_normalize_under_root_safe_paths() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        
        // Safe relative path
        let result = normalize_under_root(root, Path::new("subdir/file.txt")).unwrap();
        assert!(result.starts_with(root));
        assert!(result.ends_with("subdir/file.txt"));
        
        // Path with current directory
        let result = normalize_under_root(root, Path::new("./subdir/./file.txt")).unwrap();
        assert!(result.starts_with(root));
        assert!(result.ends_with("subdir/file.txt"));
    }
    
    #[test]
    fn test_normalize_under_root_unsafe_paths() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        
        // Parent directory traversal
        assert!(normalize_under_root(root, Path::new("../etc/passwd")).is_err());
        assert!(normalize_under_root(root, Path::new("subdir/../../etc/passwd")).is_err());
        
        // Absolute path
        assert!(normalize_under_root(root, Path::new("/etc/passwd")).is_err());
        
        // NUL byte
        assert!(normalize_under_root(root, Path::new("file\0.txt")).is_err());
    }
    
    #[cfg(windows)]
    #[test]
    fn test_normalize_under_root_windows_ads() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        
        // ADS attempt
        assert!(normalize_under_root(root, Path::new("file.txt:stream")).is_err());
        assert!(normalize_under_root(root, Path::new("dir:$DATA")).is_err());
    }
    
    #[test]
    fn test_frame_header_round_trip() {
        let frame_type = 42u8;
        let payload_len = 12345u32;
        
        let header = build_frame_header(frame_type, payload_len);
        let (parsed_type, parsed_len) = parse_frame_header(&header).unwrap();
        
        assert_eq!(parsed_type, frame_type);
        assert_eq!(parsed_len, payload_len);
    }
    
    #[test]
    fn test_validate_frame_size() {
        assert!(validate_frame_size(1024).is_ok());
        assert!(validate_frame_size(crate::protocol::MAX_FRAME_SIZE).is_ok());
        assert!(validate_frame_size(crate::protocol::MAX_FRAME_SIZE + 1).is_err());
    }
    
    #[test]
    fn test_normalize_with_symlinks() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        
        // Create a directory and file for testing
        let subdir = root.join("subdir");
        fs::create_dir(&subdir).unwrap();
        let file = subdir.join("file.txt");
        fs::write(&file, "test").unwrap();
        
        // Test that existing files are canonicalized
        let result = normalize_under_root(root, Path::new("subdir/file.txt")).unwrap();
        assert_eq!(result, file.canonicalize().unwrap());
    }
    
    #[test]
    fn test_normalize_non_existent_file() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        
        // Create parent directory
        let subdir = root.join("subdir");
        fs::create_dir(&subdir).unwrap();
        
        // Non-existent file should still work (for new file creation)
        let result = normalize_under_root(root, Path::new("subdir/newfile.txt")).unwrap();
        assert!(result.starts_with(root));
        assert!(result.ends_with("subdir/newfile.txt"));
    }
    
    #[test]
    fn test_parse_frame_header_invalid_magic() {
        let mut header = [0u8; 11];
        header[0..4].copy_from_slice(b"WRNG"); // Wrong magic
        header[4..6].copy_from_slice(&1u16.to_le_bytes());
        header[6] = 1;
        header[7..11].copy_from_slice(&100u32.to_le_bytes());
        
        assert!(parse_frame_header(&header).is_err());
    }
    
    #[test]
    fn test_parse_frame_header_wrong_version() {
        use crate::protocol::MAGIC;
        
        let mut header = [0u8; 11];
        header[0..4].copy_from_slice(MAGIC);
        header[4..6].copy_from_slice(&999u16.to_le_bytes()); // Wrong version
        header[6] = 1;
        header[7..11].copy_from_slice(&100u32.to_le_bytes());
        
        assert!(parse_frame_header(&header).is_err());
    }
    
    #[test]
    fn test_build_frame_header_all_frame_types() {
        use crate::protocol::frame;
        
        // Test various frame types
        let frame_types = vec![
            frame::START,
            frame::OK,
            frame::ERROR,
            frame::FILE_START,
            frame::FILE_DATA,
            frame::FILE_END,
            frame::TAR_START,
            frame::VERIFY_REQ,
            frame::LIST_REQ,
        ];
        
        for frame_type in frame_types {
            let header = build_frame_header(frame_type, 1000);
            let (parsed_type, parsed_len) = parse_frame_header(&header).unwrap();
            assert_eq!(parsed_type, frame_type);
            assert_eq!(parsed_len, 1000);
        }
    }
    
    #[test]
    fn test_ensure_dir_exists() {
        let temp_dir = TempDir::new().unwrap();
        let new_dir = temp_dir.path().join("new").join("nested").join("dir");
        
        assert!(!new_dir.exists());
        ensure_dir_exists(&new_dir).unwrap();
        assert!(new_dir.exists());
        assert!(new_dir.is_dir());
        
        // Should be idempotent
        ensure_dir_exists(&new_dir).unwrap();
        assert!(new_dir.exists());
    }
    
    #[test]
    fn test_ensure_parent_exists() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("new").join("nested").join("file.txt");
        
        assert!(!file_path.parent().unwrap().exists());
        ensure_parent_exists(&file_path).unwrap();
        assert!(file_path.parent().unwrap().exists());
        assert!(file_path.parent().unwrap().is_dir());
    }
    
    #[test]
    fn test_normalize_complex_paths() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        
        // Test multiple current directory markers
        let result = normalize_under_root(root, Path::new("././subdir/./file.txt")).unwrap();
        assert!(result.ends_with("subdir/file.txt"));
        
        // Test empty path components (consecutive slashes)
        let result = normalize_under_root(root, Path::new("subdir//file.txt")).unwrap();
        assert!(result.ends_with("subdir/file.txt"));
        
        // Test mixed separators on Windows
        #[cfg(windows)]
        {
            let result = normalize_under_root(root, Path::new("subdir\\file.txt")).unwrap();
            assert!(result.ends_with("file.txt"));
        }
    }
    
    #[test]
    fn test_validate_frame_size_edge_cases() {
        assert!(validate_frame_size(0).is_ok()); // Empty payload is valid
        assert!(validate_frame_size(1).is_ok()); // Minimum non-empty
        assert!(validate_frame_size(crate::protocol::MAX_FRAME_SIZE - 1).is_ok());
        assert!(validate_frame_size(usize::MAX).is_err()); // Overflow case
    }
    
    #[cfg(windows)]
    #[test]
    fn test_clear_readonly_recursive() {
        use std::os::windows::fs::MetadataExt;
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("readonly.txt");
        
        // Create a file and make it readonly
        fs::write(&test_file, "test").unwrap();
        let mut perms = fs::metadata(&test_file).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&test_file, perms).unwrap();
        
        // Verify it's readonly
        assert!(fs::metadata(&test_file).unwrap().permissions().readonly());
        
        // Clear readonly
        clear_readonly_recursive(temp_dir.path());
        
        // Verify it's no longer readonly
        assert!(!fs::metadata(&test_file).unwrap().permissions().readonly());
    }
}