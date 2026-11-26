//! The event pipeline: dedup → session tracking → cancellation → rules → enqueue.
//!
//! The engine is synchronous and storage-backed, so the same code runs inside the daemon and
//! inside a hook process when the daemon is offline.

use agentping_core::{Config, Event};
use agentping_rules::{Decision, RuleContext, RulesEngine};
use agentping_storage::{Database, InsertOutcome, NewNotification, Store};
use chrono::{DateTime, Local, Utc};
use serde::Serialize;

/// Window in which identical events are treated as duplicates.
pub const DEDUP_WINDOW_SECS: i64 = 30;

/// Result of ingesting an event.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum IngestOutcome {
    /// The event was stored and evaluated.
    Accepted {
        /// Event id.
        id: String,
        /// Rules decision.
        decision: Decision,
        /// Notifications enqueued.
        queued: usize,
        /// Pending notifications cancelled by this event.
        cancelled: usize,
    },
    /// The event duplicated a recent one and was ignored.
    Duplicate {
        /// Id of the original event.
        existing_id: String,
    },
}

impl IngestOutcome {
    /// Number of notifications enqueued.
    pub fn queued(&self) -> usize {
        match self {
            IngestOutcome::Accepted { queued, .. } => *queued,
            IngestOutcome::Duplicate { .. } => 0,
        }
    }
}

/// Pipeline over one configuration snapshot.
pub struct Engine {
    config: Config,
    rules: RulesEngine,
}

