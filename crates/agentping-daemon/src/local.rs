//! Local daemon credentials and state files.

use agentping_core::fsutil::atomic_write;
use agentping_core::Paths;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Reads the daemon API token, if one exists.
pub fn read_token(paths: &Paths) -> Option<String> {
    std::fs::read_to_string(paths.token_file())
        .ok()
        .map(|t| t.trim().to_owned())
        .filter(|t| t.len() >= 32)
}

/// Reads the daemon API token, creating a new random one if needed.
pub fn load_or_create_token(paths: &Paths) -> anyhow::Result<String> {
    if let Some(t) = read_token(paths) {
        return Ok(t);
    }
    let bytes: [u8; 32] = rand::random();
    let token = hex::encode(bytes);
    atomic_write(&paths.token_file(), token.as_bytes(), true)?;
    Ok(token)
}

/// Information the running daemon publishes about itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonState {
    /// Process id.
    pub pid: u32,
    /// Bound address.
    pub addr: String,
    /// Start time.
    pub started_at: DateTime<Utc>,
    /// AgentPing version.
    pub version: String,
}

/// Writes the daemon state file.
pub fn write_state(paths: &Paths, state: &DaemonState) -> anyhow::Result<()> {
    let json = serde_json::to_vec_pretty(state)?;
    atomic_write(&paths.daemon_state_file(), &json, true)?;
    Ok(())
}

/// Reads the daemon state file.
pub fn read_state(paths: &Paths) -> Option<DaemonState> {
    let text = std::fs::read_to_string(paths.daemon_state_file()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Removes the daemon state file.
pub fn clear_state(paths: &Paths) {
    let _ = std::fs::remove_file(paths.daemon_state_file());
}

/// Constant-time string comparison for tokens.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_created_once_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        assert!(read_token(&paths).is_none());
        let a = load_or_create_token(&paths).unwrap();
        let b = load_or_create_token(&paths).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(!agentping_core::fsutil::is_world_or_group_accessible(
            &paths.token_file()
        ));
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
    }
}
