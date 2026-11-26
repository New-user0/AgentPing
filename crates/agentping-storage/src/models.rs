//! Records returned by the storage layer.

use std::fmt;
use std::str::FromStr;

use agentping_core::{Event, EventType};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Delivery state of a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationStatus {
    /// Waiting to be sent (possibly scheduled for later).
    Pending,
    /// Claimed by a worker and currently being sent.
    Sending,
    /// Accepted by the provider.
    Delivered,
    /// Gave up after a permanent error or exhausting retries.
    Failed,
    /// Cancelled before sending (for example, answered locally).
    Cancelled,
}

impl NotificationStatus {
    /// Stable identifier stored in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            NotificationStatus::Pending => "pending",
            NotificationStatus::Sending => "sending",
            NotificationStatus::Delivered => "delivered",
            NotificationStatus::Failed => "failed",
            NotificationStatus::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for NotificationStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for NotificationStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "pending" => Self::Pending,
            "sending" => Self::Sending,
            "delivered" => Self::Delivered,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            other => return Err(format!("unknown notification status '{other}'")),
        })
    }
}

/// Outcome of inserting an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertOutcome {
    /// The event was stored.
    Inserted,
    /// An identical event was received recently; the existing id is returned.
    Duplicate(String),
}

/// A stored event with its processing metadata and delivery summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEvent {
    /// The normalized event.
    pub event: Event,
    /// When AgentPing received it.
    pub received_at: DateTime<Utc>,
    /// When rules were evaluated for it.
    pub processed_at: Option<DateTime<Utc>>,
    /// Rules decision summary.
    pub decision: Option<String>,
    /// Per-provider delivery state.
    pub deliveries: Vec<NotificationRecord>,
}

/// A notification row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationRecord {
    /// Notification id.
    pub id: String,
    /// Event id.
    pub event_id: String,
    /// Provider name.
    pub provider: String,
    /// Delivery state.
    pub status: NotificationStatus,
    /// Attempts made so far.
    pub attempts: u32,
    /// Next scheduled attempt.
    pub next_attempt_at: Option<DateTime<Utc>>,
    /// Last (redacted) error.
    pub last_error: Option<String>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Delivery time.
    pub delivered_at: Option<DateTime<Utc>>,
}

/// A delivery attempt row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryAttemptRecord {
    /// When the attempt happened.
    pub attempted_at: DateTime<Utc>,
    /// `delivered`, `retry` or `failed`.
    pub outcome: String,
    /// HTTP status, when one was received.
    pub http_status: Option<u16>,
    /// Redacted error message.
    pub error: Option<String>,
    /// Request duration.
    pub duration_ms: u64,
}

/// A notification claimed for delivery, together with its event.
#[derive(Debug, Clone)]
pub struct ClaimedNotification {
    /// Notification id.
    pub id: String,
    /// Provider to deliver through.
    pub provider: String,
    /// Attempts made before this one.
    pub attempts: u32,
    /// The event to render.
    pub event: Event,
}

/// Result of one delivery attempt, as recorded in the queue.
#[derive(Debug, Clone)]
pub enum AttemptOutcome {
    /// Delivered successfully.
    Delivered {
        /// HTTP status.
        http_status: Option<u16>,
    },
    /// Failed; try again at `retry_at`.
    Retry {
        /// When to retry.
        retry_at: DateTime<Utc>,
        /// HTTP status.
        http_status: Option<u16>,
        /// Redacted error.
        error: String,
    },
    /// Failed permanently.
    Failed {
        /// HTTP status.
        http_status: Option<u16>,
        /// Redacted error.
        error: String,
    },
}

/// A tracked agent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    /// Agent id.
    pub agent: String,
    /// Session id.
    pub session_id: String,
    /// Project name.
    pub project_name: Option<String>,
    /// Project path.
    pub project_path: Option<String>,
    /// First event time.
    pub started_at: DateTime<Utc>,
    /// Latest event time.
    pub last_activity_at: DateTime<Utc>,
    /// Derived status.
    pub status: SessionStatus,
    /// Latest event type.
    pub last_event_type: Option<String>,
    /// Number of events.
    pub event_count: u64,
}

