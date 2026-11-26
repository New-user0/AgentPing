use std::time::Duration;

use agentping_core::{AgentKind, Config, Event, EventType, Severity};
use chrono::Utc;
use serde_json::json;

use super::*;
use crate::provider::{http_client, FailureKind};
use crate::render::char_len;
use crate::render::render;
use crate::testutil::{start, Canned};

fn notification() -> RenderedNotification {
    let mut e = Event::new(EventType::PermissionRequest, AgentKind::Claude, Utc::now());
    e.project_name = Some("backend-api".into());
    e.session_id = Some("abcdef0123".into());
    e.message = Some("Claude needs your permission to use Bash @everyone".into());
    render(&e, Severity::High, &Config::default())
}

async fn provider_for(responses: Vec<Canned>) -> (DiscordProvider, crate::testutil::MockServer) {
    let server = start(responses).await;
    let url = Url::parse(&format!("{}/api/webhooks/123/tok", server.base)).unwrap();
    (
        DiscordProvider::new_unchecked(http_client().unwrap(), url, "AgentPing"),
        server,
    )
}

#[test]
fn webhook_validation_accepts_real_shapes() {
    let token = "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_";
    for url in [
        format!("https://discord.com/api/webhooks/123456/{token}"),
        format!("https://discordapp.com/api/webhooks/123456/{token}"),
        format!("https://ptb.discord.com/api/v10/webhooks/123456/{token}"),
        format!("https://canary.discord.com/api/webhooks/123456/{token}/"),
    ] {
        assert!(validate_webhook_url(&url).is_ok(), "{url}");
    }
}

#[test]
fn webhook_validation_rejects_abuse_without_leaking_token() {
    let token = "SECRETTOKENSECRETTOKENSECRET";
    let cases = [
        format!("http://discord.com/api/webhooks/1/{token}"),
        format!("https://evil.example/api/webhooks/1/{token}"),
        format!("https://discord.com.evil.example/api/webhooks/1/{token}"),
        format!("https://discord.com:8443/api/webhooks/1/{token}"),
        format!("https://user:pw@discord.com/api/webhooks/1/{token}"),
        format!("https://discord.com/api/webhooks/1/{token}?x=1"),
        format!("https://discord.com/api/channels/1/{token}"),
        format!("https://discord.com/api/webhooks/abc/{token}"),
        "https://discord.com/api/webhooks/1/short".to_owned(),
        "not a url".to_owned(),
    ];
    for url in cases {
        let err = validate_webhook_url(&url).unwrap_err();
        assert!(!err.contains(token), "error leaked token: {err}");
    }
}

#[test]
fn payload_has_embed_push_preview_and_blocks_mentions() {
    let payloads = build_payloads(&notification(), "AgentPing");
    assert_eq!(payloads.len(), 1);
    let p = &payloads[0];
    assert_eq!(p["username"], "AgentPing");
    assert_eq!(p["allowed_mentions"], json!({ "parse": [] }));
    assert_eq!(
        p["content"],
        "🔐 Claude Code needs permission · backend-api"
    );
    let embed = &p["embeds"][0];
    assert_eq!(embed["title"], "🔐 Claude Code needs permission");
    assert_eq!(embed["color"], 0xE67E22);
    assert_eq!(embed["fields"][0]["name"], "Project");
    assert_eq!(embed["fields"][0]["value"], "backend-api");
    assert_eq!(embed["footer"]["text"], "AgentPing · session abcdef01");
    assert!(embed["timestamp"].as_str().unwrap().contains('T'));
}

