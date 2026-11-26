#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Two-way flow against a mock Discord REST API with simulated gateway events.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentping_core::config::RemoteRoute;
use agentping_core::remote::{
    CreateInteraction, InteractionRequest, InteractionResponse, Question, QuestionOption,
};
use agentping_core::secrets::Secret;
use agentping_core::Config;
use agentping_daemon::remote::RemoteService;
use agentping_remote::discord::model::{Interaction, Message};
use agentping_remote::discord::{DiscordApi, GatewayEvent};
use agentping_storage::Database;
use axum::body::Bytes;
use axum::http::{Method, Uri};
use serde_json::{json, Value};

const CHANNEL: &str = "111111111111111111";
const ME: &str = "222222222222222222";
const STRANGER: &str = "333333333333333333";

#[derive(Default)]
struct Mock {
    calls: Mutex<Vec<(String, String, Value)>>,
    next_id: AtomicU64,
}

async fn start_mock() -> (String, Arc<Mock>) {
    let mock = Arc::new(Mock::default());
    let m = mock.clone();
    let app = axum::Router::new().fallback(move |method: Method, uri: Uri, body: Bytes| {
        let m = m.clone();
        async move {
            let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            m.calls
                .lock()
                .unwrap()
                .push((method.to_string(), uri.path().to_owned(), v));
            if method == Method::POST && uri.path().ends_with("/messages") {
                let id = 900_000_000_000_000_000 + m.next_id.fetch_add(1, Ordering::SeqCst);
                return axum::Json(json!({
                    "id": id.to_string(), "channel_id": CHANNEL,
                    "author": { "id": "bot", "username": "AgentPing", "bot": true }, "content": ""
                }));
            }
            axum::Json(json!({}))
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), mock)
}

async fn service() -> (Arc<RemoteService>, Arc<Mock>) {
    let (base, mock) = start_mock().await;
    let mut config = Config::default();
    config.remote.enabled = true;
    config.remote.discord_channel_id = Some(CHANNEL.into());
    config.remote.discord_user_ids = vec![ME.into()];
    config.remote.route = RemoteRoute::Always;
    let db = Arc::new(Mutex::new(Database::open_in_memory().unwrap()));
    let api = DiscordApi::with_base(reqwest::Client::new(), Secret::new("bot-token"), &base);
    let svc = RemoteService::new(db, Arc::new(config), api).unwrap();
    svc.on_event(GatewayEvent::Ready {
        bot_user_id: "bot".into(),
    })
    .await;
    (svc, mock)
}

fn create(request: InteractionRequest) -> CreateInteraction {
    CreateInteraction {
        agent: "claude".into(),
        agent_name: "Claude Code".into(),
        session_id: Some("s1".into()),
        project_name: Some("api".into()),
        request,
        timeout_secs: 600,
        supersedes_cancel_key: None,
    }
}

fn button(custom_id: &str, user: &str) -> GatewayEvent {
    let i: Interaction = serde_json::from_value(json!({
        "id": "int1", "token": "tok", "type": 3,
        "data": { "custom_id": custom_id },
        "member": { "user": { "id": user, "username": "someone" } },
        "channel_id": CHANNEL
    }))
    .unwrap();
    GatewayEvent::Interaction(Box::new(i))
}

fn message(text: &str, user: &str, reply_to: Option<&str>) -> GatewayEvent {
    let mut v = json!({
        "id": "555555555555555555", "channel_id": CHANNEL,
        "author": { "id": user, "username": "om" }, "content": text
    });
    if let Some(r) = reply_to {
        v["message_reference"] = json!({ "message_id": r });
    }
    let m: Message = serde_json::from_value(v).unwrap();
    GatewayEvent::MessageCreate(Box::new(m))
}

#[tokio::test]
async fn permission_approved_by_button_only_from_allowed_user() {
    let (svc, mock) = service().await;
    let res = svc
        .create(create(InteractionRequest::Permission {
            tool_name: "Bash".into(),
            details: "```\nnpm test\n```".into(),
        }))
        .await
        .unwrap();
    assert!(res.routed);
    let id = res.id.unwrap();
    let posted = mock.calls.lock().unwrap().clone();
    let prompt = posted
        .iter()
        .find(|(m, p, _)| m == "POST" && p.ends_with("/messages"))
        .unwrap();
    let buttons = &prompt.2["components"][0]["components"];
    assert_eq!(buttons[0]["custom_id"], format!("ap|{id}|allow"));

    // A stranger can't approve.
    svc.on_event(button(&format!("ap|{id}|allow"), STRANGER))
        .await;
    let state = svc
        .wait(&id, Duration::from_millis(50))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.status, "pending");

    // The owner can; the waiting hook sees it immediately.
    let waiter = {
        let svc = svc.clone();
        let id = id.clone();
        tokio::spawn(async move { svc.wait(&id, Duration::from_secs(5)).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    svc.on_event(button(&format!("ap|{id}|allow"), ME)).await;
    let state = waiter.await.unwrap().unwrap().unwrap();
    assert_eq!(state.status, "resolved");
    assert_eq!(
        state.response,
        Some(InteractionResponse::Permission {
            allow: true,
            message: None
        })
    );
    // Replays are rejected.
    svc.on_event(button(&format!("ap|{id}|deny"), ME)).await;
    let state = svc.wait(&id, Duration::ZERO).await.unwrap().unwrap();
    assert_eq!(
        state.response,
        Some(InteractionResponse::Permission {
            allow: true,
            message: None
        })
    );
}

#[tokio::test]
async fn permission_denied_by_text_reply_with_reason() {
    let (svc, _mock) = service().await;
    let id = svc
        .create(create(InteractionRequest::Permission {
            tool_name: "Bash".into(),
            details: "rm -rf build".into(),
        }))
        .await
        .unwrap()
        .id
        .unwrap();
    svc.on_event(message("deny: run make clean instead", ME, None))
        .await;
    let state = svc.wait(&id, Duration::ZERO).await.unwrap().unwrap();
    assert_eq!(
        state.response,
        Some(InteractionResponse::Permission {
            allow: false,
            message: Some("run make clean instead".into())
        })
    );
}

#[tokio::test]
async fn question_answered_with_option_button() {
    let (svc, _mock) = service().await;
    let id = svc
        .create(create(InteractionRequest::Question {
            questions: vec![Question {
                question: "Which provider?".into(),
                header: None,
                options: vec![
                    QuestionOption {
                        label: "Stripe".into(),
                        description: None,
                    },
                    QuestionOption {
                        label: "PayPal".into(),
                        description: None,
                    },
                ],
                multi_select: false,
            }],
        }))
        .await
        .unwrap()
        .id
        .unwrap();
    svc.on_event(button(&format!("ap|{id}|q0o1"), ME)).await;
    let state = svc.wait(&id, Duration::ZERO).await.unwrap().unwrap();
    match state.response {
        Some(InteractionResponse::Question { answers }) => {
            assert_eq!(answers["Which provider?"], "PayPal")
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn continue_reply_reaches_the_waiting_session_and_supersedes_older_waits() {
    let (svc, mock) = service().await;
    let first = svc
        .create(create(InteractionRequest::Continue { last_message: None }))
        .await
        .unwrap()
        .id
        .unwrap();
    let second = svc
        .create(create(InteractionRequest::Continue { last_message: None }))
        .await
        .unwrap()
        .id
        .unwrap();
    assert_eq!(
        svc.wait(&first, Duration::ZERO)
            .await
            .unwrap()
            .unwrap()
            .status,
        "cancelled"
    );
    // Continue listeners don't post a prompt of their own.
    assert!(!mock
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|(m, p, _)| m == "POST" && p.ends_with("/messages")));
    svc.on_event(message("great, now add tests for it", ME, None))
        .await;
    let state = svc.wait(&second, Duration::ZERO).await.unwrap().unwrap();
    assert_eq!(
        state.response,
        Some(InteractionResponse::Continue {
            text: "great, now add tests for it".into()
        })
    );
    // The reply was acknowledged with a ✅ reaction.
    assert!(mock
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|(m, p, _)| m == "PUT" && p.contains("/reactions/")));
}

#[tokio::test]
async fn strangers_are_ignored_and_commands_answered() {
    let (svc, mock) = service().await;
    let id = svc
        .create(create(InteractionRequest::Continue { last_message: None }))
        .await
        .unwrap()
        .id
        .unwrap();
    svc.on_event(message("rm -rf / please", STRANGER, None))
        .await;
    assert_eq!(
        svc.wait(&id, Duration::ZERO).await.unwrap().unwrap().status,
        "pending"
    );
    svc.on_event(message("status", ME, None)).await;
    let calls = mock.calls.lock().unwrap().clone();
    assert!(calls.iter().any(|(_, _, b)| b["content"]
        .as_str()
        .is_some_and(|c| c.contains("AgentPing"))));
}

#[tokio::test]
async fn presence_controls_routing_when_away_only() {
    let (base, _mock) = start_mock().await;
    let mut config = Config::default();
    config.remote.enabled = true;
    config.remote.discord_channel_id = Some(CHANNEL.into());
    config.remote.discord_user_ids = vec![ME.into()];
    config.remote.route = RemoteRoute::WhenAway;
    config.remote.away_after_minutes = 0;
    let db = Arc::new(Mutex::new(Database::open_in_memory().unwrap()));
    let api = DiscordApi::with_base(reqwest::Client::new(), Secret::new("t"), &base);
    let svc = RemoteService::new(db, Arc::new(config), api).unwrap();
    svc.on_event(GatewayEvent::Ready {
        bot_user_id: "bot".into(),
    })
    .await;
    let perm = || {
        create(InteractionRequest::Permission {
            tool_name: "Bash".into(),
            details: "ls".into(),
        })
    };
    assert!(
        !svc.create(perm()).await.unwrap().routed,
        "at keyboard: stays local"
    );
    svc.set_away(true).await.unwrap();
    assert!(
        svc.create(perm()).await.unwrap().routed,
        "away: routed to Discord"
    );
    svc.set_away(false).await.unwrap();
    assert!(!svc.create(perm()).await.unwrap().routed);
}
