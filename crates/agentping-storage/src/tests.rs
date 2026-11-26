use agentping_core::{AgentKind, Event, EventType};
use chrono::{Duration, Utc};

use super::*;

fn event(t: EventType, session: &str, message: &str) -> Event {
    let mut e = Event::new(t, AgentKind::Claude, Utc::now());
    e.session_id = Some(session.into());
    e.project_name = Some("backend-api".into());
    e.message = Some(message.into());
    e
}

fn insert(db: &mut Database, e: &Event) -> InsertOutcome {
    db.write(|s| s.insert_event(e, Utc::now(), Duration::seconds(30)))
        .unwrap()
}

#[test]
fn migrations_are_applied_once_and_reopen_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.db");
    {
        let db = Database::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(db.quick_check().unwrap(), None);
    }
    let db = Database::open(&path).unwrap();
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
}

#[test]
fn newer_schema_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
    }
    assert!(matches!(
        Database::open(&path),
        Err(StorageError::SchemaTooNew { found: 99, .. })
    ));
}

#[test]
fn duplicate_events_within_window_are_detected() {
    let mut db = Database::open_in_memory().unwrap();
    let a = event(EventType::PermissionRequest, "s1", "needs permission");
    let mut b = a.clone();
    b.id = uuid::Uuid::now_v7();
    assert_eq!(insert(&mut db, &a), InsertOutcome::Inserted);
    assert_eq!(
        insert(&mut db, &b),
        InsertOutcome::Duplicate(a.id.to_string())
    );
    // Outside the window the same content is a new event.
    let later = db
        .write(|s| {
            s.insert_event(
                &b,
                Utc::now() + Duration::seconds(120),
                Duration::seconds(30),
            )
        })
        .unwrap();
    assert_eq!(later, InsertOutcome::Inserted);
}

#[test]
fn queue_claim_deliver_retry_fail_lifecycle() {
    let mut db = Database::open_in_memory().unwrap();
    let e = event(EventType::TaskCompleted, "s1", "done");
    insert(&mut db, &e);
    let id = e.id.to_string();
    let now = Utc::now();
    db.write(|s| {
        assert!(s.enqueue(
            &NewNotification {
                event_id: &id,
                provider: "discord",
                not_before: now,
                cancel_key: None
            },
            now
        )?);
        // Enqueueing again for the same provider is a no-op.
        assert!(!s.enqueue(
            &NewNotification {
                event_id: &id,
                provider: "discord",
                not_before: now,
                cancel_key: None
            },
            now
        )?);
        Ok(())
    })
    .unwrap();

    let claimed = db
        .write(|s| s.claim_due(now, now + Duration::seconds(60), 10))
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].event, e);
    // A second claim sees nothing while the lease is held.
    assert!(db
        .write(|s| s.claim_due(now, now + Duration::seconds(60), 10))
        .unwrap()
        .is_empty());

    let retry_at = now + Duration::seconds(5);
    db.write(|s| {
        s.record_attempt(
            &claimed[0].id,
            &AttemptOutcome::Retry {
                retry_at,
                http_status: Some(503),
                error: "service unavailable".into(),
            },
            now,
            12,
        )
    })
    .unwrap();
    assert!(db
        .write(|s| s.claim_due(now, now + Duration::seconds(60), 10))
        .unwrap()
        .is_empty());
    let again = db
        .write(|s| s.claim_due(retry_at, retry_at + Duration::seconds(60), 10))
        .unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].attempts, 1);
    db.write(|s| {
        s.record_attempt(
            &again[0].id,
            &AttemptOutcome::Delivered {
                http_status: Some(200),
                external_id: Some("m1 m2".into()),
            },
            retry_at,
            8,
        )
    })
    .unwrap();

    let records = db.store().notifications_for_event(&id).unwrap();
    assert_eq!(records[0].status, NotificationStatus::Delivered);
    assert_eq!(records[0].attempts, 2);
    let attempts = db
        .store()
        .attempts_for_notification(&records[0].id)
        .unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].outcome, "retry");
    assert_eq!(attempts[0].http_status, Some(503));
    assert_eq!(attempts[1].outcome, "delivered");
    let stats = db.store().stats(Utc::now()).unwrap();
    assert_eq!((stats.delivered, stats.pending, stats.failed), (1, 0, 0));
}

