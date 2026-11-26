use std::time::Duration;

use agentping_core::{AgentKind, Config, Event, EventType, Severity};
use chrono::Utc;
use serde_json::json;

use super::*;
use crate::provider::{http_client, FailureKind};
use crate::render::render;
use crate::testutil::{start, Canned};

const TOKEN: &str = "123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw0";

fn notification() -> RenderedNotification {
    let mut e = Event::new(EventType::UserQuestion, AgentKind::Claude, Utc::now());
    e.project_name = Some("ecommerce-app".into());
    e.message = Some("Which payment provider should I implement? <Stripe & PayPal>".into());
    render(&e, Severity::High, &Config::default())
}

async fn provider_for(responses: Vec<Canned>) -> (TelegramProvider, crate::testutil::MockServer) {
    let server = start(responses).await;
    let base = Url::parse(&format!("{}/", server.base)).unwrap();
    (
        TelegramProvider::new_with_base(http_client().unwrap(), base, Secret::new(TOKEN), "42"),
        server,
    )
}

#[test]
fn token_validation() {
    assert!(validate_bot_token(TOKEN).is_ok());
    let err = validate_bot_token("nope:short").unwrap_err();
    assert!(!err.contains("nope"));
}

#[test]
fn html_is_escaped_and_structured() {
    let text = format_message(&notification());
    assert!(text.starts_with("<b>💬 Claude Code is asking a question</b>"));
    assert!(text.contains("&lt;Stripe &amp; PayPal&gt;"));
    assert!(text.contains("<b>Project:</b> ecommerce-app"));
}

#[test]
fn long_messages_fit_the_limit() {
    let mut n = notification();
    n.body = "<&>".repeat(5000);
    let text = format_message(&n);
    assert!(
        text.chars().count() <= MESSAGE_LIMIT,
        "{}",
        text.chars().count()
    );
    assert!(!text.contains("&am…"), "entity cut in half");
}

#[tokio::test]
async fn sends_message_to_bot_endpoint() {
    let (provider, server) =
        provider_for(vec![Canned::json(200, json!({ "ok": true, "result": {} }))]).await;
    provider.send(&notification()).await.unwrap();
    let reqs = server.requests.lock().unwrap();
    assert_eq!(reqs[0].path_and_query, format!("/bot{TOKEN}/sendMessage"));
    assert_eq!(reqs[0].body["chat_id"], "42");
    assert_eq!(reqs[0].body["parse_mode"], "HTML");
}

#[tokio::test]
async fn rate_limits_and_errors_are_classified() {
    let (provider, _s) = provider_for(vec![Canned::json(
        429,
        json!({ "ok": false, "error_code": 429, "description": "Too Many Requests: retry after 7", "parameters": { "retry_after": 7 } }),
    )])
    .await;
    let err = provider.send(&notification()).await.unwrap_err();
    assert_eq!(
        err.kind,
        FailureKind::Retryable {
            retry_after: Some(Duration::from_secs(7))
        }
    );

    let (provider, _s) = provider_for(vec![Canned::json(
        400,
        json!({ "ok": false, "error_code": 400, "description": "Bad Request: chat not found" }),
    )])
    .await;
    let err = provider.send(&notification()).await.unwrap_err();
    assert_eq!(err.kind, FailureKind::Permanent);
    assert!(err.message.contains("chat_id"));

    let (provider, _s) = provider_for(vec![Canned::json(
        401,
        json!({ "ok": false, "error_code": 401, "description": "Unauthorized" }),
    )])
    .await;
    let err = provider.send(&notification()).await.unwrap_err();
    assert_eq!(err.kind, FailureKind::Permanent);
    assert!(!err.message.contains(TOKEN));
}

#[tokio::test]
async fn transport_errors_never_contain_the_token() {
    let base = Url::parse("http://127.0.0.1:9/").unwrap();
    let provider =
        TelegramProvider::new_with_base(http_client().unwrap(), base, Secret::new(TOKEN), "42");
    let err = provider.send(&notification()).await.unwrap_err();
    assert!(
        !err.message.contains("AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw0"),
        "{}",
        err.message
    );
    let health = provider.health_check().await;
    assert!(!health
        .detail
        .contains("AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw0"));
}

#[tokio::test]
async fn health_and_chat_discovery() {
    let (provider, _s) = provider_for(vec![Canned::json(
        200,
        json!({ "ok": true, "result": { "username": "agentping_bot" } }),
    )])
    .await;
    let health = provider.health_check().await;
    assert!(health.healthy);
    assert_eq!(health.detail, "bot @agentping_bot is reachable");

    let (provider, _s) = provider_for(vec![Canned::json(
        200,
        json!({ "ok": true, "result": [
            { "update_id": 1, "message": { "chat": { "id": 1001, "type": "private", "first_name": "Ada" } } },
            { "update_id": 2, "message": { "chat": { "id": 1001, "type": "private", "first_name": "Ada" } } },
            { "update_id": 3, "channel_post": { "chat": { "id": -100200, "type": "channel", "title": "alerts" } } }
        ]}),
    )])
    .await;
    let chats = provider.discover_chats().await.unwrap();
    assert_eq!(chats.len(), 2);
    assert_eq!(chats[0].id, "1001");
    assert_eq!(chats[0].label, "Ada");
    assert_eq!(chats[1].kind, "channel");
}
