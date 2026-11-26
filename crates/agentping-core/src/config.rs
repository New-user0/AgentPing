//! User configuration (`config.toml`): schema, defaults, validation, migration and editing.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::event::{EventType, Severity};
use crate::paths::{expand_tilde, Paths};

/// Newest configuration schema version understood by this build.
pub const CONFIG_VERSION: u32 = 1;

/// Default loopback port for the local daemon API.
pub const DEFAULT_PORT: u16 = 38471;

/// Provider identifiers known to AgentPing.
pub const PROVIDER_NAMES: [&str; 3] = ["discord", "telegram", "whatsapp"];

/// Placeholders accepted in notification templates.
pub const TEMPLATE_PLACEHOLDERS: [&str; 9] = [
    "project", "agent", "event", "message", "title", "session", "severity", "time", "emoji",
];

/// Complete AgentPing configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Schema version of this file.
    pub config_version: u32,
    /// General settings.
    pub general: GeneralConfig,
    /// Local daemon settings.
    pub daemon: DaemonConfig,
    /// Agent integrations.
    pub agents: AgentsConfig,
    /// Notification providers.
    pub providers: ProvidersConfig,
    /// Notification preferences.
    pub notifications: NotificationsConfig,
    /// Quiet hours.
    pub quiet_hours: QuietHoursConfig,
    /// Ordered notification rules. The first matching rule wins.
    pub rules: Vec<RuleConfig>,
    /// Per-event-type template overrides, keyed by event type (e.g. `permission_request`).
    pub templates: BTreeMap<String, TemplateConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: CONFIG_VERSION,
            general: GeneralConfig::default(),
            daemon: DaemonConfig::default(),
            agents: AgentsConfig::default(),
            providers: ProvidersConfig::default(),
            notifications: NotificationsConfig::default(),
            quiet_hours: QuietHoursConfig::default(),
            rules: Vec::new(),
            templates: BTreeMap::new(),
        }
    }
}

/// `[general]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GeneralConfig {
    /// One of `error`, `warn`, `info`, `debug`, `trace`.
    pub log_level: String,
    /// Optional database path override (`~` is expanded).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database_path: Option<String>,
    /// Days of event history to keep.
    pub retention_days: u32,
    /// Master switch for all notifications.
    pub notifications_enabled: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            log_level: "info".into(),
            database_path: None,
            retention_days: 30,
            notifications_enabled: true,
        }
    }
}

/// `[daemon]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DaemonConfig {
    /// Loopback address to bind. Non-loopback addresses are rejected.
    pub host: String,
    /// TCP port.
    pub port: u16,
    /// When the daemon is not running, hooks start a short-lived background delivery process.
    pub spawn_delivery_when_offline: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: DEFAULT_PORT,
            spawn_delivery_when_offline: true,
        }
    }
}

/// `[agents]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct AgentsConfig {
    /// `[agents.claude]`
    pub claude: ClaudeAgentConfig,
}

/// `[agents.claude]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ClaudeAgentConfig {
    /// Accept events from Claude Code hooks.
    pub enabled: bool,
    /// Seconds to wait before notifying about an `AskUserQuestion` prompt. If the question is
    /// answered locally within this window, the notification is cancelled.
    pub question_delay_secs: u32,
}

impl Default for ClaudeAgentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            question_delay_secs: 15,
        }
    }
}

/// `[providers]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ProvidersConfig {
    /// `[providers.discord]`
    pub discord: DiscordConfig,
    /// `[providers.telegram]`
    pub telegram: TelegramConfig,
    /// `[providers.whatsapp]`
    pub whatsapp: WhatsappConfig,
}

impl ProvidersConfig {
    /// Names of providers that are enabled (and implemented).
    pub fn enabled_names(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.discord.enabled {
            out.push("discord");
        }
        if self.telegram.enabled {
            out.push("telegram");
        }
        out
    }
}