#[test]
fn expired_leases_are_reclaimed_after_crash() {
    let mut db = Database::open_in_memory().unwrap();
    let e = event(EventType::PermissionRequest, "s1", "x");
    insert(&mut db, &e);
    let now = Utc::now();
    let id = e.id.to_string();
    db.write(|s| {
        s.enqueue(
            &NewNotification {
                event_id: &id,
                provider: "discord",
                not_before: now,
                cancel_key: None,
            },
            now,
        )
    })
    .unwrap();
    let first = db
        .write(|s| s.claim_due(now, now + Duration::seconds(30), 10))
        .unwrap();
    assert_eq!(first.len(), 1);
    // The worker "crashes" and never records an attempt. After the lease expires the
    // notification becomes claimable again.
    let later = now + Duration::seconds(31);
    let second = db
        .write(|s| s.claim_due(later, later + Duration::seconds(30), 10))
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].id, first[0].id);
}

#[test]
fn release_returns_claims_without_counting_attempts() {
    let mut db = Database::open_in_memory().unwrap();
    let e = event(EventType::PermissionRequest, "s1", "x");
    insert(&mut db, &e);
    let now = Utc::now();
    let id = e.id.to_string();
    db.write(|s| {
        s.enqueue(
            &NewNotification {
                event_id: &id,
                provider: "telegram",
                not_before: now,
                cancel_key: None,
            },
            now,
        )
    })
    .unwrap();
    let claimed = db
        .write(|s| s.claim_due(now, now + Duration::seconds(30), 10))
        .unwrap();
    let ids: Vec<String> = claimed.iter().map(|c| c.id.clone()).collect();
    db.write(|s| s.release(&ids, now)).unwrap();
    let again = db
        .write(|s| s.claim_due(now, now + Duration::seconds(30), 10))
        .unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].attempts, 0);
}

#[test]
fn cancellation_only_affects_pending_notifications() {
    let mut db = Database::open_in_memory().unwrap();
    let e = event(EventType::UserQuestion, "s1", "Which provider?");
    insert(&mut db, &e);
    let now = Utc::now();
    let id = e.id.to_string();
    db.write(|s| {
        s.enqueue(
            &NewNotification {
                event_id: &id,
                provider: "discord",
                not_before: now + Duration::seconds(15),
                cancel_key: Some("claude:toolu_1"),
            },
            now,
        )
    })
    .unwrap();
    assert!(db.store().next_due_at().unwrap().is_some());
    let n = db
        .write(|s| s.cancel_pending("claude:toolu_1", "answered locally", now))
        .unwrap();
    assert_eq!(n, 1);
    let records = db.store().notifications_for_event(&id).unwrap();
    assert_eq!(records[0].status, NotificationStatus::Cancelled);
    assert!(db
        .write(|s| s.claim_due(now + Duration::seconds(60), now + Duration::seconds(90), 10))
        .unwrap()
        .is_empty());
}

#[test]
fn sessions_track_status_and_counts() {
    let mut db = Database::open_in_memory().unwrap();
    let start = event(EventType::AgentStarted, "s1", "");
    let perm = event(EventType::PermissionRequest, "s1", "perm");
    let note = event(EventType::Notification, "s1", "auth ok");
    for e in [&start, &perm, &note] {
        insert(&mut db, e);
        db.write(|s| s.touch_session(e)).unwrap();
    }
    let sessions = db
        .store()
        .sessions(Utc::now() - Duration::hours(1), false)
        .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].event_count, 3);
    // Generic notifications do not change the derived status.
    assert_eq!(sessions[0].status, SessionStatus::WaitingForPermission);
    assert_eq!(sessions[0].project_name.as_deref(), Some("backend-api"));

    let end = event(EventType::AgentStopped, "s1", "");
    db.write(|s| s.touch_session(&end)).unwrap();
    assert!(db
        .store()
        .sessions(Utc::now() - Duration::hours(1), false)
        .unwrap()
        .is_empty());
}

