//! Installs and removes AgentPing's Claude Code hooks without disturbing other settings.
//!
//! Hook configuration format: https://code.claude.com/docs/en/hooks
//! (`hooks → <EventName> → [{ matcher?, hooks: [{ type: "command", command, timeout }] }]`).

use std::path::{Path, PathBuf};

use agentping_core::config::RemoteConfig;
use agentping_core::fsutil::atomic_write;
use serde_json::{json, Map, Value};

/// One hook AgentPing registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedHook {
    /// Claude Code hook event name.
    pub event: &'static str,
    /// Matcher, if the hook is scoped (e.g. a tool name).
    pub matcher: Option<&'static str>,
    /// Timeout in seconds.
    pub timeout: u32,
    /// Runs `hook claude --wait-reply` (waits for a remote reply in the background).
    pub wait_reply: bool,
}

impl ManagedHook {
    const fn new(event: &'static str, matcher: Option<&'static str>, timeout: u32) -> Self {
        Self {
            event,
            matcher,
            timeout,
            wait_reply: false,
        }
    }

    /// Display label, e.g. `PreToolUse(AskUserQuestion)`.
    pub fn label(&self) -> String {
        let base = match self.matcher {
            Some(m) => format!("{}({m})", self.event),
            None => self.event.to_owned(),
        };
        if self.wait_reply {
            format!("{base} [waits for Discord reply]")
        } else {
            base
        }
    }
}

/// The hooks AgentPing installs for the given remote settings.
///
/// The `permission_prompt` notification is the "you are away" ping; the `PermissionRequest`
/// hook adds the exact command to it and, with remote approvals, lets you decide from Discord.
/// Hooks that wait for a remote answer get timeouts long enough for that wait.
pub fn managed_hooks(remote: &RemoteConfig) -> Vec<ManagedHook> {
    let decision = remote
        .decision_timeout_minutes
        .saturating_mul(60)
        .saturating_add(30);
    let permission_timeout = if remote.enabled && remote.permissions {
        decision
    } else {
        10
    };
    let question_timeout = if remote.enabled && remote.questions {
        decision
    } else {
        10
    };
    let mut hooks = vec![
        ManagedHook::new("Notification", None, 10),
        ManagedHook::new("PermissionRequest", None, permission_timeout),
        ManagedHook::new("PreToolUse", Some("AskUserQuestion"), question_timeout),
        ManagedHook::new("PostToolUse", Some("AskUserQuestion"), 10),
        ManagedHook::new("Stop", None, 10),
        ManagedHook::new("StopFailure", None, 10),
        ManagedHook::new("SessionStart", None, 10),
        ManagedHook::new("SessionEnd", None, 5),
        ManagedHook::new("UserPromptSubmit", None, 10),
    ];
    if remote.enabled && remote.continue_replies {
        hooks.push(ManagedHook {
            event: "Stop",
            matcher: None,
            timeout: remote
                .reply_timeout_minutes
                .saturating_mul(60)
                .saturating_add(60),
            wait_reply: true,
        });
    }
    hooks
}

/// Which settings file to modify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `~/.claude/settings.json` (all projects).
    User,
    /// `<project>/.claude/settings.json` (shared with the team).
    Project,
    /// `<project>/.claude/settings.local.json` (this machine only).
    Local,
}

/// Claude Code's configuration directory (`CLAUDE_CONFIG_DIR` or `~/.claude`).
pub fn claude_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    agentping_core::paths::home_dir().map(|h| h.join(".claude"))
}

/// Settings file for `scope`, relative to `project_dir` for project scopes.
pub fn settings_path(scope: Scope, project_dir: &Path) -> Option<PathBuf> {
    match scope {
        Scope::User => claude_config_dir().map(|d| d.join("settings.json")),
        Scope::Project => Some(project_dir.join(".claude").join("settings.json")),
        Scope::Local => Some(project_dir.join(".claude").join("settings.local.json")),
    }
}

/// Shell flavour Claude Code will use to run the hook command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookShell {
    /// POSIX shell (macOS/Linux default).
    Posix,
    /// Git Bash on Windows.
    GitBash,
    /// PowerShell on Windows (when Git Bash is unavailable).
    PowerShell,
}

/// Picks the shell for this platform. On Windows, Claude Code runs hooks with Git Bash when it
/// is installed and PowerShell otherwise; we pin the choice explicitly with the `shell` field.
pub fn detect_shell() -> HookShell {
    if cfg!(windows) {
        if find_git_bash().is_some() {
            HookShell::GitBash
        } else {
            HookShell::PowerShell
        }
    } else {
        HookShell::Posix
    }
}