/// `[providers.discord]` — the webhook URL itself is a secret and lives in the credential store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DiscordConfig {
    /// Deliver notifications to Discord.
    pub enabled: bool,
    /// Display name for webhook messages.
    pub username: String,
}

impl Default for DiscordConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            username: "AgentPing".into(),
        }
    }
}

/// `[providers.telegram]` — the bot token is a secret and lives in the credential store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct TelegramConfig {
    /// Deliver notifications to Telegram.
    pub enabled: bool,
    /// Target chat id (numeric id or `@channelname`).
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "string_or_integer"
    )]
    pub chat_id: Option<String>,
    /// Send messages silently (no sound) for low-severity events.
    pub silent_low_severity: bool,
}

fn string_or_integer<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        S(String),
        I(i64),
    }
    Ok(Option::<Raw>::deserialize(d)?.map(|r| match r {
        Raw::S(s) => s,
        Raw::I(i) => i.to_string(),
    }))
}

/// `[providers.whatsapp]` — not implemented yet; see docs/integrations/whatsapp.md.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct WhatsappConfig {
    /// Must remain `false` until WhatsApp support ships.
    pub enabled: bool,
}

/// `[notifications]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NotificationsConfig {
    /// Notify when an agent needs permission.
    pub permission_requests: bool,
    /// Notify when an agent asks a question.
    pub user_questions: bool,
    /// Notify when an agent finishes.
    pub task_completed: bool,
    /// Notify when an agent stops because of an error.
    pub task_failed: bool,
    /// Notify when an agent has been waiting idle for input.
    pub agent_idle: bool,
    /// Notify when sessions start or end.
    pub session_events: bool,
    /// Notify about other agent notifications.
    pub other_notifications: bool,
    /// Include the agent's message text in notifications.
    pub include_message: bool,
    /// Include full project paths in notifications (otherwise only the project name).
    pub include_project_path: bool,
    /// Truncate message text to this many characters (`0` = never truncate; long messages are
    /// split across several provider messages instead).
    pub max_message_chars: usize,
    /// Suppress repeated notifications of the same type for the same session within this window.
    pub repeat_window_secs: u64,
    /// `[notifications.filters]`
    pub filters: FiltersConfig,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            permission_requests: true,
            user_questions: true,
            task_completed: true,
            task_failed: true,
            agent_idle: true,
            session_events: false,
            other_notifications: true,
            include_message: true,
            include_project_path: false,
            max_message_chars: 0,
            repeat_window_secs: 60,
            filters: FiltersConfig::default(),
        }
    }
}

impl NotificationsConfig {
    /// Whether notifications for `event_type` are enabled by the per-type toggles.
    pub fn enabled_for(&self, event_type: &EventType) -> bool {
        match event_type {
            EventType::PermissionRequest => self.permission_requests,
            EventType::UserQuestion => self.user_questions,
            EventType::TaskCompleted => self.task_completed,
            EventType::TaskFailed => self.task_failed,
            EventType::AgentIdle => self.agent_idle,
            EventType::AgentStarted | EventType::AgentStopped => self.session_events,
            EventType::Notification | EventType::Custom(_) => self.other_notifications,
            EventType::Activity => false,
        }
    }
}

/// `[notifications.filters]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct FiltersConfig {
    /// When non-empty, only these projects (case-insensitive names) notify.
    pub include_projects: Vec<String>,
    /// These projects never notify.
    pub exclude_projects: Vec<String>,
}

/// `[quiet_hours]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct QuietHoursConfig {
    /// Enable quiet hours.
    pub enabled: bool,
    /// Local start time, `HH:MM`.
    pub start: String,
    /// Local end time, `HH:MM` (may be earlier than `start` to span midnight).
    pub end: String,
    /// Events at or above this severity are still delivered during quiet hours.
    pub min_severity: Severity,
}

impl Default for QuietHoursConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            start: "22:00".into(),
            end: "07:00".into(),
            min_severity: Severity::Urgent,
        }
    }
}

