//! Claude Code hook payload → AgentPing event normalization.
//!
//! Hook names and fields follow https://code.claude.com/docs/en/hooks. Parsing is tolerant:
//! every field is optional, unknown fields are ignored, and fields of unexpected types are
//! treated as missing.

use std::path::Path;

use agentping_core::config::ClaudeAgentConfig;
use agentping_core::remote::{
    describe_tool_call, InteractionRequest, InteractionResponse, Question, QuestionOption,
};
use agentping_core::{project, AgentKind, Event, EventType, Severity};
use serde_json::{json, Map, Value};

use crate::{AdapterContext, AdapterError, AgentAdapter, MAX_PAYLOAD_BYTES};

/// Integration identifier recorded in event sources.
pub const INTEGRATION_ID: &str = "claude-code-hooks";

/// The Claude Code tool used to ask the user questions.
pub const ASK_USER_QUESTION_TOOL: &str = "AskUserQuestion";

/// Claude Code adapter.
#[derive(Debug, Clone, Default)]
pub struct ClaudeAdapter {
    config: ClaudeAgentConfig,
}

impl ClaudeAdapter {
    /// Creates an adapter with the given integration settings.
    pub fn new(config: ClaudeAgentConfig) -> Self {
        Self { config }
    }
}

fn str_field<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

impl AgentAdapter for ClaudeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Claude
    }

    fn parse(&self, payload: &[u8], ctx: &AdapterContext) -> Result<Option<Event>, AdapterError> {
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(AdapterError::TooLarge);
        }
        if payload.iter().all(u8::is_ascii_whitespace) {
            return Err(AdapterError::Empty);
        }
        let value: Value = serde_json::from_slice(payload)
            .map_err(|e| AdapterError::InvalidJson(e.to_string()))?;
        let Value::Object(obj) = value else {
            return Err(AdapterError::NotAnObject);
        };
        let hook = str_field(&obj, "hook_event_name")
            .ok_or(AdapterError::MissingField("hook_event_name"))?;
        let Some(mut event) = self.map_event(hook, &obj) else {
            tracing::debug!(hook, "ignoring Claude Code hook event");
            return Ok(None);
        };
        event.message = event
            .message
            .as_deref()
            .map(agentping_core::event::clamp_message);
        event.occurred_at = ctx.received_at;
        event.source.integration = INTEGRATION_ID.to_owned();
        event.source.native_event = Some(hook.to_owned());
        event.session_id = str_field(&obj, "session_id").map(str::to_owned);
        event.cwd = str_field(&obj, "cwd").map(str::to_owned);

        // Prefer the project root Claude Code reports (CLAUDE_PROJECT_DIR); otherwise derive
        // it from the working directory.
        let explicit = ctx
            .project_dir
            .as_deref()
            .filter(|p| !p.as_os_str().is_empty())
            .and_then(|dir| {
                let name = dir.file_name()?.to_string_lossy().into_owned();
                Some(project::ProjectInfo {
                    name,
                    root: dir.to_path_buf(),
                })
            });
        let detected = explicit.or_else(|| {
            event
                .cwd
                .as_deref()
                .and_then(|c| project::detect(Path::new(c)))
        });
        if let Some(info) = detected {
            event.project_name = Some(info.name);
            event.project_path = Some(info.root.to_string_lossy().into_owned());
        }

        for key in [
            "permission_mode",
            "notification_type",
            "tool_name",
            "tool_use_id",
            "source",
            "reason",
            "error",
        ] {
            if let Some(v) = str_field(&obj, key) {
                event
                    .metadata
                    .insert(key.to_owned(), Value::String(v.to_owned()));
            }
        }
        Ok(Some(event))
    }
}

impl ClaudeAdapter {
    fn new_event(&self, t: EventType) -> Event {
        Event::new(t, AgentKind::Claude, chrono::Utc::now())
    }

