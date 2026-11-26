//! Project identification from a working directory.

use std::path::{Path, PathBuf};

/// A detected project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInfo {
    /// Short display name (the project root's folder name).
    pub name: String,
    /// Project root directory.
    pub root: PathBuf,
}

const MAX_ASCENT: usize = 64;

/// Identifies the project containing `cwd`.
///
/// Walks up from `cwd` looking for a `.git` entry (directory or worktree file). If none is
/// found the working directory itself is treated as the project root. Only metadata is
/// checked; no file contents are read.
pub fn detect(cwd: &Path) -> Option<ProjectInfo> {
    let root = find_git_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let name = folder_name(&root)?;
    Some(ProjectInfo { name, root })
}

fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    for _ in 0..MAX_ASCENT {
        let dir = current?;
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

fn folder_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .or_else(|| {
            // Root paths such as `C:\` or `/` have no file name.
            let s = path.to_string_lossy();
            let trimmed = s.trim_end_matches(['/', '\\']);
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_git_root_from_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("backend-api");
        let sub = root.join("src").join("handlers");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let info = detect(&sub).unwrap();
        assert_eq!(info.name, "backend-api");
        assert_eq!(info.root, root);
    }

    #[test]
    fn falls_back_to_cwd_name_without_git() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("scratch-project");
        std::fs::create_dir_all(&cwd).unwrap();
        let info = detect(&cwd).unwrap();
        // The temp dir itself might live inside a git checkout; only assert when it doesn't.
        if info.root == cwd {
            assert_eq!(info.name, "scratch-project");
        }
    }

    #[test]
    fn handles_nonexistent_paths() {
        let info = detect(Path::new("/definitely/not/here/my-app")).unwrap();
        assert_eq!(info.name, "my-app");
    }
}