/// One `[[rules]]` entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RuleConfig {
    /// Name shown in decisions and diagnostics.
    pub name: String,
    /// Match agents (e.g. `claude`). Empty matches all.
    pub agents: Vec<String>,
    /// Match event types (e.g. `permission_request`). Empty matches all.
    pub event_types: Vec<String>,
    /// Match project names (case-insensitive). Empty matches all.
    pub projects: Vec<String>,
    /// Match project paths with glob patterns. Empty matches all.
    pub project_paths: Vec<String>,
    /// Match only events at or above this severity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_severity: Option<Severity>,
    /// Whether matching events notify.
    pub notify: bool,
    /// Restrict delivery to these providers. Empty means all enabled providers.
    pub providers: Vec<String>,
    /// Override the event severity (e.g. `urgent` to bypass quiet hours).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
}

impl Default for RuleConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            agents: Vec::new(),
            event_types: Vec::new(),
            projects: Vec::new(),
            project_paths: Vec::new(),
            min_severity: None,
            notify: true,
            providers: Vec::new(),
            severity: None,
        }
    }
}

/// `[templates.<event_type>]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct TemplateConfig {
    /// Title template.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Body template.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// A single validation problem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigIssue {
    /// Dotted path of the offending setting.
    pub key: String,
    /// What is wrong and how to fix it.
    pub message: String,
}

impl fmt::Display for ConfigIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.key, self.message)
    }
}

/// Result of loading the configuration file.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    /// Effective configuration (file + defaults + environment overrides).
    pub config: Config,
    /// Path of the configuration file.
    pub path: PathBuf,
    /// Whether the file exists (false means built-in defaults are in use).
    pub exists: bool,
}

