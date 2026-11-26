use std::path::{Path, PathBuf};

use agentping_core::config::ClaudeAgentConfig;
use agentping_core::{EventType, Severity};

use super::*;
use crate::{AdapterContext, AdapterError, AgentAdapter};

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/claude")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn parse(name: &str) -> Result<Option<agentping_core::Event>, AdapterError> {
    ClaudeAdapter::new(ClaudeAgentConfig::default()).parse(&fixture(name), &AdapterContext::now())
}

fn event(name: &str) -> agentping_core::Event {
    parse(name).unwrap().unwrap()
}

#[test]
fn permission_prompt_becomes_permission_request() {
    let e = event("notification_permission_prompt.json");
    assert_eq!(e.event_type, EventType::PermissionRequest);
    assert_eq!(e.severity, Severity::High);
    assert_eq!(
        e.message.as_deref(),
        Some("Claude needs your permission to use Bash")
    );
    assert_eq!(
        e.session_id.as_deref(),
        Some("5f0c8b2e-3d4a-4b7e-9f1a-2c6d8e0b1a34")
    );
    assert_eq!(e.project_name.as_deref(), Some("backend-api"));
    assert_eq!(e.source.native_event.as_deref(), Some("Notification"));
    assert_eq!(e.source.subtype.as_deref(), Some("permission_prompt"));
    assert_eq!(e.metadata["permission_mode"], "default");
    assert!(e.validate().is_ok());
}

#[test]
fn idle_and_elicitation_subtypes_are_distinguished() {
    let idle = event("notification_idle_prompt.json");
    assert_eq!(idle.event_type, EventType::AgentIdle);
    assert_eq!(idle.severity, Severity::Low);
    let ask = event("notification_elicitation.json");
    assert_eq!(ask.event_type, EventType::UserQuestion);
    assert_eq!(ask.title.as_deref(), Some("MCP server needs input"));
}

#[test]
fn unknown_notification_type_is_a_generic_notification() {
    let e = event("notification_unknown_type.json");
    assert_eq!(e.event_type, EventType::Notification);
    assert_eq!(
        e.source.subtype.as_deref(),
        Some("brand_new_type_from_the_future")
    );
    assert!(
        !e.metadata.contains_key("some_future_field"),
        "unknown fields are not copied"
    );
}

#[test]
fn ask_user_question_is_delayed_and_cancellable() {
    let q = event("pre_tool_use_ask_user_question.json");
    assert_eq!(q.event_type, EventType::UserQuestion);
    let msg = q.message.as_deref().unwrap();
    assert!(msg.starts_with("Which payment provider should I implement?"));
    assert!(msg.contains("• Stripe — Cards and wallets, best docs"));
    assert_eq!(q.title.as_deref(), Some("Payments"));
    assert_eq!(q.delivery.delay_secs, 15);
    let key = q.delivery.cancel_key.clone().unwrap();
    assert_eq!(
        key,
        "claude:8a1d2c3b-0000-4000-8000-000000000001:toolu_01ABCDEF"
    );

    let answered = event("post_tool_use_ask_user_question.json");
    assert_eq!(answered.event_type, EventType::Activity);
    assert_eq!(answered.delivery.cancels.as_deref(), Some(key.as_str()));
}

#[test]
fn question_delay_follows_config() {
    let adapter = ClaudeAdapter::new(ClaudeAgentConfig {
        enabled: true,
        question_delay_secs: 0,
    });
    let q = adapter
        .parse(
            &fixture("pre_tool_use_ask_user_question.json"),
            &AdapterContext::now(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(q.delivery.delay_secs, 0);
}

#[test]
fn other_tools_and_subagents_are_ignored() {
    assert!(parse("pre_tool_use_bash.json").unwrap().is_none());
    assert!(parse("subagent_stop.json").unwrap().is_none());
}

#[test]
fn stop_and_stop_failure() {
    let done = event("stop.json");
    assert_eq!(done.event_type, EventType::TaskCompleted);
    assert!(done.message.as_deref().unwrap().contains("rate limiting"));
    let failed = event("stop_failure.json");
    assert_eq!(failed.event_type, EventType::TaskFailed);
    assert_eq!(failed.title.as_deref(), Some("API error: rate limit"));
    assert_eq!(failed.message.as_deref(), Some("429 Too Many Requests"));
    assert_eq!(failed.metadata["error"], "rate_limit");
}

#[test]
fn session_lifecycle() {
    let start = event("session_start.json");
    assert_eq!(start.event_type, EventType::AgentStarted);
    assert_eq!(start.source.subtype.as_deref(), Some("startup"));
    let end = event("session_end.json");
    assert_eq!(end.event_type, EventType::AgentStopped);
    assert_eq!(end.source.subtype.as_deref(), Some("prompt_input_exit"));
}

#[test]
fn user_prompt_text_is_never_captured() {
    let e = event("user_prompt_submit.json");
    assert_eq!(e.event_type, EventType::Activity);
    let json = serde_json::to_string(&e).unwrap();
    assert!(!json.contains("hunter2"), "prompt leaked: {json}");
    assert!(e.message.is_none());
}

#[test]
fn missing_and_mistyped_fields_are_tolerated() {
    let e = event("missing_optional_fields.json");
    assert_eq!(e.event_type, EventType::Notification);
    assert!(e.session_id.is_none() && e.project_name.is_none() && e.message.is_none());

    let e = event("wrong_field_types.json");
    assert!(e.session_id.is_none());
    assert!(e.message.is_none());
    assert!(e.cwd.is_none());
}

#[test]
fn malformed_payloads_are_errors_not_panics() {
    assert!(matches!(
        parse("invalid_truncated.json"),
        Err(AdapterError::InvalidJson(_))
    ));
    assert_eq!(
        parse("not_an_object.json").unwrap_err(),
        AdapterError::NotAnObject
    );
    assert_eq!(
        parse("missing_event_name.json").unwrap_err(),
        AdapterError::MissingField("hook_event_name")
    );
    let adapter = ClaudeAdapter::default();
    assert_eq!(
        adapter.parse(b"  \n", &AdapterContext::now()).unwrap_err(),
        AdapterError::Empty
    );
    let huge = vec![b' '; crate::MAX_PAYLOAD_BYTES + 1];
    assert_eq!(
        adapter.parse(&huge, &AdapterContext::now()).unwrap_err(),
        AdapterError::TooLarge
    );
}

#[test]
fn claude_project_dir_takes_precedence() {
    let ctx = AdapterContext {
        received_at: chrono::Utc::now(),
        project_dir: Some(PathBuf::from("/work/monorepo")),
    };
    let e = ClaudeAdapter::default()
        .parse(&fixture("stop.json"), &ctx)
        .unwrap()
        .unwrap();
    assert_eq!(e.project_name.as_deref(), Some("monorepo"));
    assert_eq!(e.cwd.as_deref(), Some("/Users/dev/backend-api"));
}

#[test]
fn every_fixture_parses_without_panicking() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/claude");
    let adapter = ClaudeAdapter::default();
    for entry in std::fs::read_dir(dir).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        if let Ok(Some(e)) = adapter.parse(&bytes, &AdapterContext::now()) {
            assert!(e.validate().is_ok());
        }
    }
}
