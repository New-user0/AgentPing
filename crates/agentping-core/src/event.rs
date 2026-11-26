//! The unified, versioned AgentPing event model.
//!
//! Agent integrations translate native agent payloads into [`Event`]s. Everything
//! downstream (storage, rules, notification providers) only ever sees this type.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Current version of the serialized event schema.
pub const EVENT_SCHEMA_VERSION: u32 = 1;

/// Maximum accepted length (in bytes) of an event message.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// Maximum accepted length (in bytes) of short text fields (titles, names, ids).
pub const MAX_SHORT_FIELD_BYTES: usize = 4 * 1024;
/// Maximum accepted size of serialized metadata.
pub const MAX_METADATA_BYTES: usize = 32 * 1024;

/// Normalized event category.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EventType {
    /// The agent is blocked waiting for the user to approve an action.
    PermissionRequest,
    /// The agent asked the user a question and is waiting for an answer.
    UserQuestion,
    /// The agent finished responding / completed its task.
    TaskCompleted,
    /// The agent stopped because of an error.
    TaskFailed,
    /// The agent has been idle, waiting for input.
    AgentIdle,
    /// A session started.
    AgentStarted,
    /// A session ended.
    AgentStopped,
    /// A generic notification emitted by the agent.
    Notification,
    /// Status-only activity (for example the user submitted a prompt). Never notifies.
    Activity,
    /// An integration-specific event type.
    Custom(String),
}

impl EventType {
    /// All built-in (non-custom) event types.
    pub const BUILTIN: [EventType; 9] = [
        EventType::PermissionRequest,
        EventType::UserQuestion,
        EventType::TaskCompleted,
        EventType::TaskFailed,
        EventType::AgentIdle,
        EventType::AgentStarted,
        EventType::AgentStopped,
        EventType::Notification,
        EventType::Activity,
    ];

    /// Stable snake_case identifier used in config, storage and the API.
    pub fn as_str(&self) -> &str {
        match self {
            EventType::PermissionRequest => "permission_request",
            EventType::UserQuestion => "user_question",
            EventType::TaskCompleted => "task_completed",
            EventType::TaskFailed => "task_failed",
            EventType::AgentIdle => "agent_idle",
            EventType::AgentStarted => "agent_started",
            EventType::AgentStopped => "agent_stopped",
            EventType::Notification => "notification",
            EventType::Activity => "activity",
            EventType::Custom(name) => name,
        }
    }

    /// Human readable label.
    pub fn label(&self) -> &str {
        match self {
            EventType::PermissionRequest => "Permission request",
            EventType::UserQuestion => "Question",
            EventType::TaskCompleted => "Task completed",
            EventType::TaskFailed => "Task failed",
            EventType::AgentIdle => "Waiting for input",
            EventType::AgentStarted => "Session started",
            EventType::AgentStopped => "Session ended",
            EventType::Notification => "Notification",
            EventType::Activity => "Activity",
            EventType::Custom(name) => name,
        }
    }

    /// Default severity for events of this type.
    pub fn default_severity(&self) -> Severity {
        match self {
            EventType::PermissionRequest | EventType::UserQuestion | EventType::TaskFailed => {
                Severity::High
            }
            EventType::TaskCompleted | EventType::Notification | EventType::Custom(_) => {
                Severity::Normal
            }
            EventType::AgentIdle
            | EventType::AgentStarted
            | EventType::AgentStopped
            | EventType::Activity => Severity::Low,
        }
    }

    /// Returns true when `name` is a built-in event type identifier.
    pub fn is_builtin_name(name: &str) -> bool {
        Self::BUILTIN.iter().any(|t| t.as_str() == name)
    }
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EventType {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::BUILTIN
            .iter()
            .find(|t| t.as_str() == s)
            .cloned()
            .unwrap_or_else(|| EventType::Custom(s.to_owned())))
    }
}

impl Serialize for EventType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EventType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.parse() {
            Ok(t) => Ok(t),
            Err(never) => match never {},
        }
    }
}

/// The kind of agent that produced an event.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentKind {
    /// Anthropic's Claude Code.
    Claude,
    /// Any other agent, identified by a short lowercase id.
    Other(String),
}

impl AgentKind {
    /// Stable identifier used in config, storage and the API.
    pub fn as_str(&self) -> &str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Other(id) => id,
        }
    }

    /// Human readable product name.
    pub fn display_name(&self) -> &str {
        match self {
            AgentKind::Claude => "Claude Code",
            AgentKind::Other(id) => id,
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentKind {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "claude" | "claude-code" | "claude_code" => AgentKind::Claude,
            other => AgentKind::Other(other.to_owned()),
        })
    }
}

