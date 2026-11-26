//! Remote interactions (prompts awaiting an answer from a remote channel) and small key/value
//! state.

use std::fmt;
use std::str::FromStr;

use agentping_core::time::format_ts;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::repo::{opt_ts, ts};
use crate::{Result, StorageError, Store};

/// What a remote interaction is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionKind {
    /// Approve or deny a tool permission request.
    Permission,
    /// Answer an agent's multiple-choice question.
    Question,
    /// Send follow-up instructions after the agent stopped.
    Continue,
}

impl InteractionKind {
    /// Stable identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            InteractionKind::Permission => "permission",
            InteractionKind::Question => "question",
            InteractionKind::Continue => "continue",
        }
    }
}

impl fmt::Display for InteractionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for InteractionKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "permission" => Self::Permission,
            "question" => Self::Question,
            "continue" => Self::Continue,
            other => return Err(format!("unknown interaction kind '{other}'")),
        })
    }
}

/// Lifecycle state of an interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionStatus {
    /// Waiting for an answer.
    Pending,
    /// Answered remotely.
    Resolved,
    /// Withdrawn (answered locally, superseded, or the waiting hook gave up).
    Cancelled,
    /// Not answered before it expired.
    Expired,
}

impl InteractionStatus {
    /// Stable identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            InteractionStatus::Pending => "pending",
            InteractionStatus::Resolved => "resolved",
            InteractionStatus::Cancelled => "cancelled",
            InteractionStatus::Expired => "expired",
        }
    }
}

impl FromStr for InteractionStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "pending" => Self::Pending,
            "resolved" => Self::Resolved,
            "cancelled" => Self::Cancelled,
            "expired" => Self::Expired,
            other => return Err(format!("unknown interaction status '{other}'")),
        })
    }
}

/// A stored interaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionRecord {
    /// Random unguessable id (also used in remote button ids).
    pub id: String,
    /// Kind.
    pub kind: InteractionKind,
    /// Agent id.
    pub agent: String,
    /// Agent session id.
    pub session_id: Option<String>,
    /// Project name.
    pub project_name: Option<String>,
    /// State.
    pub status: InteractionStatus,
    /// What is being asked (tool details, questions…).
    pub request: Value,
    /// The answer (or partial answers while pending).
    pub response: Option<Value>,
    /// Who answered (remote user id and name).
    pub responder: Option<String>,
    /// Remote message carrying the prompt.
    pub remote_message_id: Option<String>,
    /// Free-form note (why it was cancelled, …).
    pub note: Option<String>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Expiry time.
    pub expires_at: DateTime<Utc>,
    /// Resolution time.
    pub resolved_at: Option<DateTime<Utc>>,
}

/// A new interaction.
#[derive(Debug, Clone)]
pub struct NewInteraction<'a> {
    /// Random id.
    pub id: &'a str,
    /// Kind.
    pub kind: InteractionKind,
    /// Agent id.
    pub agent: &'a str,
    /// Session id.
    pub session_id: Option<&'a str>,
    /// Project name.
    pub project_name: Option<&'a str>,
    /// Request payload.
    pub request: &'a Value,
    /// Expiry.
    pub expires_at: DateTime<Utc>,
}

fn decode(row: &Row<'_>) -> Result<InteractionRecord> {
    let kind: String = row.get("kind")?;
    let status: String = row.get("status")?;
    let request: String = row.get("request_json")?;
    let response: Option<String> = row.get("response_json")?;
    Ok(InteractionRecord {
        id: row.get("id")?,
        kind: kind.parse().map_err(StorageError::Corrupt)?,
        agent: row.get("agent")?,
        session_id: row.get("session_id")?,
        project_name: row.get("project_name")?,
        status: status.parse().map_err(StorageError::Corrupt)?,
        request: serde_json::from_str(&request)
            .map_err(|e| StorageError::Corrupt(format!("interaction request: {e}")))?,
        response: response
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| StorageError::Corrupt(format!("interaction response: {e}")))?,
        responder: row.get("responder")?,
        remote_message_id: row.get("remote_message_id")?,
        note: row.get("note")?,
        created_at: ts(row, "created_at")?,
        expires_at: ts(row, "expires_at")?,
        resolved_at: opt_ts(row, "resolved_at")?,
    })
}

