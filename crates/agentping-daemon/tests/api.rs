#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use agentping_core::{AgentKind, Config, Event, EventType};
use agentping_daemon::engine::Engine;
use agentping_daemon::server::{router, AppState};
use agentping_storage::Database;
use chrono::Utc;
use tokio::sync::{watch, Notify};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Server {
    base: String,
    port: u16,
    shutdown: watch::Receiver<bool>,
    db: Arc<Mutex<Database>>,
}

async fn start() -> Server {
    let mut config = Config::default();
    config.providers.discord.enabled = true;
    let db = Arc::new(Mutex::new(Database::open_in_memory().unwrap()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = watch::channel(false);
    let state = AppState {
        db: db.clone(),
        engine: Arc::new(Engine::new(config).unwrap()),
        token: TOKEN.into(),
        wake: Arc::new(Notify::new()),
        shutdown: tx,
        started_at: Utc::now(),
        port,
        providers_ready: Arc::new(vec!["discord".into()]),
        providers_failed: Arc::new(vec![]),
    };
    tokio::spawn(async move {
        axum::serve(listener, router(state)).await.unwrap();
    });
    Server {
        base: format!("http://127.0.0.1:{port}"),
        port,
        shutdown: rx,
        db,
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn event() -> Event {
    let mut e = Event::new(EventType::PermissionRequest, AgentKind::Claude, Utc::now());
    e.session_id = Some("s1".into());
    e.project_name = Some("api".into());
    e.message = Some("Claude needs your permission to use Bash".into());
    e
}

#[tokio::test]
async fn health_is_public_but_everything_else_needs_the_token() {
    let s = start().await;
    let c = client();
    let health = c.get(format!("{}/v1/health", s.base)).send().await.unwrap();
    assert_eq!(health.status(), 200);
    let body: serde_json::Value = health.json().await.unwrap();
    assert_eq!(body["service"], "agentping");

    for (method, path) in [
        ("GET", "/v1/status"),
        ("POST", "/v1/events"),
        ("POST", "/v1/admin/shutdown"),
    ] {
        let req = c.request(method.parse().unwrap(), format!("{}{path}", s.base));
        assert_eq!(
            req.send().await.unwrap().status(),
            401,
            "{path} without token"
        );
        let req = c
            .request(method.parse().unwrap(), format!("{}{path}", s.base))
            .bearer_auth("wrong-token");
        assert_eq!(
            req.send().await.unwrap().status(),
            401,
            "{path} with wrong token"
        );
    }
    assert!(!*s.shutdown.borrow());
}

#[tokio::test]
async fn foreign_hosts_and_browser_origins_are_rejected() {
    let s = start().await;
    let c = client();
    let rebinding = c
        .get(format!("{}/v1/health", s.base))
        .header("Host", format!("evil.example:{}", s.port))
        .send()
        .await
        .unwrap();
    assert_eq!(rebinding.status(), 403);
    let browser = c
        .post(format!("{}/v1/events", s.base))
        .header("Origin", "https://evil.example")
        .bearer_auth(TOKEN)
        .json(&event())
        .send()
        .await
        .unwrap();
    assert_eq!(browser.status(), 403);
}

#[tokio::test]
async fn ingest_validates_stores_and_dedups() {
    let s = start().await;
    let c = client();
    let e = event();
    let first = c
        .post(format!("{}/v1/events", s.base))
        .bearer_auth(TOKEN)
        .json(&e)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 202);
    let body: serde_json::Value = first.json().await.unwrap();
    assert_eq!(body["outcome"], "accepted");
    assert_eq!(body["queued"], 1);

    let mut dup = e.clone();
    dup.id = uuid::Uuid::now_v7();
    let second = c
        .post(format!("{}/v1/events", s.base))
        .bearer_auth(TOKEN)
        .json(&dup)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 200);
    let body: serde_json::Value = second.json().await.unwrap();
    assert_eq!(body["outcome"], "duplicate");

    let mut bad = event();
    bad.schema_version = 99;
    let rejected = c
        .post(format!("{}/v1/events", s.base))
        .bearer_auth(TOKEN)
        .json(&bad)
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), 422);

    let garbage = c
        .post(format!("{}/v1/events", s.base))
        .bearer_auth(TOKEN)
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(garbage.status(), 400);

    let huge = c
        .post(format!("{}/v1/events", s.base))
        .bearer_auth(TOKEN)
        .body(vec![b'x'; 300 * 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(huge.status(), 413);

    let stats = s.db.lock().unwrap().store().stats(Utc::now()).unwrap();
    assert_eq!((stats.events, stats.pending), (1, 1));

    let status: serde_json::Value = c
        .get(format!("{}/v1/status", s.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["stats"]["events"], 1);
    assert_eq!(status["sessions"][0]["status"], "waiting_for_permission");
}

#[tokio::test]
async fn shutdown_endpoint_signals_stop() {
    let s = start().await;
    let resp = client()
        .post(format!("{}/v1/admin/shutdown", s.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    assert!(*s.shutdown.borrow());
}