impl Serialize for AgentKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AgentKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.parse() {
            Ok(t) => Ok(t),
            Err(never) => match never {},
        }
    }
}

/// How urgent an event is. Ordered from least to most urgent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Informational; safe to batch or drop.
    Low,
    /// Normal priority.
    Normal,
    /// Needs attention soon.
    High,
    /// Needs attention now; delivered even during quiet hours by default.
    Urgent,
}

impl Severity {
    /// Stable identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Low => "low",
            Severity::Normal => "normal",
            Severity::High => "high",
            Severity::Urgent => "urgent",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Severity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "low" => Ok(Severity::Low),
            "normal" => Ok(Severity::Normal),
            "high" => Ok(Severity::High),
            "urgent" => Ok(Severity::Urgent),
            other => Err(format!(
                "unknown severity '{other}' (expected low, normal, high or urgent)"
            )),
        }
    }
}

/// Where an event came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EventSource {
    /// Integration identifier, e.g. `claude-hooks`.
    pub integration: String,
    /// Native event name, e.g. `Notification`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_event: Option<String>,
    /// Native subtype, e.g. `permission_prompt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
}

/// Hints that influence how notifications for an event are scheduled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DeliveryHints {
    /// Delay delivery by this many seconds (so it can be cancelled if the user responds locally).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub delay_secs: u32,
    /// Key under which pending notifications for this event can be cancelled later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_key: Option<String>,
    /// When set, this event cancels pending notifications registered under this key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancels: Option<String>,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

/// A normalized agent event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Schema version of this event.
    pub schema_version: u32,
    /// Unique id (UUIDv7, time ordered).
    pub id: Uuid,
    /// Normalized event type.
    pub event_type: EventType,
    /// Agent that produced the event.
    pub agent: AgentKind,
    /// Human readable agent name.
    pub agent_name: String,
    /// Agent session id, when the agent provides one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Short project name (e.g. the repository folder name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    /// Project root path, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    /// Working directory of the agent, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Short title supplied by the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Message supplied by the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Severity.
    pub severity: Severity,
    /// When the event happened.
    pub occurred_at: DateTime<Utc>,
    /// Origin of the event.
    #[serde(default)]
    pub source: EventSource,
    /// Scheduling hints.
    #[serde(default)]
    pub delivery: DeliveryHints,
    /// Additional structured, non-secret metadata.
    #[serde(default)]
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