impl Config {
    /// Loads, migrates, applies environment overrides and validates the configuration.
    pub fn load(paths: &Paths) -> Result<LoadedConfig, CoreError> {
        let path = paths.config_file();
        let (config, exists) = match std::fs::read_to_string(&path) {
            Ok(text) => (Self::from_toml_str(&text, &path)?, true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), false),
            Err(e) => return Err(CoreError::io(&path, e)),
        };
        let mut config = config;
        config.apply_env_overrides(|k| std::env::var(k).ok());
        let issues = config.validate();
        if !issues.is_empty() {
            return Err(CoreError::ConfigInvalid(issues));
        }
        Ok(LoadedConfig {
            config,
            path,
            exists,
        })
    }

    /// Parses and migrates configuration text (without environment overrides or validation).
    pub fn from_toml_str(text: &str, path: &Path) -> Result<Self, CoreError> {
        let value: toml::Value = toml::from_str(text).map_err(|e| CoreError::ConfigParse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        let value = migrate(value)?;
        value
            .try_into()
            .map_err(|e: toml::de::Error| CoreError::ConfigParse {
                path: path.to_path_buf(),
                message: e.to_string(),
            })
    }

    /// Applies supported environment variable overrides.
    pub fn apply_env_overrides(&mut self, lookup: impl Fn(&str) -> Option<String>) {
        if let Some(level) = lookup("AGENTPING_LOG_LEVEL").filter(|v| !v.is_empty()) {
            self.general.log_level = level;
        }
        if let Some(port) = lookup("AGENTPING_DAEMON_PORT").and_then(|v| v.parse().ok()) {
            self.daemon.port = port;
        }
        if let Some(v) = lookup("AGENTPING_NOTIFICATIONS_ENABLED") {
            match v.as_str() {
                "0" | "false" | "no" | "off" => self.general.notifications_enabled = false,
                "1" | "true" | "yes" | "on" => self.general.notifications_enabled = true,
                _ => {}
            }
        }
    }

    /// Effective database path.
    pub fn database_path(&self, paths: &Paths) -> PathBuf {
        self.general
            .database_path
            .as_deref()
            .filter(|p| !p.trim().is_empty())
            .map(expand_tilde)
            .unwrap_or_else(|| paths.default_database())
    }

    /// Validates semantic constraints, returning every problem found.
    pub fn validate(&self) -> Vec<ConfigIssue> {
        let mut issues = Vec::new();
        let mut issue = |key: &str, message: String| {
            issues.push(ConfigIssue {
                key: key.to_owned(),
                message,
            })
        };

        if !matches!(
            self.general.log_level.as_str(),
            "error" | "warn" | "info" | "debug" | "trace"
        ) {
            issue(
                "general.log_level",
                format!(
                    "'{}' is not a log level (use error, warn, info, debug or trace)",
                    self.general.log_level
                ),
            );
        }
        if self.general.retention_days == 0 {
            issue(
                "general.retention_days",
                "must be at least 1 (use `agentping events clear` to delete history)".into(),
            );
        }
        if !is_loopback_host(&self.daemon.host) {
            issue(
                "daemon.host",
                format!(
                    "'{}' is not a loopback address; the daemon only binds to 127.0.0.1, ::1 or localhost",
                    self.daemon.host
                ),
            );
        }
        if self.daemon.port == 0 {
            issue("daemon.port", "must be between 1 and 65535".into());
        }
        if self.agents.claude.question_delay_secs > 600 {
            issue(
                "agents.claude.question_delay_secs",
                "must be 600 seconds or less".into(),
            );
        }
        if self.providers.whatsapp.enabled {
            issue(
                "providers.whatsapp.enabled",
                "WhatsApp delivery is not implemented yet (see docs/integrations/whatsapp.md); set it to false".into(),
            );
        }
        if self.providers.telegram.enabled
            && self
                .providers
                .telegram
                .chat_id
                .as_deref()
                .is_none_or(|c| c.trim().is_empty())
        {
            issue(
                "providers.telegram.chat_id",
                "is required when Telegram is enabled (run `agentping setup telegram`)".into(),
            );
        }
        if let Some(chat) = self.providers.telegram.chat_id.as_deref() {
            let chat = chat.trim();
            let numeric = chat.strip_prefix('-').unwrap_or(chat);
            let valid = chat.is_empty()
                || (!numeric.is_empty() && numeric.chars().all(|c| c.is_ascii_digit()))
                || (chat.starts_with('@') && chat.len() > 1);
            if !valid {
                issue(
                    "providers.telegram.chat_id",
                    format!("'{chat}' must be a numeric chat id or @channelname"),
                );
            }
        }
        if self.providers.discord.username.chars().count() > 80 {
            issue(
                "providers.discord.username",
                "must be 80 characters or fewer".into(),
            );
        }
        let max = self.notifications.max_message_chars;
        if max != 0 && !(40..=crate::event::MAX_MESSAGE_CHARS).contains(&max) {
            issue(
                "notifications.max_message_chars",
                format!(
                    "must be 0 (no limit) or between 40 and {}",
                    crate::event::MAX_MESSAGE_CHARS
                ),
            );
        }
        for (field, value) in [
            ("quiet_hours.start", &self.quiet_hours.start),
            ("quiet_hours.end", &self.quiet_hours.end),
        ] {
            if parse_hhmm(value).is_none() {
                issue(field, format!("'{value}' must be a 24h time like 22:00"));
            }
        }
        for (i, rule) in self.rules.iter().enumerate() {
            let key = format!("rules[{i}]");
            for t in &rule.event_types {
                if !EventType::is_builtin_name(t) {
                    issue(
                        &format!("{key}.event_types"),
                        format!(
                            "unknown event type '{t}' (known: {})",
                            builtin_event_type_names().join(", ")
                        ),
                    );
                }
            }
            for p in &rule.providers {
                if !PROVIDER_NAMES.contains(&p.as_str()) {
                    issue(
                        &format!("{key}.providers"),
                        format!("unknown provider '{p}' (known: discord, telegram)"),
                    );
                }
            }
            for g in &rule.project_paths {
                if let Err(e) = globset::Glob::new(g) {
                    issue(
                        &format!("{key}.project_paths"),
                        format!("invalid glob '{g}': {e}"),
                    );
                }
            }
        }
        for (event_type, template) in &self.templates {
            if !EventType::is_builtin_name(event_type) && event_type != "default" {
                issue(
                    &format!("templates.{event_type}"),
                    "unknown event type (use an event type name or `default`)".into(),
                );
            }
            for (field, text) in [("title", &template.title), ("body", &template.body)] {
                if let Some(text) = text {
                    for name in template_placeholders(text) {
                        if !TEMPLATE_PLACEHOLDERS.contains(&name) {
                            issue(
                                &format!("templates.{event_type}.{field}"),
                                format!(
                                    "unknown placeholder {{{name}}} (available: {})",
                                    TEMPLATE_PLACEHOLDERS
                                        .iter()
                                        .map(|p| format!("{{{p}}}"))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ),
                            );
                        }
                    }
                }
            }
        }
        issues
    }

    /// Serializes the configuration as TOML.
    pub fn to_toml_string(&self) -> Result<String, CoreError> {
        toml::to_string_pretty(self).map_err(|e| CoreError::ConfigKey {
            key: "<config>".into(),
            reason: e.to_string(),
        })
    }
}