#[test]
fn long_messages_are_split_not_truncated() {
    let mut n = notification();
    n.title = "t".repeat(1000);
    let paragraph = "lorem ipsum dolor sit amet ".repeat(40);
    n.body = format!(
        "{paragraph}\n```rust\n{}\n```\n{paragraph}",
        "let x = 1;\n".repeat(900)
    ) + &paragraph.repeat(10);
    n.fields = (0..40)
        .map(|i| crate::render::Field {
            name: format!("f{i}"),
            value: "v".repeat(2000),
        })
        .collect();
    let payloads = build_payloads(&n, "AgentPing");
    assert!(payloads.len() > 2, "{} parts", payloads.len());
    let mut all_text = String::new();
    for (i, p) in payloads.iter().enumerate() {
        let embed = &p["embeds"][0];
        let title = embed["title"].as_str().unwrap();
        let desc = embed["description"].as_str().unwrap();
        let footer = embed["footer"]["text"].as_str().unwrap();
        assert!(char_len(title) <= TITLE_LIMIT);
        assert!(char_len(desc) <= DESCRIPTION_LIMIT);
        assert!(footer.ends_with(&format!("part {}/{}", i + 1, payloads.len())));
        assert_eq!(
            desc.matches("```").count() % 2,
            0,
            "fences balanced in part {i}"
        );
        let fields: usize = embed["fields"]
            .as_array()
            .map(|f| {
                f.iter()
                    .map(|f| {
                        char_len(f["name"].as_str().unwrap())
                            + char_len(f["value"].as_str().unwrap())
                    })
                    .sum()
            })
            .unwrap_or(0);
        assert!(char_len(title) + char_len(desc) + char_len(footer) + fields <= EMBED_TOTAL_LIMIT);
        assert_eq!(
            p.get("content").is_some(),
            i == 0,
            "push preview only on part 1"
        );
        all_text.push_str(&desc.replace("```rust", "").replace("```", ""));
    }
    let squash = |s: &str| s.split_whitespace().collect::<String>();
    assert_eq!(
        squash(&all_text),
        squash(&n.body.replace("```rust", "").replace("```", "")),
        "no content lost"
    );
}

#[tokio::test]
async fn long_message_is_sent_as_multiple_parts() {
    let (provider, server) = provider_for(vec![Canned::json(200, json!({ "id": "1" }))]).await;
    let mut n = notification();
    n.body = "word ".repeat(2000);
    provider.send(&n).await.unwrap();
    assert_eq!(server.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn sends_to_webhook_with_wait_parameter() {
    let (provider, server) = provider_for(vec![Canned::json(200, json!({ "id": "1" }))]).await;
    let delivered = provider.send(&notification()).await.unwrap();
    assert_eq!(delivered.http_status, Some(200));
    let reqs = server.requests.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[0].path_and_query, "/api/webhooks/123/tok?wait=true");
    assert_eq!(
        reqs[0].body["embeds"][0]["fields"][0]["value"],
        "backend-api"
    );
}

#[tokio::test]
async fn rate_limit_is_retryable_with_retry_after() {
    let (provider, _server) = provider_for(vec![Canned::json(
        429,
        json!({ "message": "You are being rate limited.", "retry_after": 1.5, "global": false }),
    )])
    .await;
    let err = provider.send(&notification()).await.unwrap_err();
    assert_eq!(err.http_status, Some(429));
    assert_eq!(
        err.kind,
        FailureKind::Retryable {
            retry_after: Some(Duration::from_millis(1500))
        }
    );
}

#[tokio::test]
async fn server_errors_retry_and_client_errors_are_permanent() {
    let (provider, _s) = provider_for(vec![Canned::json(502, json!({}))]).await;
    let err = provider.send(&notification()).await.unwrap_err();
    assert!(matches!(err.kind, FailureKind::Retryable { .. }));

    let (provider, _s) = provider_for(vec![Canned::json(
        404,
        json!({ "message": "Unknown Webhook", "code": 10015 }),
    )])
    .await;
    let err = provider.send(&notification()).await.unwrap_err();
    assert_eq!(err.kind, FailureKind::Permanent);
    assert!(err.message.contains("Unknown Webhook"));
    assert!(err.message.contains("agentping setup discord"));
}

#[tokio::test]
async fn transport_errors_do_not_leak_the_webhook_url() {
    let url = Url::parse("http://127.0.0.1:9/api/webhooks/123/SECRETTOKENVALUE").unwrap();
    let provider = DiscordProvider::new_unchecked(http_client().unwrap(), url, "AgentPing");
    let err = provider.send(&notification()).await.unwrap_err();
    assert!(matches!(err.kind, FailureKind::Retryable { .. }));
    assert!(!err.message.contains("SECRETTOKENVALUE"), "{}", err.message);
    let health = provider.health_check().await;
    assert!(!health.healthy);
    assert!(!health.detail.contains("SECRETTOKENVALUE"));
}

#[tokio::test]
async fn health_check_reports_webhook_name_only() {
    let (provider, server) = provider_for(vec![Canned::json(
        200,
        json!({ "name": "agent-alerts", "token": "SHOULD_NOT_APPEAR", "channel_id": "1" }),
    )])
    .await;
    let health = provider.health_check().await;
    assert!(health.healthy);
    assert_eq!(health.detail, "webhook 'agent-alerts' is reachable");
    assert_eq!(server.requests.lock().unwrap()[0].method, "GET");
}
