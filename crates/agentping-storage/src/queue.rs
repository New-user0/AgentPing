//! Durable notification queue: enqueue, lease-based claiming, attempt recording,
//! cancellation and crash recovery.

use agentping_core::time::format_ts;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use crate::models::*;
use crate::repo::{decode_event, opt_ts, ts};
use crate::{Result, StorageError, Store};

impl Store<'_> {
    /// Enqueues a notification. Returns false if one already exists for this event/provider.
    pub fn enqueue(&self, n: &NewNotification<'_>, now: DateTime<Utc>) -> Result<bool> {
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO notifications (id, event_id, provider, status, attempts,
                next_attempt_at, cancel_key, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'pending', 0, ?4, ?5, ?6, ?6)",
            params![
                Uuid::now_v7().to_string(),
                n.event_id,
                n.provider,
                format_ts(n.not_before),
                n.cancel_key,
                format_ts(now),
            ],
        )?;
        Ok(inserted == 1)
    }

    /// Atomically claims up to `limit` due notifications, leasing them until `lease_until`.
    ///
    /// A claimed notification that is never resolved (for example because the process
    /// crashed) becomes claimable again once its lease expires.
    pub fn claim_due(
        &self,
        now: DateTime<Utc>,
        lease_until: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<ClaimedNotification>> {
        let now_s = format_ts(now);
        self.conn.execute(
            "UPDATE notifications SET status = 'pending', lease_until = NULL, updated_at = ?1
             WHERE status = 'sending' AND lease_until < ?1",
            params![now_s],
        )?;
        let mut stmt = self.conn.prepare(
            "UPDATE notifications SET status = 'sending', lease_until = ?2, updated_at = ?1
             WHERE id IN (
                SELECT id FROM notifications
                WHERE status = 'pending' AND next_attempt_at <= ?1
                ORDER BY next_attempt_at LIMIT ?3)
             RETURNING id, event_id, provider, attempts",
        )?;
        let claimed: Vec<(String, String, String, u32)> = stmt
            .query_map(params![now_s, format_ts(lease_until), limit], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        let mut out = Vec::with_capacity(claimed.len());
        for (id, event_id, provider, attempts) in claimed {
            let json: Option<String> = self
                .conn
                .query_row(
                    "SELECT event_json FROM events WHERE id = ?1",
                    params![event_id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(json) = json else {
                return Err(StorageError::Corrupt(format!(
                    "notification {id} references missing event {event_id}"
                )));
            };
            out.push(ClaimedNotification {
                id,
                provider,
                attempts,
                event: decode_event(&json)?,
            });
        }
        Ok(out)
    }

    /// Records the outcome of an attempt on a claimed notification.
    pub fn record_attempt(
        &self,
        notification_id: &str,
        outcome: &AttemptOutcome,
        attempted_at: DateTime<Utc>,
        duration_ms: u64,
    ) -> Result<()> {
        let at = format_ts(attempted_at);
        let (label, http_status, error) = match outcome {
            AttemptOutcome::Delivered { http_status, .. } => ("delivered", *http_status, None),
            AttemptOutcome::Retry {
                http_status, error, ..
            } => ("retry", *http_status, Some(error.as_str())),
            AttemptOutcome::Failed { http_status, error } => {
                ("failed", *http_status, Some(error.as_str()))
            }
        };
        self.conn.execute(
            "INSERT INTO delivery_attempts (notification_id, attempted_at, outcome, http_status, error, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![notification_id, at, label, http_status, error, duration_ms as i64],
        )?;
        match outcome {
            AttemptOutcome::Delivered { external_id, .. } => self.conn.execute(
                "UPDATE notifications SET status = 'delivered', attempts = attempts + 1,
                    lease_until = NULL, last_error = NULL, delivered_at = ?2, updated_at = ?2,
                    external_id = COALESCE(?3, external_id)
                 WHERE id = ?1",
                params![notification_id, at, external_id],
            )?,
            AttemptOutcome::Retry {
                retry_at, error, ..
            } => self.conn.execute(
                "UPDATE notifications SET status = 'pending', attempts = attempts + 1,
                    lease_until = NULL, last_error = ?3, next_attempt_at = ?4, updated_at = ?2
                 WHERE id = ?1",
                params![notification_id, at, error, format_ts(*retry_at)],
            )?,
            AttemptOutcome::Failed { error, .. } => self.conn.execute(
                "UPDATE notifications SET status = 'failed', attempts = attempts + 1,
                    lease_until = NULL, last_error = ?3, next_attempt_at = NULL, updated_at = ?2
                 WHERE id = ?1",
                params![notification_id, at, error],
            )?,
        };
        Ok(())
    }

    /// Returns leased notifications to the queue without counting an attempt (graceful shutdown).
    pub fn release(&self, notification_ids: &[String], now: DateTime<Utc>) -> Result<()> {
        for id in notification_ids {
            self.conn.execute(
                "UPDATE notifications SET status = 'pending', lease_until = NULL, updated_at = ?2
                 WHERE id = ?1 AND status = 'sending'",
                params![id, format_ts(now)],
            )?;
        }
        Ok(())
    }

    /// Cancels pending notifications registered under `cancel_key`. Returns how many.
    pub fn cancel_pending(
        &self,
        cancel_key: &str,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE notifications SET status = 'cancelled', last_error = ?2, next_attempt_at = NULL,
                updated_at = ?3
             WHERE cancel_key = ?1 AND status = 'pending'",
            params![cancel_key, reason, format_ts(now)],
        )?)
    }

    /// Earliest scheduled attempt among pending notifications.
    pub fn next_due_at(&self) -> Result<Option<DateTime<Utc>>> {
        let raw: Option<String> = self.conn.query_row(
            "SELECT MIN(next_attempt_at) FROM notifications WHERE status IN ('pending', 'sending')",
            [],
            |r| r.get(0),
        )?;
        Ok(raw.as_deref().and_then(agentping_core::time::parse_ts))
    }

    /// Notifications for an event.
    pub fn notifications_for_event(&self, event_id: &str) -> Result<Vec<NotificationRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM notifications WHERE event_id = ?1 ORDER BY provider")?;
        let mut rows = stmt.query(params![event_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let status: String = row.get("status")?;
            out.push(NotificationRecord {
                id: row.get("id")?,
                event_id: row.get("event_id")?,
                provider: row.get("provider")?,
                status: status.parse().map_err(StorageError::Corrupt)?,
                attempts: row.get("attempts")?,
                next_attempt_at: opt_ts(row, "next_attempt_at")?,
                last_error: row.get("last_error")?,
                created_at: ts(row, "created_at")?,
                delivered_at: opt_ts(row, "delivered_at")?,
            });
        }
        Ok(out)
    }

    /// Delivery attempts for a notification, oldest first.
    pub fn attempts_for_notification(
        &self,
        notification_id: &str,
    ) -> Result<Vec<DeliveryAttemptRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM delivery_attempts WHERE notification_id = ?1 ORDER BY id")?;
        let mut rows = stmt.query(params![notification_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(DeliveryAttemptRecord {
                attempted_at: ts(row, "attempted_at")?,
                outcome: row.get("outcome")?,
                http_status: row.get("http_status")?,
                error: row.get("error")?,
                duration_ms: row.get::<_, i64>("duration_ms")?.max(0) as u64,
            });
        }
        Ok(out)
    }
}