/// Names of all built-in event types.
pub fn builtin_event_type_names() -> Vec<&'static str> {
    vec![
        "permission_request",
        "user_question",
        "task_completed",
        "task_failed",
        "agent_idle",
        "agent_started",
        "agent_stopped",
        "notification",
        "activity",
    ]
}

/// Returns true for loopback hostnames/addresses.
pub fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.trim_matches(['[', ']'])
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// Parses `HH:MM` (24h).
pub fn parse_hhmm(value: &str) -> Option<chrono::NaiveTime> {
    chrono::NaiveTime::parse_from_str(value.trim(), "%H:%M").ok()
}

/// Extracts `{placeholder}` names from a template string.
pub fn template_placeholders(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                let name = &after[..end];
                if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    out.push(name);
                }
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    out
}

/// Upgrades older configuration documents to [`CONFIG_VERSION`].
///
/// A missing `config_version` is treated as version 1. Future schema changes add a step here so
/// existing files keep working after an upgrade.
fn migrate(mut value: toml::Value) -> Result<toml::Value, CoreError> {
    let version = value
        .get("config_version")
        .and_then(toml::Value::as_integer)
        .unwrap_or(1);
    let version = u32::try_from(version).unwrap_or(u32::MAX);
    if version > CONFIG_VERSION {
        return Err(CoreError::ConfigTooNew {
            found: version,
            supported: CONFIG_VERSION,
        });
    }
    if let Some(table) = value.as_table_mut() {
        table.insert(
            "config_version".into(),
            toml::Value::Integer(i64::from(CONFIG_VERSION)),
        );
    }
    Ok(value)
}

/// Sets a dotted `key` to `raw_value` in configuration text, preserving comments and layout.
///
/// The value is parsed as a TOML value when possible (`true`, `42`, `["a"]`), otherwise it is
/// stored as a string. The resulting document must still deserialize and validate.
pub fn set_value(text: &str, key: &str, raw_value: &str) -> Result<String, CoreError> {
    let mut doc: toml_edit::DocumentMut =
        text.parse()
            .map_err(|e: toml_edit::TomlError| CoreError::ConfigKey {
                key: key.into(),
                reason: format!("existing config is not valid TOML: {e}"),
            })?;
    let segments: Vec<&str> = key.split('.').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(CoreError::ConfigKey {
            key: key.into(),
            reason: "keys look like `notifications.include_message`".into(),
        });
    }
    let value = parse_edit_value(raw_value);
    let (last, parents) = segments.split_last().ok_or_else(|| CoreError::ConfigKey {
        key: key.into(),
        reason: "empty key".into(),
    })?;
    let mut table = doc.as_table_mut();
    for segment in parents {
        let entry = table
            .entry(segment)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        table = entry.as_table_mut().ok_or_else(|| CoreError::ConfigKey {
            key: key.into(),
            reason: format!("'{segment}' is not a table"),
        })?;
    }
    table.insert(last, toml_edit::value(value));
    let out = doc.to_string();
    let parsed = Config::from_toml_str(&out, Path::new("config.toml")).map_err(|e| {
        CoreError::ConfigKey {
            key: key.into(),
            reason: match e {
                CoreError::ConfigParse { message, .. } => message,
                other => other.to_string(),
            },
        }
    })?;
    let issues = parsed.validate();
    if let Some(first) = issues.first() {
        return Err(CoreError::ConfigKey {
            key: key.into(),
            reason: first.to_string(),
        });
    }
    Ok(out)
}

