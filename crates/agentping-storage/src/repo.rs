//! Event and session repository operations.

use agentping_core::time::{format_ts, parse_ts};
use agentping_core::Event;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Row};

use crate::models::*;
use crate::{Result, StorageError, Store};

pub(crate) fn ts(row: &Row<'_>, idx: &str) -> Result<DateTime<Utc>> {
    let raw: String = row.get(idx)?;
    parse_ts(&raw).ok_or_else(|| StorageError::Corrupt(format!("bad timestamp '{raw}' in {idx}")))
}

pub(crate) fn opt_ts(row: &Row<'_>, idx: &str) -> Result<Option<DateTime<Utc>>> {
    let raw: Option<String> = row.get(idx)?;
    Ok(raw.as_deref().and_then(parse_ts))
}

pub(crate) fn decode_event(json: &str) -> Result<Event> {
    serde_json::from_str(json).map_err(|e| StorageError::Corrupt(format!("event json: {e}")))
}

impl Store<'_> {
    /// Stores `event` unless an event with the same dedup key was received within `window`.
    ///
    /// Call inside [`crate::Database::write`] so the check and insert are atomic.
    pub fn insert_event(
        &self,
        event: &Event,
        received_at: DateTime<Utc>,
        window: chrono::Duration,
    ) -> Result<InsertOutcome> {
        let key = event.dedup_key();
        let since = format_ts(received_at - window);
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM events WHERE dedup_key = ?1 AND received_at >= ?2 LIMIT 1",
                params![key, since],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok(InsertOutcome::Duplicate(id));
        }
        let json = serde_json::to_string(event)
            .map_err(|e| StorageError::Corrupt(format!("encode event: {e}")))?;
        self.conn.execute(
            "INSERT INTO events (id, dedup_key, schema_version, event_type, agent, session_id,
                project_name, project_path, severity, title, message, native_event, event_json,
                occurred_at, received_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                event.id.to_string(),
                key,
                event.schema_version,
                event.event_type.as_str(),
                event.agent.as_str(),
                event.session_id,
                event.project_name,
                event.project_path,
                event.severity.as_str(),
                event.title,
                event.message,
                event.source.native_event,
                json,
                format_ts(event.occurred_at),
                format_ts(received_at),
            ],
        )?;
        Ok(InsertOutcome::Inserted)
    }

    /// Marks an event as processed and records the rules decision.
    pub fn mark_processed(&self, event_id: &str, decision: &str, at: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "UPDATE events SET processed_at = ?2, decision = ?3 WHERE id = ?1",
            params![event_id, format_ts(at), decision],
        )?;
        Ok(())
    }

    /// Events that were stored but not yet evaluated (e.g. stored by a hook while offline).
    pub fn unprocessed_events(&self, limit: u32) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT event_json FROM events WHERE processed_at IS NULL ORDER BY received_at LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for json in rows {
            out.push(decode_event(&json?)?);
        }
        Ok(out)
    }

    /// Whether a notification for the same agent/session/event type was queued or delivered
    /// since `since` (used for repeat suppression).
    pub fn recently_notified(&self, event: &Event, since: DateTime<Utc>) -> Result<bool> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM events e JOIN notifications n ON n.event_id = e.id
                 WHERE e.agent = ?1 AND e.session_id IS ?2 AND e.event_type = ?3
                   AND e.received_at >= ?4 AND e.id != ?5
                   AND n.status IN ('pending', 'sending', 'delivered')
                 LIMIT 1",
                params![
                    event.agent.as_str(),
                    event.session_id,
                    event.event_type.as_str(),
                    format_ts(since),
                    event.id.to_string(),
                ],
                |r| r.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// Updates (or creates) the session an event belongs to.
    pub fn touch_session(&self, event: &Event) -> Result<()> {
        let Some(session_id) = event.session_id.as_deref() else {
            return Ok(());
        };
        let at = format_ts(event.occurred_at);
        let status = SessionStatus::from_event(&event.event_type);
        self.conn.execute(
            "INSERT INTO sessions (agent, session_id, project_name, project_path, started_at,
                last_activity_at, status, last_event_type, event_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, 1)
             ON CONFLICT (agent, session_id) DO UPDATE SET
                project_name = COALESCE(excluded.project_name, sessions.project_name),
                project_path = COALESCE(excluded.project_path, sessions.project_path),
                last_activity_at = MAX(sessions.last_activity_at, excluded.last_activity_at),
                status = CASE WHEN ?8 THEN excluded.status ELSE sessions.status END,
                last_event_type = excluded.last_event_type,
                event_count = sessions.event_count + 1",
            params![
                event.agent.as_str(),
                session_id,
                event.project_name,
                event.project_path,
                at,
                status.unwrap_or(SessionStatus::Active).as_str(),
                event.event_type.as_str(),
                status.is_some(),
            ],
        )?;
        Ok(())
    }

    /// Sessions active since `since`, most recent first. Ended sessions are excluded unless
    /// `include_ended` is set.
    pub fn sessions(
        &self,
        since: DateTime<Utc>,
        include_ended: bool,
    ) -> Result<Vec<SessionRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM sessions WHERE last_activity_at >= ?1
               AND (?2 OR status != 'ended')
             ORDER BY last_activity_at DESC LIMIT 100",
        )?;
        let mut rows = stmt.query(params![format_ts(since), include_ended])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let status: String = row.get("status")?;
            out.push(SessionRecord {
                agent: row.get("agent")?,
                session_id: row.get("session_id")?,
                project_name: row.get("project_name")?,
                project_path: row.get("project_path")?,
                started_at: ts(row, "started_at")?,
                last_activity_at: ts(row, "last_activity_at")?,
                status: status.parse().map_err(StorageError::Corrupt)?,
                last_event_type: row.get("last_event_type")?,
                event_count: row.get::<_, i64>("event_count")?.max(0) as u64,
            });
        }
        Ok(out)
    }

    /// Lists events matching `query`, newest first, with their deliveries.
    pub fn list_events(&self, query: &EventQuery) -> Result<Vec<StoredEvent>> {
        let mut sql = String::from(
            "SELECT event_json, received_at, processed_at, decision FROM events WHERE 1 = 1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(since) = query.since {
            args.push(Box::new(format_ts(since)));
            sql.push_str(&format!(" AND received_at >= ?{}", args.len()));
        }
        if let Some(agent) = &query.agent {
            args.push(Box::new(agent.clone()));
            sql.push_str(&format!(" AND agent = ?{}", args.len()));
        }
        if let Some(project) = &query.project {
            args.push(Box::new(project.clone()));
            sql.push_str(&format!(
                " AND project_name = ?{} COLLATE NOCASE",
                args.len()
            ));
        }
        if let Some(event_type) = &query.event_type {
            args.push(Box::new(event_type.clone()));
            sql.push_str(&format!(" AND event_type = ?{}", args.len()));
        }
        if let Some(session) = &query.session {
            args.push(Box::new(format!("{}%", escape_like(session))));
            sql.push_str(&format!(" AND session_id LIKE ?{} ESCAPE '\\'", args.len()));
        }
        args.push(Box::new(query.limit.unwrap_or(50).min(1000)));
        sql.push_str(&format!(" ORDER BY received_at DESC LIMIT ?{}", args.len()));
        args.push(Box::new(query.offset.unwrap_or(0)));
        sql.push_str(&format!(" OFFSET ?{}", args.len()));

        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let mut rows = stmt.query(params.as_slice())?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let json: String = row.get("event_json")?;
            let event = decode_event(&json)?;
            out.push(StoredEvent {
                received_at: ts(row, "received_at")?,
                processed_at: opt_ts(row, "processed_at")?,
                decision: row.get("decision")?,
                deliveries: Vec::new(),
                event,
            });
        }
        drop(rows);
        for stored in &mut out {
            stored.deliveries = self.notifications_for_event(&stored.event.id.to_string())?;
        }
        Ok(out)
    }

    /// Finds an event by full id or unique id prefix (at least 4 characters).
    pub fn find_event(&self, id_or_prefix: &str) -> Result<Option<StoredEvent>> {
        if id_or_prefix.len() < 4 {
            return Ok(None);
        }
        let mut stmt = self.conn.prepare(
            "SELECT event_json, received_at, processed_at, decision FROM events
             WHERE id LIKE ?1 ESCAPE '\\' LIMIT 2",
        )?;
        let mut rows = stmt.query(params![format!("{}%", escape_like(id_or_prefix))])?;
        let mut found = Vec::new();
        while let Some(row) = rows.next()? {
            let json: String = row.get("event_json")?;
            found.push(StoredEvent {
                event: decode_event(&json)?,
                received_at: ts(row, "received_at")?,
                processed_at: opt_ts(row, "processed_at")?,
                decision: row.get("decision")?,
                deliveries: Vec::new(),
            });
        }
        if found.len() != 1 {
            return Ok(None);
        }
        let mut stored = found.remove(0);
        stored.deliveries = self.notifications_for_event(&stored.event.id.to_string())?;
        Ok(Some(stored))
    }

    /// Aggregate counters.
    pub fn stats(&self, now: DateTime<Utc>) -> Result<Stats> {
        let count = |sql: &str, p: &[&dyn rusqlite::ToSql]| -> Result<u64> {
            let n: i64 = self.conn.query_row(sql, p, |r| r.get(0))?;
            Ok(n.max(0) as u64)
        };
        let day_ago = format_ts(now - chrono::Duration::hours(24));
        Ok(Stats {
            events: count("SELECT COUNT(*) FROM events", &[])?,
            events_24h: count(
                "SELECT COUNT(*) FROM events WHERE received_at >= ?1",
                &[&day_ago],
            )?,
            delivered: count(
                "SELECT COUNT(*) FROM notifications WHERE status = 'delivered'",
                &[],
            )?,
            pending: count(
                "SELECT COUNT(*) FROM notifications WHERE status IN ('pending', 'sending')",
                &[],
            )?,
            failed: count(
                "SELECT COUNT(*) FROM notifications WHERE status = 'failed'",
                &[],
            )?,
            cancelled: count(
                "SELECT COUNT(*) FROM notifications WHERE status = 'cancelled'",
                &[],
            )?,
            unprocessed_events: count(
                "SELECT COUNT(*) FROM events WHERE processed_at IS NULL",
                &[],
            )?,
        })
    }

    /// Deletes events (and their notifications) received before `cutoff`, keeping any event
    /// that still has undelivered notifications. Returns the number of events removed.
    pub fn prune(&self, cutoff: DateTime<Utc>) -> Result<usize> {
        let cutoff = format_ts(cutoff);
        let removed = self.conn.execute(
            "DELETE FROM events WHERE received_at < ?1
               AND id NOT IN (SELECT event_id FROM notifications WHERE status IN ('pending', 'sending'))",
            params![cutoff],
        )?;
        self.conn.execute(
            "DELETE FROM sessions WHERE last_activity_at < ?1",
            params![cutoff],
        )?;
        Ok(removed)
    }

    /// Deletes all events, sessions and delivery history. Returns the number of events removed.
    pub fn clear_all(&self) -> Result<usize> {
        let removed = self.conn.execute("DELETE FROM events", [])?;
        self.conn.execute("DELETE FROM sessions", [])?;
        Ok(removed)
    }
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}