    fn map_event(&self, hook: &str, obj: &Map<String, Value>) -> Option<Event> {
        match hook {
            "Notification" => Some(self.map_notification(obj)),
            "PreToolUse" if str_field(obj, "tool_name") == Some(ASK_USER_QUESTION_TOOL) => {
                Some(self.map_question(obj))
            }
            "PostToolUse" if str_field(obj, "tool_name") == Some(ASK_USER_QUESTION_TOOL) => {
                let mut e = self.new_event(EventType::Activity);
                e.delivery.cancels = question_cancel_key(obj);
                Some(e)
            }
            "PermissionRequest" => {
                // Context only: the `permission_prompt` notification (sent when the prompt has
                // waited a few seconds) is the user-facing ping, enriched with these details.
                let tool = str_field(obj, "tool_name").unwrap_or("a tool");
                let mut e = self.new_event(EventType::Activity);
                e.title = Some(format!("Permission requested: {tool}"));
                let details =
                    describe_tool_call(tool, obj.get("tool_input").unwrap_or(&Value::Null));
                e.message = (!details.is_empty()).then_some(details);
                Some(e)
            }
            "Stop" => {
                let mut e = self.new_event(EventType::TaskCompleted);
                e.message = str_field(obj, "last_assistant_message").map(str::to_owned);
                Some(e)
            }
            "StopFailure" => {
                let mut e = self.new_event(EventType::TaskFailed);
                let kind = str_field(obj, "error").unwrap_or("unknown");
                e.title = Some(format!("API error: {}", kind.replace('_', " ")));
                e.message = str_field(obj, "error_details")
                    .or_else(|| str_field(obj, "last_assistant_message"))
                    .map(str::to_owned)
                    .or_else(|| {
                        Some(format!(
                            "Claude Code stopped because of an API error ({kind})."
                        ))
                    });
                Some(e)
            }
            "SessionStart" => {
                let mut e = self.new_event(EventType::AgentStarted);
                e.source.subtype = str_field(obj, "source").map(str::to_owned);
                Some(e)
            }
            "SessionEnd" => {
                let mut e = self.new_event(EventType::AgentStopped);
                e.source.subtype = str_field(obj, "reason").map(str::to_owned);
                Some(e)
            }
            // The prompt text is deliberately never read or stored.
            "UserPromptSubmit" => Some(self.new_event(EventType::Activity)),
            _ => None,
        }
    }

    fn map_notification(&self, obj: &Map<String, Value>) -> Event {
        let subtype = str_field(obj, "notification_type");
        let (t, severity) = match subtype {
            Some("permission_prompt") => (EventType::PermissionRequest, Severity::High),
            Some("idle_prompt") => (EventType::AgentIdle, Severity::Low),
            Some("elicitation_dialog" | "elicitation_url_dialog" | "agent_needs_input") => {
                (EventType::UserQuestion, Severity::High)
            }
            Some(s) if s.starts_with("auth_") => (EventType::Notification, Severity::Low),
            Some(s) if s.starts_with("quota_") => (EventType::Notification, Severity::Normal),
            _ => (EventType::Notification, Severity::Normal),
        };
        let mut e = self.new_event(t);
        e.severity = severity;
        e.source.subtype = subtype.map(str::to_owned);
        e.title = str_field(obj, "title").map(str::to_owned);
        e.message = str_field(obj, "message").map(str::to_owned);
        e
    }

    fn map_question(&self, obj: &Map<String, Value>) -> Event {
        let mut e = self.new_event(EventType::UserQuestion);
        let questions = obj
            .get("tool_input")
            .and_then(|i| i.get("questions"))
            .and_then(Value::as_array);
        let mut lines = Vec::new();
        let mut headers = Vec::new();
        for q in questions.into_iter().flatten() {
            if let Some(text) = q.get("question").and_then(Value::as_str) {
                lines.push(text.trim().to_owned());
            }
            if let Some(h) = q.get("header").and_then(Value::as_str) {
                headers.push(h.trim().to_owned());
            }
            for opt in q
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(label) = opt.get("label").and_then(Value::as_str) else {
                    continue;
                };
                match opt.get("description").and_then(Value::as_str) {
                    Some(d) if !d.trim().is_empty() => {
                        lines.push(format!("• {label} — {}", d.trim()))
                    }
                    _ => lines.push(format!("• {label}")),
                }
            }
            lines.push(String::new());
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        if !lines.is_empty() {
            e.message = Some(lines.join("\n"));
        }
        if !headers.is_empty() {
            e.title = Some(headers.join(" · "));
        }
        if let Some(key) = question_cancel_key(obj) {
            e.delivery.delay_secs = self.config.question_delay_secs;
            e.delivery.cancel_key = Some(key);
        }
        e
    }
}