fn parse_edit_value(raw: &str) -> toml_edit::Value {
    let trimmed = raw.trim();
    if let Ok(v) = format!("v = {trimmed}").parse::<toml_edit::DocumentMut>() {
        if let Some(item) = v.get("v").and_then(|i| i.as_value()) {
            let mut value = item.clone();
            value.decor_mut().clear();
            return value;
        }
    }
    toml_edit::Value::from(raw)
}

/// Commented default configuration written by `agentping init`.
pub const DEFAULT_CONFIG_TOML: &str = r#"# AgentPing configuration
# Reference: https://github.com/New-user0/AgentPing/blob/main/docs/configuration.md
# Secrets (webhook URLs, bot tokens) are NOT stored here; they live in your OS credential store.
config_version = 1

[general]
log_level = "info"            # error | warn | info | debug | trace
retention_days = 30           # days of event history to keep
notifications_enabled = true  # master switch
# database_path = "~/agentping.db"   # optional override

[daemon]
host = "127.0.0.1"            # loopback only
port = 38471
spawn_delivery_when_offline = true   # hooks start a short-lived sender when the daemon is not running

[agents.claude]
enabled = true
question_delay_secs = 15      # wait before notifying about a question; cancelled if answered locally

[providers.discord]
enabled = false
username = "AgentPing"

[providers.telegram]
enabled = false
# chat_id = "123456789"
silent_low_severity = false

[providers.whatsapp]
enabled = false               # not implemented yet

[notifications]
permission_requests = true
user_questions = true
task_completed = true
task_failed = true
agent_idle = true
session_events = false
other_notifications = true
include_message = true        # include agent message text in notifications
include_project_path = false  # include full paths (otherwise project name only)
max_message_chars = 0         # 0 = full text (long messages are split); otherwise truncate
repeat_window_secs = 60       # suppress identical event types per session within this window

[notifications.filters]
include_projects = []
exclude_projects = []

[quiet_hours]
enabled = false
start = "22:00"
end = "07:00"
min_severity = "urgent"       # events at or above this severity still notify

# Rules are evaluated in order; the first match wins.
# [[rules]]
# name = "permissions everywhere"
# event_types = ["permission_request"]
# providers = ["discord", "telegram"]
# severity = "urgent"
#
# [[rules]]
# name = "mute sandbox"
# projects = ["sandbox"]
# notify = false