#[test]
fn list_events_filters_and_includes_deliveries() {
    let mut db = Database::open_in_memory().unwrap();
    let a = event(EventType::PermissionRequest, "s1", "a");
    let mut b = event(EventType::TaskCompleted, "s2", "b");
    b.project_name = Some("frontend".into());
    insert(&mut db, &a);
    insert(&mut db, &b);
    let now = Utc::now();
    let a_id = a.id.to_string();
    db.write(|s| {
        s.enqueue(
            &NewNotification {
                event_id: &a_id,
                provider: "discord",
                not_before: now,
                cancel_key: None,
            },
            now,
        )
    })
    .unwrap();
    let all = db.store().list_events(&EventQuery::default()).unwrap();
    assert_eq!(all.len(), 2);
    let perms = db
        .store()
        .list_events(&EventQuery {
            event_type: Some("permission_request".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(perms.len(), 1);
    assert_eq!(perms[0].deliveries.len(), 1);
    let front = db
        .store()
        .list_events(&EventQuery {
            project: Some("FRONTEND".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(front.len(), 1);
    let prefix = &a_id[..30];
    assert_eq!(
        db.store().find_event(prefix).unwrap().unwrap().event.id,
        a.id
    );
}

#[test]
fn recently_notified_detects_repeats_per_session() {
    let mut db = Database::open_in_memory().unwrap();
    let first = event(EventType::AgentIdle, "s1", "idle");
    insert(&mut db, &first);
    let now = Utc::now();
    let id = first.id.to_string();
    db.write(|s| {
        s.enqueue(
            &NewNotification {
                event_id: &id,
                provider: "discord",
                not_before: now,
                cancel_key: None,
            },
            now,
        )
    })
    .unwrap();
    let second = event(EventType::AgentIdle, "s1", "idle again");
    let other_session = event(EventType::AgentIdle, "s2", "idle");
    let since = now - Duration::seconds(60);
    assert!(db.store().recently_notified(&second, since).unwrap());
    assert!(!db.store().recently_notified(&other_session, since).unwrap());
}

#[test]
fn prune_keeps_undelivered_and_clear_removes_everything() {
    let mut db = Database::open_in_memory().unwrap();
    let old = event(EventType::TaskCompleted, "s1", "old");
    let pending = event(EventType::PermissionRequest, "s1", "pending");
    let past = Utc::now() - Duration::days(40);
    db.write(|s| s.insert_event(&old, past, Duration::seconds(30)))
        .unwrap();
    db.write(|s| s.insert_event(&pending, past, Duration::seconds(30)))
        .unwrap();
    let pid = pending.id.to_string();
    db.write(|s| {
        s.enqueue(
            &NewNotification {
                event_id: &pid,
                provider: "discord",
                not_before: past,
                cancel_key: None,
            },
            past,
        )
    })
    .unwrap();
    let removed = db
        .write(|s| s.prune(Utc::now() - Duration::days(30)))
        .unwrap();
    assert_eq!(removed, 1);
    assert_eq!(db.store().stats(Utc::now()).unwrap().events, 1);
    assert_eq!(db.write(|s| s.clear_all()).unwrap(), 1);
    assert_eq!(db.store().stats(Utc::now()).unwrap(), Stats::default());
}

#[test]
fn unprocessed_events_are_returned_until_marked() {
    let mut db = Database::open_in_memory().unwrap();
    let e = event(EventType::TaskCompleted, "s1", "done");
    insert(&mut db, &e);
    assert_eq!(db.store().unprocessed_events(10).unwrap().len(), 1);
    db.write(|s| s.mark_processed(&e.id.to_string(), "notify: discord", Utc::now()))
        .unwrap();
    assert!(db.store().unprocessed_events(10).unwrap().is_empty());
}

#[test]
fn interactions_resolve_once_and_close_per_session() {
    let mut db = Database::open_in_memory().unwrap();
    let now = Utc::now();
    let req = serde_json::json!({ "kind": "permission", "tool_name": "Bash", "details": "ls" });
    db.write(|s| {
        s.create_interaction(
            &NewInteraction {
                id: "i1",
                kind: InteractionKind::Permission,
                agent: "claude",
                session_id: Some("s1"),
                project_name: Some("api"),
                request: &req,
                expires_at: now + Duration::minutes(10),
            },
            now,
        )?;
        s.set_interaction_message("i1", "m1")
    })
    .unwrap();
    assert_eq!(db.store().pending_interactions(now).unwrap().len(), 1);
    assert_eq!(
        db.store().interaction_by_message("m1").unwrap().unwrap().id,
        "i1"
    );
    assert!(db
        .store()
        .has_recent_interaction(
            "claude",
            "s1",
            InteractionKind::Permission,
            now - Duration::minutes(1)
        )
        .unwrap());
    let resp = serde_json::json!({ "kind": "permission", "allow": true });
    assert!(db
        .write(|s| s.resolve_interaction("i1", &resp, "om", now))
        .unwrap());
    assert!(
        !db.write(|s| s.resolve_interaction("i1", &resp, "om", now))
            .unwrap(),
        "replay rejected"
    );
    let rec = db.store().interaction("i1").unwrap().unwrap();
    assert_eq!(rec.status, InteractionStatus::Resolved);
    assert_eq!(rec.responder.as_deref(), Some("om"));

    db.write(|s| {
        for id in ["i2", "i3"] {
            s.create_interaction(
                &NewInteraction {
                    id,
                    kind: InteractionKind::Continue,
                    agent: "claude",
                    session_id: Some("s1"),
                    project_name: None,
                    request: &req,
                    expires_at: now + Duration::minutes(10),
                },
                now,
            )?;
        }
        Ok(())
    })
    .unwrap();
    let closed = db
        .write(|s| s.close_session_interactions("claude", "s1", None, "typed locally", now))
        .unwrap();
    assert_eq!(closed.len(), 2);
    assert!(db.store().pending_interactions(now).unwrap().is_empty());
}

#[test]
fn expired_interactions_cannot_be_resolved() {
    let mut db = Database::open_in_memory().unwrap();
    let now = Utc::now();
    let req = serde_json::json!({});
    db.write(|s| {
        s.create_interaction(
            &NewInteraction {
                id: "old",
                kind: InteractionKind::Question,
                agent: "claude",
                session_id: None,
                project_name: None,
                request: &req,
                expires_at: now - Duration::seconds(1),
            },
            now - Duration::minutes(5),
        )
    })
    .unwrap();
    assert!(!db
        .write(|s| s.resolve_interaction("old", &req, "om", now))
        .unwrap());
    let expired = db.write(|s| s.expire_interactions(now)).unwrap();
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].status, InteractionStatus::Expired);
}

#[test]
fn replies_map_to_sessions_through_message_ids_and_kv_works() {
    let mut db = Database::open_in_memory().unwrap();
    let e = event(EventType::TaskCompleted, "s9", "done");
    insert(&mut db, &e);
    let now = Utc::now();
    let id = e.id.to_string();
    db.write(|s| {
        s.enqueue(
            &NewNotification {
                event_id: &id,
                provider: "discord",
                not_before: now,
                cancel_key: None,
            },
            now,
        )
    })
    .unwrap();
    let claimed = db
        .write(|s| s.claim_due(now, now + Duration::seconds(30), 5))
        .unwrap();
    db.write(|s| {
        s.record_attempt(
            &claimed[0].id,
            &AttemptOutcome::Delivered {
                http_status: Some(200),
                external_id: Some("111 222".into()),
            },
            now,
            5,
        )
    })
    .unwrap();
    assert_eq!(
        db.store().session_for_message("discord", "222").unwrap(),
        Some(("claude".to_owned(), "s9".to_owned()))
    );
    assert_eq!(
        db.store().session_for_message("discord", "22").unwrap(),
        None
    );
    db.write(|s| s.kv_set("presence", "away", now)).unwrap();
    assert_eq!(
        db.store().kv_get("presence").unwrap().as_deref(),
        Some("away")
    );
}