impl Store<'_> {
    fn query_interactions(
        &self,
        sql: &str,
        params: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<InteractionRecord>> {
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query(params)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(decode(row)?);
        }
        Ok(out)
    }

    /// Stores a new pending interaction.
    pub fn create_interaction(&self, n: &NewInteraction<'_>, now: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO interactions (id, kind, agent, session_id, project_name, status,
                request_json, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7, ?8)",
            params![
                n.id,
                n.kind.as_str(),
                n.agent,
                n.session_id,
                n.project_name,
                n.request.to_string(),
                format_ts(now),
                format_ts(n.expires_at),
            ],
        )?;
        Ok(())
    }

    /// Looks up an interaction.
    pub fn interaction(&self, id: &str) -> Result<Option<InteractionRecord>> {
        Ok(self
            .query_interactions("SELECT * FROM interactions WHERE id = ?1", &[&id])?
            .pop())
    }

    /// The interaction whose prompt is the given remote message.
    pub fn interaction_by_message(&self, message_id: &str) -> Result<Option<InteractionRecord>> {
        Ok(self
            .query_interactions(
                "SELECT * FROM interactions WHERE remote_message_id = ?1 ORDER BY created_at DESC LIMIT 1",
                &[&message_id],
            )?
            .pop())
    }

    /// Pending, unexpired interactions, newest first.
    pub fn pending_interactions(&self, now: DateTime<Utc>) -> Result<Vec<InteractionRecord>> {
        self.query_interactions(
            "SELECT * FROM interactions WHERE status = 'pending' AND expires_at > ?1
             ORDER BY created_at DESC",
            &[&format_ts(now)],
        )
    }

    /// Records the remote message that carries the prompt.
    pub fn set_interaction_message(&self, id: &str, message_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE interactions SET remote_message_id = ?2 WHERE id = ?1",
            params![id, message_id],
        )?;
        Ok(())
    }

    /// Stores partial answers on a still-pending interaction.
    pub fn set_interaction_progress(&self, id: &str, response: &Value) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE interactions SET response_json = ?2 WHERE id = ?1 AND status = 'pending'",
            params![id, response.to_string()],
        )?;
        Ok(n == 1)
    }

    /// Resolves a pending, unexpired interaction exactly once. Returns false when it was already
    /// answered, withdrawn or expired (replays are rejected).
    pub fn resolve_interaction(
        &self,
        id: &str,
        response: &Value,
        responder: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE interactions SET status = 'resolved', response_json = ?2, responder = ?3,
                resolved_at = ?4
             WHERE id = ?1 AND status = 'pending' AND expires_at > ?4",
            params![id, response.to_string(), responder, format_ts(now)],
        )?;
        Ok(n == 1)
    }

    /// Withdraws a pending interaction. Returns the record if it was pending.
    pub fn close_interaction(
        &self,
        id: &str,
        status: InteractionStatus,
        note: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<InteractionRecord>> {
        let n = self.conn.execute(
            "UPDATE interactions SET status = ?2, note = ?3, resolved_at = ?4
             WHERE id = ?1 AND status = 'pending'",
            params![id, status.as_str(), note, format_ts(now)],
        )?;
        if n == 1 {
            self.interaction(id)
        } else {
            Ok(None)
        }
    }

    /// Withdraws every pending interaction of a session (optionally only of `kind`).
    pub fn close_session_interactions(
        &self,
        agent: &str,
        session_id: &str,
        kind: Option<InteractionKind>,
        note: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<InteractionRecord>> {
        let pending = self.query_interactions(
            "SELECT * FROM interactions WHERE agent = ?1 AND session_id = ?2 AND status = 'pending'",
            &[&agent, &session_id],
        )?;
        let mut closed = Vec::new();
        for i in pending
            .into_iter()
            .filter(|i| kind.is_none_or(|k| k == i.kind))
        {
            if let Some(r) =
                self.close_interaction(&i.id, InteractionStatus::Cancelled, note, now)?
            {
                closed.push(r);
            }
        }
        Ok(closed)
    }

    /// Marks overdue pending interactions as expired and returns them.
    pub fn expire_interactions(&self, now: DateTime<Utc>) -> Result<Vec<InteractionRecord>> {
        let due = self.query_interactions(
            "SELECT * FROM interactions WHERE status = 'pending' AND expires_at <= ?1",
            &[&format_ts(now)],
        )?;
        let mut out = Vec::new();
        for i in due {
            if let Some(r) =
                self.close_interaction(&i.id, InteractionStatus::Expired, "no answer in time", now)?
            {
                out.push(r);
            }
        }
        Ok(out)
    }

    /// Recent interactions (newest first) for diagnostics and the audit trail.
    pub fn recent_interactions(&self, limit: u32) -> Result<Vec<InteractionRecord>> {
        self.query_interactions(
            "SELECT * FROM interactions ORDER BY created_at DESC LIMIT ?1",
            &[&limit],
        )
    }

    /// Records the provider message ids of a delivered notification.
    pub fn set_notification_external_id(
        &self,
        notification_id: &str,
        external_id: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE notifications SET external_id = ?2 WHERE id = ?1",
            params![notification_id, external_id],
        )?;
        Ok(())
    }

    /// Finds the session of the event whose notification produced provider message `message_id`.
    pub fn session_for_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Result<Option<(String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT e.agent, e.session_id FROM notifications n JOIN events e ON e.id = n.event_id
                 WHERE n.provider = ?1 AND e.session_id IS NOT NULL
                   AND (' ' || n.external_id || ' ') LIKE ('% ' || ?2 || ' %')
                 ORDER BY n.created_at DESC LIMIT 1",
                params![provider, message_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// Reads a key/value entry.
    pub fn kv_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Writes a key/value entry.
    pub fn kv_set(&self, key: &str, value: &str, now: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value, format_ts(now)],
        )?;
        Ok(())
    }

    /// Time of the most recent local user activity (a submitted prompt) across sessions.
    pub fn last_local_activity(&self) -> Result<Option<DateTime<Utc>>> {
        let raw: Option<String> = self.conn.query_row(
            "SELECT MAX(received_at) FROM events WHERE native_event = 'UserPromptSubmit'",
            [],
            |r| r.get(0),
        )?;
        Ok(raw.as_deref().and_then(agentping_core::time::parse_ts))
    }
}

impl Store<'_> {
    /// Whether an interaction of `kind` was created for the session since `since`.
    pub fn has_recent_interaction(
        &self,
        agent: &str,
        session_id: &str,
        kind: InteractionKind,
        since: DateTime<Utc>,
    ) -> Result<bool> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM interactions WHERE agent = ?1 AND session_id = ?2 AND kind = ?3
                   AND created_at >= ?4 LIMIT 1",
                params![agent, session_id, kind.as_str(), format_ts(since)],
                |r| r.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// The most recent event of a session that came from `native_event`, since `since`.
    pub fn latest_native_event(
        &self,
        agent: &str,
        session_id: &str,
        native_event: &str,
        since: DateTime<Utc>,
    ) -> Result<Option<agentping_core::Event>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT event_json FROM events WHERE agent = ?1 AND session_id = ?2
                   AND native_event = ?3 AND received_at >= ?4
                 ORDER BY received_at DESC LIMIT 1",
                params![agent, session_id, native_event, format_ts(since)],
                |r| r.get(0),
            )
            .optional()?;
        json.as_deref().map(crate::repo::decode_event).transpose()
    }
}
