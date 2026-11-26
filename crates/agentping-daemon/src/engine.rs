//! The event pipeline: dedup → session tracking → cancellation → rules → enqueue.
//!
//! The engine is synchronous and storage-backed, so the same code runs inside the daemon and
//! inside a hook process when the daemon is offline.

use agentping_core::{Config, Event, EventType};
use agentping_rules::{Decision, RuleContext, RulesEngine};
use agentping_storage::{
    Database, InsertOutcome, InteractionKind, InteractionRecord, NewNotification, Store,
};
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
        /// Remote prompts withdrawn because the user acted locally (their messages need updating).
        #[serde(skip)]
        closed_interactions: Vec<InteractionRecord>,
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

    /// Remote prompts withdrawn by this event.
    pub fn closed_interactions(&self) -> &[InteractionRecord] {
        match self {
            IngestOutcome::Accepted {
                closed_interactions,
                ..
            } => closed_interactions,
            IngestOutcome::Duplicate { .. } => &[],
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
            let event = self.enrich(store, event, now)?;
            match store.insert_event(&event, now, chrono::Duration::seconds(DEDUP_WINDOW_SECS))? {
                InsertOutcome::Duplicate(existing_id) => {
                    Ok(IngestOutcome::Duplicate { existing_id })
                }
                InsertOutcome::Inserted => self.process(store, &event, now),
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

    /// Adds the exact tool call (captured by the `PermissionRequest` hook) to the delayed
    /// `permission_prompt` notification, so the ping says what is being asked.
    fn enrich(
        &self,
        store: &Store<'_>,
        event: &Event,
        now: DateTime<Utc>,
    ) -> agentping_storage::Result<Event> {
        let mut event = event.clone();
        let is_prompt = event.event_type == EventType::PermissionRequest
            && event.source.native_event.as_deref() == Some("Notification");
        if let (true, Some(session)) = (is_prompt, event.session_id.clone()) {
            let since = now - chrono::Duration::minutes(10);
            if let Some(ctx) = store.latest_native_event(
                event.agent.as_str(),
                &session,
                "PermissionRequest",
                since,
            )? {
                let details = ctx.message.unwrap_or_default();
                let base = event.message.take().unwrap_or_default();
                event.message = Some(format!("{base}\n\n{details}").trim().to_owned());
                if let Some(tool) = ctx.metadata.get("tool_name") {
                    event.metadata.insert("tool_name".into(), tool.clone());
                }
            }
        }
        Ok(event)
    }

    /// Withdraws remote prompts the user made obsolete by acting locally.
    fn close_obsolete_interactions(
        &self,
        store: &Store<'_>,
        event: &Event,
        now: DateTime<Utc>,
    ) -> agentping_storage::Result<Vec<InteractionRecord>> {
        let Some(session) = event.session_id.as_deref() else {
            return Ok(Vec::new());
        };
        let agent = event.agent.as_str();
        match (
            event.event_type.clone(),
            event.source.native_event.as_deref(),
        ) {
            (EventType::Activity, Some("UserPromptSubmit")) | (EventType::AgentStopped, _) => store
                .close_session_interactions(agent, session, None, "handled in the terminal", now),
            (EventType::TaskCompleted, _) | (EventType::TaskFailed, _) => {
                let mut closed = store.close_session_interactions(
                    agent,
                    session,
                    Some(InteractionKind::Permission),
                    "handled in the terminal",
                    now,
                )?;
                closed.extend(store.close_session_interactions(
                    agent,
                    session,
                    Some(InteractionKind::Question),
                    "handled in the terminal",
                    now,
                )?);
                Ok(closed)
            }
            _ => Ok(Vec::new()),
        }
    }

    /// Whether this ping duplicates an interactive remote prompt already sent for the session.
    fn covered_by_prompt(
        &self,
        store: &Store<'_>,
        event: &Event,
        now: DateTime<Utc>,
    ) -> agentping_storage::Result<bool> {
        let (Some(session), true) = (event.session_id.as_deref(), self.config.remote.enabled)
        else {
            return Ok(false);
        };
        let kind = match event.event_type {
            EventType::PermissionRequest => InteractionKind::Permission,
            EventType::UserQuestion => InteractionKind::Question,
            _ => return Ok(false),
        };
        store.has_recent_interaction(
            event.agent.as_str(),
            session,
            kind,
            now - chrono::Duration::minutes(5),
        )
    }

    fn process(
        &self,
        store: &Store<'_>,
        event: &Event,
        now: DateTime<Utc>,
    ) -> agentping_storage::Result<IngestOutcome> {
        store.touch_session(event)?;
        let closed_interactions = self.close_obsolete_interactions(store, event, now)?;
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
        let mut decision = self.rules.evaluate(event, &ctx);
        if decision.notify && self.covered_by_prompt(store, event, now)? {
            decision.notify = false;
            decision.providers.clear();
            decision.reason = "already sent as an interactive Discord prompt".into();
        }
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
            closed_interactions,
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

    #[test]
    fn permission_prompt_is_enriched_with_the_command() {
        let engine = Engine::new(config()).unwrap();
        let mut db = Database::open_in_memory().unwrap();
        let mut ctx = event(EventType::Activity);
        ctx.source.native_event = Some("PermissionRequest".into());
        ctx.message = Some("```\nrm -rf node_modules\n```".into());
        ctx.metadata.insert("tool_name".into(), "Bash".into());
        engine.ingest(&mut db, &ctx, Utc::now()).unwrap();
        let mut ping = event(EventType::PermissionRequest);
        ping.source.native_event = Some("Notification".into());
        ping.message = Some("Claude needs your permission to use Bash".into());
        engine.ingest(&mut db, &ping, Utc::now()).unwrap();
        let stored = db
            .store()
            .find_event(&ping.id.to_string())
            .unwrap()
            .unwrap();
        let msg = stored.event.message.unwrap();
        assert!(msg.starts_with("Claude needs your permission to use Bash"));
        assert!(msg.contains("rm -rf node_modules"));
    }

    #[test]
    fn local_prompt_closes_remote_interactions() {
        let engine = Engine::new(config()).unwrap();
        let mut db = Database::open_in_memory().unwrap();
        let now = Utc::now();
        let req = serde_json::json!({});
        db.write(|s| {
            s.create_interaction(
                &agentping_storage::NewInteraction {
                    id: "c1",
                    kind: InteractionKind::Continue,
                    agent: "claude",
                    session_id: Some("s1"),
                    project_name: None,
                    request: &req,
                    expires_at: now + chrono::Duration::minutes(30),
                },
                now,
            )
        })
        .unwrap();
        let mut prompt = event(EventType::Activity);
        prompt.message = None;
        prompt.source.native_event = Some("UserPromptSubmit".into());
        let out = engine.ingest(&mut db, &prompt, now).unwrap();
        assert_eq!(out.closed_interactions().len(), 1);
    }

    #[test]
    fn ping_is_muted_when_an_interactive_prompt_exists() {
        let mut c = config();
        c.remote.enabled = true;
        let engine = Engine::new(c).unwrap();
        let mut db = Database::open_in_memory().unwrap();
        let now = Utc::now();
        let req = serde_json::json!({});
        db.write(|s| {
            s.create_interaction(
                &agentping_storage::NewInteraction {
                    id: "p1",
                    kind: InteractionKind::Permission,
                    agent: "claude",
                    session_id: Some("s1"),
                    project_name: None,
                    request: &req,
                    expires_at: now + chrono::Duration::minutes(10),
                },
                now,
            )
        })
        .unwrap();
        let out = engine
            .ingest(&mut db, &event(EventType::PermissionRequest), now)
            .unwrap();
        assert_eq!(out.queued(), 0);
    }
}