/// Locates Git Bash on Windows.
pub fn find_git_bash() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH").map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            // `...\Git\cmd\git.exe` → `...\Git\bin\bash.exe`
            if dir.join("git.exe").is_file() {
                if let Some(root) = dir.parent() {
                    candidates.push(root.join("bin").join("bash.exe"));
                }
            }
        }
    }
    for var in ["ProgramFiles", "ProgramW6432", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(var).map(PathBuf::from) {
            candidates.push(base.join("Git").join("bin").join("bash.exe"));
            candidates.push(
                base.join("Programs")
                    .join("Git")
                    .join("bin")
                    .join("bash.exe"),
            );
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Builds the hook handler object for `exe`.
pub fn handler(exe: &Path, shell: HookShell, hook: &ManagedHook) -> Value {
    let args = if hook.wait_reply {
        "hook claude --wait-reply"
    } else {
        "hook claude"
    };
    let (command, shell_name) = match shell {
        HookShell::Posix => (
            format!("{} {args}", posix_quote(&exe.to_string_lossy())),
            None,
        ),
        HookShell::GitBash => (
            format!(
                "{} {args}",
                posix_quote(&exe.to_string_lossy().replace('\\', "/"))
            ),
            Some("bash"),
        ),
        HookShell::PowerShell => (
            format!("& '{}' {args}", exe.to_string_lossy().replace('\'', "''")),
            Some("powershell"),
        ),
    };
    let mut h = json!({ "type": "command", "command": command, "timeout": hook.timeout });
    if hook.wait_reply {
        if let Some(obj) = h.as_object_mut() {
            // Runs in the background and wakes Claude when it exits with code 2.
            obj.insert("asyncRewake".into(), Value::Bool(true));
        }
    }
    if let (Some(s), Some(obj)) = (shell_name, h.as_object_mut()) {
        obj.insert("shell".into(), Value::String(s.into()));
    }
    h
}

fn posix_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-:+".contains(c))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Whether a hook handler was created by AgentPing.
pub fn is_managed_handler(handler: &Value) -> bool {
    let Some(cmd) = handler.get("command").and_then(Value::as_str) else {
        return false;
    };
    let lower = cmd.to_ascii_lowercase();
    let lower = lower.trim_end();
    lower.contains("agentping")
        && (lower.ends_with("hook claude") || lower.ends_with("hook claude --wait-reply"))
}

/// Errors while editing Claude Code settings.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// The settings file could not be read or written.
    #[error("{0}")]
    Io(String),
    /// The settings file is not valid JSON; it is left untouched.
    #[error(
        "{path} is not valid JSON ({message}); fix it before running setup (it was not modified)"
    )]
    InvalidJson {
        /// File path.
        path: String,
        /// Parser message.
        message: String,
    },
    /// The settings have an unexpected shape; they are left untouched.
    #[error("{0} (the file was not modified)")]
    UnexpectedShape(String),
}

/// A computed change to a settings file.
#[derive(Debug, Clone)]
pub struct SettingsChange {
    /// Settings file.
    pub path: PathBuf,
    /// Whether the file existed.
    pub existed: bool,
    /// New document.
    pub after: Value,
    /// Whether anything changes.
    pub changed: bool,
}

impl SettingsChange {
    /// Writes the change, first copying the original to a timestamped backup.
    /// Returns the backup path, if one was made.
    pub fn apply(&self) -> Result<Option<PathBuf>, InstallError> {
        if !self.changed {
            return Ok(None);
        }
        let backup = if self.existed {
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let name = format!(
                "{}.agentping-backup-{stamp}",
                self.path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "settings.json".into())
            );
            let backup = self.path.with_file_name(name);
            std::fs::copy(&self.path, &backup)
                .map_err(|e| InstallError::Io(format!("backup {}: {e}", backup.display())))?;
            Some(backup)
        } else {
            None
        };
        let mut text = serde_json::to_string_pretty(&self.after)
            .map_err(|e| InstallError::Io(e.to_string()))?;
        text.push('\n');
        atomic_write(&self.path, text.as_bytes(), false)
            .map_err(|e| InstallError::Io(e.to_string()))?;
        Ok(backup)
    }
}

fn load(path: &Path) -> Result<(Value, bool), InstallError> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok((Value::Object(Map::new()), true)),
        Ok(text) => {
            let v: Value = serde_json::from_str(&text).map_err(|e| InstallError::InvalidJson {
                path: path.display().to_string(),
                message: e.to_string(),
            })?;
            if !v.is_object() {
                return Err(InstallError::UnexpectedShape(format!(
                    "{} does not contain a JSON object",
                    path.display()
                )));
            }
            Ok((v, true))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok((Value::Object(Map::new()), false))
        }
        Err(e) => Err(InstallError::Io(format!("{}: {e}", path.display()))),
    }
}

/// Removes every AgentPing handler, dropping groups/events that become empty as a result.
/// Returns true when something was removed.
fn strip_managed(hooks: &mut Map<String, Value>) -> bool {
    let mut removed_any = false;
    let event_names: Vec<String> = hooks.keys().cloned().collect();
    for name in event_names {
        let Some(groups) = hooks.get_mut(&name).and_then(Value::as_array_mut) else {
            continue;
        };
        let mut removed_here = false;
        let mut i = 0;
        while i < groups.len() {
            let mut drop_group = false;
            if let Some(handlers) = groups[i].get_mut("hooks").and_then(Value::as_array_mut) {
                let before = handlers.len();
                handlers.retain(|h| !is_managed_handler(h));
                if handlers.len() != before {
                    removed_here = true;
                    drop_group = handlers.is_empty();
                }
            }
            if drop_group {
                groups.remove(i);
            } else {
                i += 1;
            }
        }
        if removed_here && groups.is_empty() {
            hooks.remove(&name);
        }
        removed_any |= removed_here;
    }
    removed_any
}