# Template overrides. Placeholders: {project} {agent} {event} {message} {title} {session} {severity} {time} {emoji}
# [templates.task_completed]
# title = "{emoji} {agent} finished in {project}"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_template_matches_default_struct() {
        let parsed = Config::from_toml_str(DEFAULT_CONFIG_TOML, Path::new("x")).unwrap();
        assert_eq!(parsed, Config::default());
        assert!(parsed.validate().is_empty());
    }

    #[test]
    fn empty_file_uses_defaults() {
        let parsed = Config::from_toml_str("", Path::new("x")).unwrap();
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn unknown_keys_are_rejected_with_location() {
        let err = Config::from_toml_str("[general]\nlog_levle = \"info\"\n", Path::new("c.toml"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("log_levle"), "{err}");
    }

    #[test]
    fn newer_config_version_is_rejected() {
        let err = Config::from_toml_str("config_version = 99", Path::new("x")).unwrap_err();
        assert!(matches!(err, CoreError::ConfigTooNew { found: 99, .. }));
    }

    #[test]
    fn validation_reports_all_problems() {
        let mut c = Config::default();
        c.general.log_level = "loud".into();
        c.daemon.host = "0.0.0.0".into();
        c.providers.whatsapp.enabled = true;
        c.providers.telegram.enabled = true;
        c.quiet_hours.start = "25:00".into();
        c.rules.push(RuleConfig {
            event_types: vec!["nope".into()],
            providers: vec!["pager".into()],
            project_paths: vec!["[".into()],
            ..RuleConfig::default()
        });
        c.templates.insert(
            "task_completed".into(),
            TemplateConfig {
                title: Some("{bogus} done".into()),
                body: None,
            },
        );
        let keys: Vec<String> = c.validate().into_iter().map(|i| i.key).collect();
        for expected in [
            "general.log_level",
            "daemon.host",
            "providers.whatsapp.enabled",
            "providers.telegram.chat_id",
            "quiet_hours.start",
            "rules[0].event_types",
            "rules[0].providers",
            "rules[0].project_paths",
            "templates.task_completed.title",
        ] {
            assert!(
                keys.iter().any(|k| k == expected),
                "missing {expected} in {keys:?}"
            );
        }
    }

    #[test]
    fn loopback_detection() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("::1"));
        assert!(is_loopback_host("[::1]"));
        assert!(is_loopback_host("localhost"));
        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("192.168.1.5"));
    }

    #[test]
    fn env_overrides_apply() {
        let mut c = Config::default();
        c.apply_env_overrides(|k| match k {
            "AGENTPING_LOG_LEVEL" => Some("debug".into()),
            "AGENTPING_DAEMON_PORT" => Some("40000".into()),
            "AGENTPING_NOTIFICATIONS_ENABLED" => Some("false".into()),
            _ => None,
        });
        assert_eq!(c.general.log_level, "debug");
        assert_eq!(c.daemon.port, 40000);
        assert!(!c.general.notifications_enabled);
    }

    #[test]
    fn set_value_preserves_comments_and_types() {
        let out = set_value(
            DEFAULT_CONFIG_TOML,
            "notifications.include_message",
            "false",
        )
        .unwrap();
        assert!(out.contains("# AgentPing configuration"));
        let c = Config::from_toml_str(&out, Path::new("x")).unwrap();
        assert!(!c.notifications.include_message);

        // Chat ids typed as bare numbers are accepted and stored as strings.
        let out = set_value(&out, "providers.telegram.chat_id", "-100123").unwrap();
        let c = Config::from_toml_str(&out, Path::new("x")).unwrap();
        assert_eq!(c.providers.telegram.chat_id.as_deref(), Some("-100123"));

        let out = set_value(
            "",
            "notifications.filters.exclude_projects",
            r#"["sandbox"]"#,
        )
        .unwrap();
        let c = Config::from_toml_str(&out, Path::new("x")).unwrap();
        assert_eq!(c.notifications.filters.exclude_projects, vec!["sandbox"]);
    }

    #[test]
    fn set_value_rejects_unknown_and_invalid() {
        assert!(set_value("", "notifications.nope", "true").is_err());
        assert!(set_value("", "daemon.host", "0.0.0.0").is_err());
        assert!(set_value("", "general.log_level", "loud").is_err());
    }

    #[test]
    fn placeholders_are_extracted() {
        assert_eq!(
            template_placeholders("{emoji} {agent} in {project} {not a placeholder}"),
            vec!["emoji", "agent", "project"]
        );
    }

    #[test]
    fn notification_toggles_map_event_types() {
        let n = NotificationsConfig {
            task_completed: false,
            ..Default::default()
        };
        assert!(!n.enabled_for(&EventType::TaskCompleted));
        assert!(n.enabled_for(&EventType::PermissionRequest));
        assert!(!n.enabled_for(&EventType::Activity));
        assert!(!n.enabled_for(&EventType::AgentStarted));
    }
}
