//! Asynchronous delivery of queued notifications with retries and backoff.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agentping_core::retry::RetryPolicy;
use agentping_core::Config;
use agentping_notifiers::{render, FailureKind, ProviderSet};
use agentping_storage::{AttemptOutcome, ClaimedNotification, Database};
use chrono::Utc;
use serde::Serialize;
use tokio::task::JoinSet;

/// Shared database handle for async code.
pub type SharedDb = Arc<Mutex<Database>>;

/// Runs a blocking database closure on the blocking thread pool.
pub async fn with_db<T, F>(db: &SharedDb, f: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce(&mut Database) -> anyhow::Result<T> + Send + 'static,
{
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let mut guard = db
            .lock()
            .map_err(|_| anyhow::anyhow!("database lock poisoned"))?;
        f(&mut guard)
    })
    .await?
}

/// Counters for one delivery pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PassStats {
    /// Notifications delivered.
    pub delivered: usize,
    /// Notifications scheduled for retry.
    pub retried: usize,
    /// Notifications that failed permanently.
    pub failed: usize,
}

impl PassStats {
    /// Total notifications handled.
    pub fn total(&self) -> usize {
        self.delivered + self.retried + self.failed
    }
}

/// Delivers queued notifications through configured providers.
pub struct Deliverer {
    db: SharedDb,
    config: Arc<Config>,
    providers: Arc<ProviderSet>,
    retry: RetryPolicy,
    lease: Duration,
    batch: u32,
}

impl Deliverer {
    /// Creates a deliverer.
    pub fn new(db: SharedDb, config: Arc<Config>, providers: Arc<ProviderSet>) -> Self {
        Self {
            db,
            config,
            providers,
            retry: RetryPolicy::default(),
            lease: Duration::from_secs(60),
            batch: 20,
        }
    }

    /// Overrides the retry policy (tests).
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Claims due notifications and delivers them concurrently.
    pub async fn run_pass(&self) -> anyhow::Result<PassStats> {
        let lease = chrono::Duration::from_std(self.lease)?;
        let batch = self.batch;
        let claimed = with_db(&self.db, move |db| {
            let now = Utc::now();
            Ok(db.write(|s| s.claim_due(now, now + lease, batch))?)
        })
        .await?;
        let mut stats = PassStats::default();
        if claimed.is_empty() {
            return Ok(stats);
        }
        let mut set = JoinSet::new();
        for n in claimed {
            let providers = self.providers.clone();
            let config = self.config.clone();
            let retry = self.retry;
            set.spawn(async move {
                let started = Instant::now();
                let outcome = attempt(&n, &providers, &config, retry).await;
                (n.id, outcome, started.elapsed())
            });
        }
        while let Some(joined) = set.join_next().await {
            let (id, outcome, elapsed) = joined?;
            match &outcome {
                AttemptOutcome::Delivered { .. } => stats.delivered += 1,
                AttemptOutcome::Retry { .. } => stats.retried += 1,
                AttemptOutcome::Failed { .. } => stats.failed += 1,
            }
            log_outcome(&id, &outcome);
            let ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
            with_db(&self.db, move |db| {
                Ok(db.write(|s| s.record_attempt(&id, &outcome, Utc::now(), ms))?)
            })
            .await?;
        }
        Ok(stats)
    }

    /// Time until the next scheduled notification, if any.
    pub async fn next_due_in(&self) -> anyhow::Result<Option<Duration>> {
        let next = with_db(&self.db, |db| Ok(db.store().next_due_at()?)).await?;
        Ok(next.map(|t| (t - Utc::now()).to_std().unwrap_or(Duration::ZERO)))
    }
}

fn log_outcome(id: &str, outcome: &AttemptOutcome) {
    match outcome {
        AttemptOutcome::Delivered { http_status } => {
            tracing::info!(notification = id, ?http_status, "notification delivered");
        }
        AttemptOutcome::Retry {
            retry_at, error, ..
        } => {
            tracing::warn!(notification = id, %retry_at, error = %error, "delivery failed; will retry");
        }
        AttemptOutcome::Failed { error, .. } => {
            tracing::error!(notification = id, error = %error, "delivery failed permanently");
        }
    }
}

async fn attempt(
    n: &ClaimedNotification,
    providers: &ProviderSet,
    config: &Config,
    retry: RetryPolicy,
) -> AttemptOutcome {
    let Some(provider) = providers.get(&n.provider) else {
        let reason = providers
            .errors
            .get(&n.provider)
            .cloned()
            .unwrap_or_else(|| format!("provider '{}' is not enabled", n.provider));
        return AttemptOutcome::Failed {
            http_status: None,
            error: reason,
        };
    };
    let rendered = render(&n.event, n.event.severity, config);
    let result = tokio::time::timeout(Duration::from_secs(20), provider.send(&rendered)).await;
    let attempts_made = n.attempts + 1;
    match result {
        Ok(Ok(delivered)) => AttemptOutcome::Delivered {
            http_status: delivered.http_status,
        },
        Ok(Err(err)) => match err.kind {
            FailureKind::Permanent => AttemptOutcome::Failed {
                http_status: err.http_status,
                error: err.message,
            },
            FailureKind::Retryable { retry_after } => retry_or_fail(
                retry,
                attempts_made,
                retry_after,
                err.http_status,
                err.message,
            ),
        },
        Err(_) => retry_or_fail(
            retry,
            attempts_made,
            None,
            None,
            "provider did not respond within 20s".into(),
        ),
    }
}