fn question_cancel_key(obj: &Map<String, Value>) -> Option<String> {
    let tool_use_id = str_field(obj, "tool_use_id")?;
    let session = str_field(obj, "session_id").unwrap_or("-");
    Some(format!("claude:{session}:{tool_use_id}"))
}

/// A remote interaction a hook payload can be turned into.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteCandidate {
    /// What to ask.
    pub request: InteractionRequest,
    /// Pending notification this prompt replaces.
    pub supersedes_cancel_key: Option<String>,
}

/// Extracts the remote interaction a Claude Code hook payload supports, if any:
/// `PermissionRequest` → approve/deny, `PreToolUse(AskUserQuestion)` → answers,
/// `Stop` → follow-up instructions.
pub fn remote_candidate(payload: &Value) -> Option<RemoteCandidate> {
    let obj = payload.as_object()?;
    match str_field(obj, "hook_event_name")? {
        "PermissionRequest" => {
            let tool = str_field(obj, "tool_name").unwrap_or("a tool").to_owned();
            let details = describe_tool_call(&tool, obj.get("tool_input").unwrap_or(&Value::Null));
            Some(RemoteCandidate {
                request: InteractionRequest::Permission {
                    tool_name: tool,
                    details,
                },
                supersedes_cancel_key: None,
            })
        }
        "PreToolUse" if str_field(obj, "tool_name") == Some(ASK_USER_QUESTION_TOOL) => {
            let raw = obj.get("tool_input")?.get("questions")?.as_array()?;
            let questions: Vec<Question> = raw
                .iter()
                .filter_map(|q| {
                    let question = q.get("question")?.as_str()?.trim().to_owned();
                    let options = q
                        .get("options")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|o| {
                            Some(QuestionOption {
                                label: o.get("label")?.as_str()?.to_owned(),
                                description: o
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned),
                            })
                        })
                        .collect();
                    Some(Question {
                        question,
                        header: q.get("header").and_then(Value::as_str).map(str::to_owned),
                        options,
                        multi_select: q
                            .get("multiSelect")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                })
                .collect();
            (!questions.is_empty()).then(|| RemoteCandidate {
                request: InteractionRequest::Question { questions },
                supersedes_cancel_key: question_cancel_key(obj),
            })
        }
        "Stop" => Some(RemoteCandidate {
            request: InteractionRequest::Continue {
                last_message: str_field(obj, "last_assistant_message").map(str::to_owned),
            },
            supersedes_cancel_key: None,
        }),
        _ => None,
    }
}

/// The hook output that applies a remote response, per the Claude Code hooks reference.
pub enum HookOutput {
    /// Print this JSON on stdout and exit 0.
    Json(Value),
    /// Print this text on stderr and exit 2 (wakes Claude when the hook uses `asyncRewake`).
    Wake(String),
}

/// Builds the hook output for a response to `payload`.
pub fn hook_output(
    payload: &Value,
    response: &InteractionResponse,
    responder: &str,
) -> Option<HookOutput> {
    let who = if responder.is_empty() {
        "the user".to_owned()
    } else {
        responder.to_owned()
    };
    match response {
        InteractionResponse::Permission { allow: true, .. } => Some(HookOutput::Json(json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": { "behavior": "allow" }
            }
        }))),
        InteractionResponse::Permission {
            allow: false,
            message,
        } => {
            let message = match message {
                Some(m) => {
                    format!("Denied remotely by {who} via AgentPing (Discord). Their feedback: {m}")
                }
                None => format!("Denied remotely by {who} via AgentPing (Discord)."),
            };
            Some(HookOutput::Json(json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": { "behavior": "deny", "message": message }
                }
            })))
        }
        InteractionResponse::Question { answers } => {
            let mut input = payload.get("tool_input")?.clone();
            input
                .as_object_mut()?
                .insert("answers".into(), json!(answers));
            Some(HookOutput::Json(json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "permissionDecisionReason": format!("Answered remotely by {who} via AgentPing (Discord)"),
                    "updatedInput": input
                }
            })))
        }
        InteractionResponse::Continue { text } => Some(HookOutput::Wake(format!(
            "The user ({who}) replied remotely via AgentPing (Discord) after you finished. \
             Treat this as their next message and continue:\n\n{text}"
        ))),
    }
}
