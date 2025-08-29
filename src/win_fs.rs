use normpath::PathExt;
use std::fs;
use std::path::{Path, PathBuf};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{CloseHandle, HANDLE, LUID},
        Security::{
            PrivilegeCheck, LUID_AND_ATTRIBUTES, PRIVILEGE_SET, SE_PRIVILEGE_ENABLED, TOKEN_QUERY,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

/// Normalizes a Windows path, resolving `.` and `..` components.
///
/// This function uses the `normpath` crate to provide a robust normalization
/// that works even for paths that do not exist on the filesystem.
///
/// # Arguments
///
/// * `path` - The path to normalize.
///
/// # Returns
///
/// A `PathBuf` containing the normalized path.
pub fn normalize_path(path: &Path) -> PathBuf {
    // The `normalize` method on the `PathExt` trait will handle the
    // normalization of the path, including handling `.` and `..`.
    match path.normalize() {
        Ok(normalized_path) => normalized_path.into_path_buf(),
        Err(_) => path.to_path_buf(), // Fallback to original path on error
    }
}

fn to_wide(path: &Path) -> Vec<u16> {
    // Use lossy conversion to avoid panics on non-UTF8 paths
    path.as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

pub fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    if !has_symlink_privilege() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Missing symlink privilege",
        ));
    }

    let target_wide = to_wide(target);
    let link_wide = to_wide(link);

    unsafe {
        use windows::Win32::Storage::FileSystem::{
            CreateSymbolicLinkW, SYMBOLIC_LINK_FLAGS, SYMBOLIC_LINK_FLAG_DIRECTORY,
        };
        let flags: SYMBOLIC_LINK_FLAGS = if target.is_dir() {
            SYMBOLIC_LINK_FLAG_DIRECTORY
        } else {
            SYMBOLIC_LINK_FLAGS(0)
        };
        let result = CreateSymbolicLinkW(
            PCWSTR(link_wide.as_ptr()),
            PCWSTR(target_wide.as_ptr()),
            flags,
        );
        if result.as_bool() {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

/// Compares two relative paths case-insensitively, which is important on Windows.
///
/// # Arguments
///
/// * `path1` - The first path to compare.
/// * `path2` - The second path to compare.
///
/// # Returns
///
/// `true` if the paths are equal, `false` otherwise.
pub fn compare_paths_case_insensitive(path1: &Path, path2: &Path) -> bool {
    let a = path1.as_os_str().to_string_lossy().to_ascii_lowercase();
    let b = path2.as_os_str().to_string_lossy().to_ascii_lowercase();
    a == b
}

/// Checks if the current process has the privilege to create symbolic links.
///
/// On Windows, creating symbolic links requires the `SeCreateSymbolicLinkPrivilege`.
/// This function checks if this privilege is enabled for the current process token.
///
/// # Returns
///
/// `true` if the privilege is held, `false` otherwise.
pub fn has_symlink_privilege() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }

        let privilege_name: PCWSTR = windows::core::w!("SeCreateSymbolicLinkPrivilege");
        let mut luid = LUID::default();
        if windows::Win32::Security::LookupPrivilegeValueW(None, privilege_name, &mut luid).is_err()
        {
            let _ = CloseHandle(token);
            return false;
        }

        let mut privilege_set = PRIVILEGE_SET {
            PrivilegeCount: 1,
            Control: 0,
            Privilege: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };

        use windows::Win32::Foundation::BOOL;
        let mut has_privilege = BOOL(0);
        let ok = PrivilegeCheck(token, &mut privilege_set, &mut has_privilege).is_ok();

        let _ = CloseHandle(token);

        ok && has_privilege.as_bool()
    }
}

/// Recursively clears the read-only attribute from a path and all its contents.
///
/// This is essential for Windows mirror deletions where files may have the
/// read-only attribute set, preventing normal deletion operations.
///
/// # Arguments
///
/// * `path` - The path to clear read-only attributes from
///
/// # Note
///
/// This function will silently continue on errors to ensure best-effort clearing.
/// Optimized to only call metadata once per file.
pub fn clear_readonly_recursive(path: &Path) {
    if let Ok(metadata) = fs::metadata(path) {
        // Clear read-only on the current path if needed
        if metadata.permissions().readonly() {
            let mut perms = metadata.permissions();
            perms.set_readonly(false);
            let _ = fs::set_permissions(path, perms);
        }

        // Recursively process directories
        if metadata.is_dir() {
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    clear_readonly_recursive(&entry.path());
                }
            }
        }
    }
}