fn retry_or_fail(
    retry: RetryPolicy,
    attempts_made: u32,
    retry_after: Option<Duration>,
    http_status: Option<u16>,
    error: String,
) -> AttemptOutcome {
    if !retry.should_retry(attempts_made) {
        return AttemptOutcome::Failed {
            http_status,
            error: format!("gave up after {attempts_made} attempts: {error}"),
        };
    }
    let backoff = retry.delay_random(attempts_made);
    let wait = retry_after.map_or(backoff, |r| r.max(Duration::from_millis(500)));
    let wait = chrono::Duration::from_std(wait).unwrap_or(chrono::Duration::seconds(60));
    AttemptOutcome::Retry {
        retry_at: Utc::now() + wait,
        http_status,
        error,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use agentping_core::{AgentKind, Event, EventType};
    use agentping_notifiers::{
        Delivered, DeliveryError, NotificationProvider, ProviderHealth, RenderedNotification,
    };
    use agentping_storage::NotificationStatus;
    use async_trait::async_trait;

    use super::*;
    use crate::engine::Engine;

    struct MockProvider {
        responses: Mutex<VecDeque<Result<Delivered, DeliveryError>>>,
        sent: Mutex<Vec<RenderedNotification>>,
    }

    #[async_trait]
    impl NotificationProvider for MockProvider {
        fn name(&self) -> &'static str {
            "discord"
        }
        async fn send(&self, n: &RenderedNotification) -> Result<Delivered, DeliveryError> {
            self.sent.lock().unwrap().push(n.clone());
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(Delivered {
                    http_status: Some(200),
                }))
        }
        async fn health_check(&self) -> ProviderHealth {
            ProviderHealth {
                healthy: true,
                detail: "mock".into(),
            }
        }
    }

    fn setup(
        responses: Vec<Result<Delivered, DeliveryError>>,
    ) -> (SharedDb, Arc<MockProvider>, Deliverer, Event) {
        let mut config = Config::default();
        config.providers.discord.enabled = true;
        let mut db = Database::open_in_memory().unwrap();
        let mut e = Event::new(EventType::PermissionRequest, AgentKind::Claude, Utc::now());
        e.session_id = Some("s".into());
        e.project_name = Some("api".into());
        Engine::new(config.clone())
            .unwrap()
            .ingest(&mut db, &e, Utc::now())
            .unwrap();
        let mock = Arc::new(MockProvider {
            responses: Mutex::new(responses.into()),
            sent: Mutex::new(Vec::new()),
        });
        let mut set = ProviderSet::default();
        set.providers.insert(
            "discord".into(),
            mock.clone() as Arc<dyn NotificationProvider>,
        );
        let db = Arc::new(Mutex::new(db));
        let d =
            Deliverer::new(db.clone(), Arc::new(config), Arc::new(set)).with_retry(RetryPolicy {
                base: Duration::from_millis(1),
                max_delay: Duration::from_millis(5),
                max_attempts: 3,
            });
        (db, mock, d, e)
    }

    fn status(db: &SharedDb, e: &Event) -> (NotificationStatus, u32) {
        let n = db
            .lock()
            .unwrap()
            .store()
            .notifications_for_event(&e.id.to_string())
            .unwrap();
        (n[0].status, n[0].attempts)
    }

    #[tokio::test]
    async fn delivers_rendered_notification() {
        let (db, mock, d, e) = setup(vec![]);
        let stats = d.run_pass().await.unwrap();
        assert_eq!(stats.delivered, 1);
        assert_eq!(status(&db, &e), (NotificationStatus::Delivered, 1));
        let title = mock.sent.lock().unwrap()[0].title.clone();
        assert_eq!(title, "🔐 Claude Code needs permission");
        // Nothing left to do.
        assert_eq!(d.run_pass().await.unwrap().total(), 0);
    }

    #[tokio::test]
    async fn retries_then_gives_up() {
        let err = || Err(DeliveryError::retryable("boom", Some(503), None));
        let (db, _mock, d, e) = setup(vec![err(), err(), err()]);
        assert_eq!(d.run_pass().await.unwrap().retried, 1);
        assert_eq!(status(&db, &e), (NotificationStatus::Pending, 1));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(d.run_pass().await.unwrap().retried, 1);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(d.run_pass().await.unwrap().failed, 1);
        assert_eq!(status(&db, &e), (NotificationStatus::Failed, 3));
    }

    #[tokio::test]
    async fn permanent_errors_fail_immediately() {
        let (db, _mock, d, e) = setup(vec![Err(DeliveryError::permanent(
            "Unknown Webhook",
            Some(404),
        ))]);
        assert_eq!(d.run_pass().await.unwrap().failed, 1);
        assert_eq!(status(&db, &e), (NotificationStatus::Failed, 1));
    }

    #[tokio::test]
    async fn missing_provider_fails_with_explanation() {
        let (db, _mock, _d, e) = setup(vec![]);
        let mut set = ProviderSet::default();
        set.errors
            .insert("discord".into(), "no webhook URL is stored".into());
        let d = Deliverer::new(db.clone(), Arc::new(Config::default()), Arc::new(set));
        assert_eq!(d.run_pass().await.unwrap().failed, 1);
        let n = db
            .lock()
            .unwrap()
            .store()
            .notifications_for_event(&e.id.to_string())
            .unwrap();
        assert_eq!(n[0].last_error.as_deref(), Some("no webhook URL is stored"));
    }

    #[test]
    fn retry_after_is_respected() {
        let policy = RetryPolicy::default();
        match retry_or_fail(
            policy,
            1,
            Some(Duration::from_secs(30)),
            Some(429),
            "rl".into(),
        ) {
            AttemptOutcome::Retry { retry_at, .. } => {
                let wait = (retry_at - Utc::now()).num_seconds();
                assert!((29..=31).contains(&wait), "{wait}");
            }
            other => panic!("{other:?}"),
        }
    }
}