/// Validation failure for an incoming event.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EventValidationError {
    /// The schema version is not supported by this build.
    #[error("unsupported event schema version {0} (this build supports {EVENT_SCHEMA_VERSION})")]
    UnsupportedSchema(u32),
    /// A field exceeds its size limit.
    #[error("field '{0}' is too large")]
    TooLarge(&'static str),
    /// A field is empty or malformed.
    #[error("field '{0}' is invalid")]
    Invalid(&'static str),
}

impl Event {
    /// Creates an event with defaults derived from its type.
    pub fn new(event_type: EventType, agent: AgentKind, occurred_at: DateTime<Utc>) -> Self {
        let severity = event_type.default_severity();
        let agent_name = agent.display_name().to_owned();
        Self {
            schema_version: EVENT_SCHEMA_VERSION,
            id: Uuid::now_v7(),
            event_type,
            agent,
            agent_name,
            session_id: None,
            project_name: None,
            project_path: None,
            cwd: None,
            title: None,
            message: None,
            severity,
            occurred_at,
            source: EventSource::default(),
            delivery: DeliveryHints::default(),
            metadata: serde_json::Map::new(),
        }
    }

    /// Validates an event received from an untrusted-ish local source.
    pub fn validate(&self) -> Result<(), EventValidationError> {
        if self.schema_version != EVENT_SCHEMA_VERSION {
            return Err(EventValidationError::UnsupportedSchema(self.schema_version));
        }
        let short_fields: [(&'static str, Option<&str>); 7] = [
            ("agent_name", Some(self.agent_name.as_str())),
            ("session_id", self.session_id.as_deref()),
            ("project_name", self.project_name.as_deref()),
            ("project_path", self.project_path.as_deref()),
            ("cwd", self.cwd.as_deref()),
            ("title", self.title.as_deref()),
            ("event_type", Some(self.event_type.as_str())),
        ];
        for (name, value) in short_fields {
            if value.is_some_and(|v| v.len() > MAX_SHORT_FIELD_BYTES) {
                return Err(EventValidationError::TooLarge(name));
            }
        }
        if self.agent.as_str().is_empty() || self.agent.as_str().len() > 64 {
            return Err(EventValidationError::Invalid("agent"));
        }
        if self.event_type.as_str().is_empty() {
            return Err(EventValidationError::Invalid("event_type"));
        }
        if self
            .message
            .as_ref()
            .is_some_and(|m| m.len() > MAX_MESSAGE_BYTES)
        {
            return Err(EventValidationError::TooLarge("message"));
        }
        let metadata_len = serde_json::to_vec(&self.metadata)
            .map(|v| v.len())
            .unwrap_or(usize::MAX);
        if metadata_len > MAX_METADATA_BYTES {
            return Err(EventValidationError::TooLarge("metadata"));
        }
        if self.delivery.delay_secs > 3600 {
            return Err(EventValidationError::Invalid("delivery.delay_secs"));
        }
        Ok(())
    }

    /// Deterministic fingerprint used to detect duplicate deliveries of the same native event.
    ///
    /// Two events with the same fingerprint received within the dedup window are treated as one.
    pub fn dedup_key(&self) -> String {
        let mut hasher = Sha256::new();
        let parts: [&str; 8] = [
            self.agent.as_str(),
            self.session_id.as_deref().unwrap_or(""),
            self.event_type.as_str(),
            self.source.native_event.as_deref().unwrap_or(""),
            self.source.subtype.as_deref().unwrap_or(""),
            self.title.as_deref().unwrap_or(""),
            self.message.as_deref().unwrap_or(""),
            self.metadata
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        ];
        for part in parts {
            hasher.update(part.as_bytes());
            hasher.update([0x1f]);
        }
        hex::encode(hasher.finalize())
    }

    /// Short (8 character) prefix of the session id, for display.
    pub fn session_short(&self) -> Option<&str> {
        self.session_id.as_deref().map(|s| s.get(..8).unwrap_or(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Event {
        let mut e = Event::new(EventType::PermissionRequest, AgentKind::Claude, Utc::now());
        e.session_id = Some("abc123def456".into());
        e.message = Some("Claude needs your permission to use Bash".into());
        e
    }

    #[test]
    fn event_type_roundtrips_through_strings() {
        for t in EventType::BUILTIN {
            let parsed: EventType = t.as_str().parse().unwrap();
            assert_eq!(parsed, t);
        }
        assert_eq!(
            "something_else".parse::<EventType>().unwrap(),
            EventType::Custom("something_else".into())
        );
    }

    #[test]
    fn event_serializes_with_snake_case_identifiers() {
        let e = sample();
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["event_type"], "permission_request");
        assert_eq!(json["agent"], "claude");
        assert_eq!(json["severity"], "high");
        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn severity_is_ordered() {
        assert!(Severity::Urgent > Severity::High);
        assert!(Severity::High > Severity::Normal);
        assert!(Severity::Normal > Severity::Low);
        assert_eq!("URGENT".parse::<Severity>().unwrap(), Severity::Urgent);
        assert!("loud".parse::<Severity>().is_err());
    }

    #[test]
    fn dedup_key_is_stable_and_content_sensitive() {
        let a = sample();
        let mut b = a.clone();
        b.id = Uuid::now_v7();
        b.occurred_at = a.occurred_at + chrono::Duration::seconds(1);
        assert_eq!(
            a.dedup_key(),
            b.dedup_key(),
            "id/time must not affect dedup"
        );
        b.message = Some("different".into());
        assert_ne!(a.dedup_key(), b.dedup_key());
        let mut c = a.clone();
        c.session_id = Some("other".into());
        assert_ne!(a.dedup_key(), c.dedup_key());
    }

    #[test]
    fn validation_rejects_oversized_and_future_schema() {
        let mut e = sample();
        assert!(e.validate().is_ok());
        e.message = Some("x".repeat(MAX_MESSAGE_BYTES + 1));
        assert_eq!(e.validate(), Err(EventValidationError::TooLarge("message")));
        let mut e = sample();
        e.schema_version = 99;
        assert_eq!(
            e.validate(),
            Err(EventValidationError::UnsupportedSchema(99))
        );
    }

    #[test]
    fn session_short_handles_short_ids() {
        let mut e = sample();
        assert_eq!(e.session_short(), Some("abc123de"));
        e.session_id = Some("abc".into());
        assert_eq!(e.session_short(), Some("abc"));
    }
}