impl Engine {
    /// Builds an engine for `config`.
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let rules = RulesEngine::new(&config)?;
        Ok(Self { config, rules })
    }

    /// The configuration this engine was built from.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Stores and evaluates an event atomically.
    pub fn ingest(
        &self,
        db: &mut Database,
        event: &Event,
        now: DateTime<Utc>,
    ) -> anyhow::Result<IngestOutcome> {
        db.write(|store| {
            match store.insert_event(event, now, chrono::Duration::seconds(DEDUP_WINDOW_SECS))? {
                InsertOutcome::Duplicate(existing_id) => {
                    Ok(IngestOutcome::Duplicate { existing_id })
                }
                InsertOutcome::Inserted => self.process(store, event, now),
            }
        })
        .map_err(Into::into)
    }

    /// Evaluates events that were stored but never processed (e.g. after a crash).
    pub fn process_backlog(&self, db: &mut Database, now: DateTime<Utc>) -> anyhow::Result<usize> {
        let pending = db.store().unprocessed_events(500)?;
        let count = pending.len();
        for event in pending {
            db.write(|store| self.process(store, &event, now).map(|_| ()))?;
        }
        Ok(count)
    }

    /// Deletes history older than the retention period.
    pub fn prune(&self, db: &mut Database, now: DateTime<Utc>) -> anyhow::Result<usize> {
        let cutoff = now - chrono::Duration::days(i64::from(self.config.general.retention_days));
        Ok(db.write(|s| s.prune(cutoff))?)
    }

    fn process(
        &self,
        store: &Store<'_>,
        event: &Event,
        now: DateTime<Utc>,
    ) -> agentping_storage::Result<IngestOutcome> {
        store.touch_session(event)?;
        let cancelled = match event.delivery.cancels.as_deref() {
            Some(key) => store.cancel_pending(key, "answered locally before delivery", now)?,
            None => 0,
        };
        let recently_notified = match self.rules.repeat_window(&event.event_type) {
            Some(window) => {
                let window = chrono::Duration::from_std(window).unwrap_or(chrono::Duration::zero());
                store.recently_notified(event, now - window)?
            }
            None => false,
        };
        let ctx = RuleContext {
            now_local: now.with_timezone(&Local).time(),
            recently_notified,
        };
        let decision = self.rules.evaluate(event, &ctx);
        let id = event.id.to_string();
        let mut queued = 0;
        if decision.notify {
            let not_before = now + chrono::Duration::seconds(i64::from(event.delivery.delay_secs));
            for provider in &decision.providers {
                if store.enqueue(
                    &NewNotification {
                        event_id: &id,
                        provider,
                        not_before,
                        cancel_key: event.delivery.cancel_key.as_deref(),
                    },
                    now,
                )? {
                    queued += 1;
                }
            }
        }
        store.mark_processed(&id, &decision.summary(), now)?;
        tracing::info!(
            event_id = %id,
            event_type = %event.event_type,
            notify = decision.notify,
            queued,
            cancelled,
            reason = %decision.reason,
            "event processed"
        );
        Ok(IngestOutcome::Accepted {
            id,
            decision,
            queued,
            cancelled,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentping_core::{AgentKind, EventType};
    use agentping_storage::NotificationStatus;

    fn config() -> Config {
        let mut c = Config::default();
        c.providers.discord.enabled = true;
        c
    }

    fn event(t: EventType) -> Event {
        let mut e = Event::new(t, AgentKind::Claude, Utc::now());
        e.session_id = Some("s1".into());
        e.project_name = Some("api".into());
        e.message = Some("hello".into());
        e
    }

    #[test]
    fn ingest_enqueues_per_provider_and_dedups() {
        let engine = Engine::new(config()).unwrap();
        let mut db = Database::open_in_memory().unwrap();
        let e = event(EventType::PermissionRequest);
        let out = engine.ingest(&mut db, &e, Utc::now()).unwrap();
        assert_eq!(out.queued(), 1);
        let mut dup = e.clone();
        dup.id = uuid::Uuid::now_v7();
        let out = engine.ingest(&mut db, &dup, Utc::now()).unwrap();
        assert!(matches!(out, IngestOutcome::Duplicate { .. }));
        let stats = db.store().stats(Utc::now()).unwrap();
        assert_eq!(
            (stats.events, stats.pending, stats.unprocessed_events),
            (1, 1, 0)
        );
    }

    #[test]
    fn muted_events_are_recorded_with_reason() {
        let engine = Engine::new(Config::default()).unwrap();
        let mut db = Database::open_in_memory().unwrap();
        let e = event(EventType::PermissionRequest);
        engine.ingest(&mut db, &e, Utc::now()).unwrap();
        let stored = db.store().find_event(&e.id.to_string()).unwrap().unwrap();
        assert_eq!(
            stored.decision.as_deref(),
            Some("muted: no notification providers are enabled")
        );
        assert!(stored.deliveries.is_empty());
    }

    #[test]
    fn question_is_delayed_and_cancelled_by_answer() {
        let engine = Engine::new(config()).unwrap();
        let mut db = Database::open_in_memory().unwrap();
        let mut q = event(EventType::UserQuestion);
        q.delivery.delay_secs = 15;
        q.delivery.cancel_key = Some("claude:s1:toolu_1".into());
        let now = Utc::now();
        engine.ingest(&mut db, &q, now).unwrap();
        // Not due yet.
        assert!(db
            .write(|s| s.claim_due(now, now + chrono::Duration::seconds(30), 10))
            .unwrap()
            .is_empty());
        let mut answer = event(EventType::Activity);
        answer.message = None;
        answer.delivery.cancels = Some("claude:s1:toolu_1".into());
        match engine.ingest(&mut db, &answer, now).unwrap() {
            IngestOutcome::Accepted {
                cancelled, queued, ..
            } => {
                assert_eq!((cancelled, queued), (1, 0));
            }
            other => panic!("{other:?}"),
        }
        let n = db
            .store()
            .notifications_for_event(&q.id.to_string())
            .unwrap();
        assert_eq!(n[0].status, NotificationStatus::Cancelled);
    }

    #[test]
    fn repeat_idle_events_are_suppressed() {
        let engine = Engine::new(config()).unwrap();
        let mut db = Database::open_in_memory().unwrap();
        let first = event(EventType::AgentIdle);
        assert_eq!(
            engine.ingest(&mut db, &first, Utc::now()).unwrap().queued(),
            1
        );
        let mut second = event(EventType::AgentIdle);
        second.message = Some("still idle".into());
        assert_eq!(
            engine
                .ingest(&mut db, &second, Utc::now())
                .unwrap()
                .queued(),
            0
        );
    }

    #[test]
    fn backlog_is_processed_once() {
        let engine = Engine::new(config()).unwrap();
        let mut db = Database::open_in_memory().unwrap();
        let e = event(EventType::TaskCompleted);
        db.write(|s| s.insert_event(&e, Utc::now(), chrono::Duration::seconds(30)))
            .unwrap();
        assert_eq!(engine.process_backlog(&mut db, Utc::now()).unwrap(), 1);
        assert_eq!(engine.process_backlog(&mut db, Utc::now()).unwrap(), 0);
        assert_eq!(db.store().stats(Utc::now()).unwrap().pending, 1);
    }
}