/// Derived session status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// Session started.
    Active,
    /// Agent is working.
    Working,
    /// Waiting for a permission decision.
    WaitingForPermission,
    /// Waiting for an answer or input.
    WaitingForInput,
    /// Finished its turn; waiting for the next prompt.
    Completed,
    /// Stopped because of an error.
    Error,
    /// Session ended.
    Ended,
}

impl SessionStatus {
    /// Stable identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            SessionStatus::Active => "active",
            SessionStatus::Working => "working",
            SessionStatus::WaitingForPermission => "waiting_for_permission",
            SessionStatus::WaitingForInput => "waiting_for_input",
            SessionStatus::Completed => "completed",
            SessionStatus::Error => "error",
            SessionStatus::Ended => "ended",
        }
    }

    /// Human readable label.
    pub fn label(self) -> &'static str {
        match self {
            SessionStatus::Active => "Active",
            SessionStatus::Working => "Working",
            SessionStatus::WaitingForPermission => "Waiting for permission",
            SessionStatus::WaitingForInput => "Waiting for input",
            SessionStatus::Completed => "Finished, waiting for next prompt",
            SessionStatus::Error => "Stopped with an error",
            SessionStatus::Ended => "Ended",
        }
    }

    /// Status implied by an event, or `None` if the event does not change status.
    pub fn from_event(event_type: &EventType) -> Option<Self> {
        Some(match event_type {
            EventType::PermissionRequest => SessionStatus::WaitingForPermission,
            EventType::UserQuestion | EventType::AgentIdle => SessionStatus::WaitingForInput,
            EventType::TaskCompleted => SessionStatus::Completed,
            EventType::TaskFailed => SessionStatus::Error,
            EventType::AgentStarted => SessionStatus::Active,
            EventType::AgentStopped => SessionStatus::Ended,
            EventType::Activity => SessionStatus::Working,
            EventType::Notification | EventType::Custom(_) => return None,
        })
    }
}

impl FromStr for SessionStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "active" => Self::Active,
            "working" => Self::Working,
            "waiting_for_permission" => Self::WaitingForPermission,
            "waiting_for_input" => Self::WaitingForInput,
            "completed" => Self::Completed,
            "error" => Self::Error,
            "ended" => Self::Ended,
            other => return Err(format!("unknown session status '{other}'")),
        })
    }
}

/// Filters for listing events.
#[derive(Debug, Clone, Default)]
pub struct EventQuery {
    /// Only events received at or after this time.
    pub since: Option<DateTime<Utc>>,
    /// Agent id.
    pub agent: Option<String>,
    /// Project name (case-insensitive).
    pub project: Option<String>,
    /// Event type identifier.
    pub event_type: Option<String>,
    /// Session id prefix.
    pub session: Option<String>,
    /// Maximum rows (defaults to 50).
    pub limit: Option<u32>,
    /// Rows to skip.
    pub offset: Option<u32>,
}

/// Aggregate counters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stats {
    /// Stored events.
    pub events: u64,
    /// Events received in the last 24 hours.
    pub events_24h: u64,
    /// Delivered notifications.
    pub delivered: u64,
    /// Pending or in-flight notifications.
    pub pending: u64,
    /// Failed notifications.
    pub failed: u64,
    /// Cancelled notifications.
    pub cancelled: u64,
    /// Events still waiting for rule evaluation.
    pub unprocessed_events: u64,
}

/// A notification to enqueue.
#[derive(Debug, Clone)]
pub struct NewNotification<'a> {
    /// Event id.
    pub event_id: &'a str,
    /// Provider name.
    pub provider: &'a str,
    /// First attempt time.
    pub not_before: DateTime<Utc>,
    /// Optional cancellation key.
    pub cancel_key: Option<&'a str>,
}