/// Computes the settings change that installs AgentPing's hooks for `exe`.
pub fn plan_install(
    path: &Path,
    exe: &Path,
    shell: HookShell,
    managed: &[ManagedHook],
) -> Result<SettingsChange, InstallError> {
    let (before, existed) = load(path)?;
    let mut after = before.clone();
    let root = after
        .as_object_mut()
        .ok_or_else(|| InstallError::UnexpectedShape("settings root is not an object".into()))?;
    let settings_hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| InstallError::UnexpectedShape("\"hooks\" is not an object".into()))?;
    for (name, v) in settings_hooks.iter() {
        if !v.is_array() {
            return Err(InstallError::UnexpectedShape(format!(
                "\"hooks.{name}\" is not an array"
            )));
        }
    }
    strip_managed(settings_hooks);
    for spec in managed {
        let groups = settings_hooks
            .entry(spec.event)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| {
                InstallError::UnexpectedShape(format!("\"hooks.{}\" is not an array", spec.event))
            })?;
        let h = handler(exe, shell, spec);
        let existing = groups.iter_mut().find(|g| {
            g.get("matcher")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
                == spec.matcher
                && g.get("hooks").is_some_and(Value::is_array)
        });
        match existing
            .and_then(|g| g.get_mut("hooks"))
            .and_then(Value::as_array_mut)
        {
            Some(handlers) => handlers.push(h),
            None => {
                let mut group = Map::new();
                if let Some(m) = spec.matcher {
                    group.insert("matcher".into(), Value::String(m.into()));
                }
                group.insert("hooks".into(), Value::Array(vec![h]));
                groups.push(Value::Object(group));
            }
        }
    }
    let changed = after != before;
    Ok(SettingsChange {
        path: path.to_path_buf(),
        existed,
        after,
        changed,
    })
}

/// Computes the settings change that removes AgentPing's hooks (and nothing else).
pub fn plan_uninstall(path: &Path) -> Result<SettingsChange, InstallError> {
    let (before, existed) = load(path)?;
    let mut after = before.clone();
    if let Some(root) = after.as_object_mut() {
        if let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) {
            let removed = strip_managed(hooks);
            if removed && hooks.is_empty() {
                root.remove("hooks");
            }
        }
    }
    let changed = after != before;
    Ok(SettingsChange {
        path: path.to_path_buf(),
        existed,
        after,
        changed,
    })
}

/// Installation state of AgentPing hooks in one settings file.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct HookStatus {
    /// Managed hook events present.
    pub installed: Vec<String>,
    /// Managed hook events missing.
    pub missing: Vec<String>,
    /// Commands referencing an executable path that no longer exists.
    pub broken_commands: Vec<String>,
}

impl HookStatus {
    /// Whether all managed hooks are present and point at an existing binary.
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty() && self.broken_commands.is_empty()
    }
}

/// Inspects a settings file for AgentPing hooks.
pub fn inspect(path: &Path, managed: &[ManagedHook]) -> Result<HookStatus, InstallError> {
    let (doc, _) = load(path)?;
    let mut status = HookStatus::default();
    let hooks = doc.get("hooks").and_then(Value::as_object);
    for spec in managed {
        let label = spec.label();
        let handlers: Vec<&Value> = hooks
            .and_then(|h| h.get(spec.event))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|g| {
                g.get("matcher")
                    .and_then(Value::as_str)
                    .filter(|m| !m.is_empty())
                    == spec.matcher
            })
            .filter_map(|g| g.get("hooks").and_then(Value::as_array))
            .flatten()
            .filter(|h| is_managed_handler(h))
            .filter(|h| {
                h.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|c| c.trim_end().ends_with("--wait-reply") == spec.wait_reply)
            })
            .collect();
        if handlers.is_empty() {
            status.missing.push(label);
            continue;
        }
        status.installed.push(label);
        for h in handlers {
            if let Some(cmd) = h.get("command").and_then(Value::as_str) {
                if let Some(exe) = command_executable(cmd) {
                    if !Path::new(&exe).exists() && !status.broken_commands.contains(&exe) {
                        status.broken_commands.push(exe);
                    }
                }
            }
        }
    }
    Ok(status)
}

/// Extracts the executable path from a command we generated.
fn command_executable(cmd: &str) -> Option<String> {
    let cmd = cmd.trim().trim_start_matches("& ").trim();
    let path = if let Some(rest) = cmd.strip_prefix('\'') {
        rest.split_once('\'').map(|(p, _)| p.to_owned())?
    } else if let Some(rest) = cmd.strip_prefix('"') {
        rest.split_once('"').map(|(p, _)| p.to_owned())?
    } else {
        cmd.split_whitespace().next()?.to_owned()
    };
    Some(path)
}

#[cfg(test)]
mod tests;
