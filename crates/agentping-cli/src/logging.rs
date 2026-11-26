//! Structured logging to daily-rotated files (and optionally stderr).

use agentping_core::Paths;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter, Layer};

/// Log file name prefix (`agentping.YYYY-MM-DD.log`).
pub const LOG_PREFIX: &str = "agentping";

const CRATES: [&str; 7] = [
    "agentping",
    "agentping_core",
    "agentping_storage",
    "agentping_rules",
    "agentping_notifiers",
    "agentping_agents",
    "agentping_daemon",
];

fn filter(level: &str) -> EnvFilter {
    let mut directives = String::from("warn");
    for c in CRATES {
        directives.push_str(&format!(",{c}={level}"));
    }
    EnvFilter::try_new(directives).unwrap_or_else(|_| EnvFilter::new("warn"))
}

/// Where logs should go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// File only (hooks must never write to stdout/stderr).
    FileOnly,
    /// File, plus stderr at `stderr_level` when set.
    WithStderr(Option<&'static str>),
}

/// Installs the global subscriber. Failures to open the log file are non-fatal.
pub fn init(paths: &Paths, level: &str, mode: Mode) {
    let file_layer = std::fs::create_dir_all(&paths.log_dir).ok().and_then(|_| {
        RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix(LOG_PREFIX)
            .filename_suffix("log")
            .max_log_files(14)
            .build(&paths.log_dir)
            .ok()
    });
    let file_layer = file_layer.map(|appender| {
        fmt::layer()
            .with_ansi(false)
            .with_target(false)
            .with_writer(appender)
            .with_filter(filter(level))
    });
    let stderr_layer = match mode {
        Mode::WithStderr(Some(l)) => Some(
            fmt::layer()
                .with_target(false)
                .with_writer(std::io::stderr)
                .with_filter(filter(l)),
        ),
        _ => None,
    };
    let _ = tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer)
        .try_init();
}

/// Log files, newest last.
pub fn log_files(paths: &Paths) -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(&paths.log_dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with(LOG_PREFIX))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    files
}
