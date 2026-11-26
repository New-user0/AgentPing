//! Small filesystem helpers: atomic writes and permission hardening.

use std::io::Write;
use std::path::Path;

use crate::error::CoreError;

/// Atomically replaces `path` with `contents`.
///
/// The data is written to a temporary file in the same directory, flushed to disk and then
/// renamed over the destination, so readers never observe a partially written file.
/// When `private` is true the file is restricted to the current user (Unix `0600`).
pub fn atomic_write(path: &Path, contents: &[u8], private: bool) -> Result<(), CoreError> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| CoreError::io(dir, e))?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned());
    let tmp = dir.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        rand::random::<u32>()
    ));
    let result = (|| {
        let mut file = std::fs::File::create(&tmp).map_err(|e| CoreError::io(&tmp, e))?;
        if private {
            restrict_file_permissions(&tmp)?;
        }
        file.write_all(contents)
            .map_err(|e| CoreError::io(&tmp, e))?;
        file.sync_all().map_err(|e| CoreError::io(&tmp, e))?;
        drop(file);
        std::fs::rename(&tmp, path).map_err(|e| CoreError::io(path, e))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Restricts a file to the current user (Unix `0600`).
///
/// On Windows, files under the user profile already inherit a per-user ACL, so this is a no-op.
pub fn restrict_file_permissions(path: &Path) -> Result<(), CoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| CoreError::io(path, e))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Restricts a directory to the current user (Unix `0700`).
pub fn restrict_dir_permissions(path: &Path) -> Result<(), CoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| CoreError::io(path, e))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Returns true when a file is readable by users other than its owner (Unix only).
pub fn is_world_or_group_accessible(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            return meta.permissions().mode() & 0o077 != 0;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("f.txt");
        atomic_write(&path, b"one", true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one");
        atomic_write(&path, b"two", true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        assert!(!is_world_or_group_accessible(&path));
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
    }
}
