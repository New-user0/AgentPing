//! Platform-aware filesystem locations.

use std::path::{Path, PathBuf};

use crate::error::CoreError;

/// Environment variable that relocates every AgentPing directory under one root.
///
/// Useful for tests, portable installs and running multiple isolated instances.
pub const HOME_ENV: &str = "AGENTPING_HOME";

/// All filesystem locations used by AgentPing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Directory containing `config.toml`.
    pub config_dir: PathBuf,
    /// Directory containing the database, daemon token and other state.
    pub data_dir: PathBuf,
    /// Directory containing log files.
    pub log_dir: PathBuf,
}

impl Paths {
    /// Resolves paths from `AGENTPING_HOME` or the platform conventions.
    ///
    /// * Linux: `~/.config/agentping`, `~/.local/share/agentping`
    /// * macOS: `~/Library/Application Support/agentping`
    /// * Windows: `%APPDATA%\agentping\config`, `%LOCALAPPDATA%\agentping\data`
    pub fn discover() -> Result<Self, CoreError> {
        if let Some(home) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            return Ok(Self::under(PathBuf::from(home)));
        }
        let dirs = directories::ProjectDirs::from("", "", "agentping")
            .ok_or(CoreError::NoHomeDirectory)?;
        let data_dir = dirs.data_local_dir().to_path_buf();
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            log_dir: data_dir.join("logs"),
            data_dir,
        })
    }

    /// Places every directory under a single root.
    pub fn under(root: PathBuf) -> Self {
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            log_dir: root.join("logs"),
        }
    }

    /// Creates all directories (restricted to the current user on Unix).
    pub fn ensure_dirs(&self) -> Result<(), CoreError> {
        for dir in [&self.config_dir, &self.data_dir, &self.log_dir] {
            std::fs::create_dir_all(dir).map_err(|e| CoreError::io(dir, e))?;
            crate::fsutil::restrict_dir_permissions(dir)?;
        }
        Ok(())
    }

    /// Path of the TOML configuration file.
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// Default path of the SQLite database.
    pub fn default_database(&self) -> PathBuf {
        self.data_dir.join("agentping.db")
    }

    /// Path of the daemon's local API token.
    pub fn token_file(&self) -> PathBuf {
        self.data_dir.join("daemon.token")
    }

    /// Path of the daemon pid/port state file.
    pub fn daemon_state_file(&self) -> PathBuf {
        self.data_dir.join("daemon.json")
    }

    /// Path of the plaintext secrets fallback (only used after explicit opt-in).
    pub fn secrets_file(&self) -> PathBuf {
        self.config_dir.join("secrets.toml")
    }
}

/// Expands a leading `~` to the user's home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        if let Some(base) = directories::BaseDirs::new() {
            return base.home_dir().join(rest);
        }
    } else if path == "~" {
        if let Some(base) = directories::BaseDirs::new() {
            return base.home_dir().to_path_buf();
        }
    }
    PathBuf::from(path)
}

/// Returns the user's home directory, if it can be determined.
pub fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
}

/// Shortens a path for display by replacing the home directory with `~`.
pub fn display_path(path: &Path) -> String {
    if let Some(home) = home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display());
        }
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_places_everything_below_root() {
        let p = Paths::under(PathBuf::from("/tmp/ap"));
        assert_eq!(p.config_file(), PathBuf::from("/tmp/ap/config/config.toml"));
        assert_eq!(
            p.default_database(),
            PathBuf::from("/tmp/ap/data/agentping.db")
        );
        assert!(p.log_dir.starts_with("/tmp/ap"));
    }

    #[test]
    fn expand_tilde_leaves_plain_paths_alone() {
        assert_eq!(expand_tilde("/var/x.db"), PathBuf::from("/var/x.db"));
        if let Some(home) = home_dir() {
            assert_eq!(expand_tilde("~/x.db"), home.join("x.db"));
        }
    }
}
