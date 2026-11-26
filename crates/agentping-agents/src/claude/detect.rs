//! Detects a local Claude Code installation.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use super::install::claude_config_dir;

/// What we know about the local Claude Code installation.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ClaudeDetection {
    /// Path of the `claude` executable, if found on PATH or in the default install location.
    pub binary: Option<PathBuf>,
    /// Output of `claude --version`, if it could be run.
    pub version: Option<String>,
    /// Claude Code configuration directory.
    pub config_dir: Option<PathBuf>,
    /// Whether the configuration directory exists.
    pub config_dir_exists: bool,
}

impl ClaudeDetection {
    /// Whether Claude Code appears to be installed.
    pub fn installed(&self) -> bool {
        self.binary.is_some() || self.config_dir_exists
    }
}

fn executable_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["claude.exe", "claude.cmd", "claude"]
    } else {
        &["claude"]
    }
}

fn find_binary() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = agentping_core::paths::home_dir() {
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join(".claude").join("local"));
    }
    dirs.iter()
        .flat_map(|d| executable_names().iter().map(move |n| d.join(n)))
        .find(|p| p.is_file())
}

fn run_version(binary: &std::path::Path, timeout: Duration) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    let binary = binary.to_path_buf();
    std::thread::spawn(move || {
        let out = Command::new(&binary)
            .arg("--version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        let _ = tx.send(out);
    });
    let out = rx.recv_timeout(timeout).ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// Detects Claude Code. When `with_version` is set, runs `claude --version` (up to 5s).
pub fn detect(with_version: bool) -> ClaudeDetection {
    let binary = find_binary();
    let version = if with_version {
        binary
            .as_ref()
            .and_then(|b| run_version(b, Duration::from_secs(5)))
    } else {
        None
    };
    let config_dir = claude_config_dir();
    let config_dir_exists = config_dir.as_ref().is_some_and(|d| d.is_dir());
    ClaudeDetection {
        binary,
        version,
        config_dir,
        config_dir_exists,
    }
}
