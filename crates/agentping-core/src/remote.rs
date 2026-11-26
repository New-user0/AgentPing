//! Types shared by hooks, the daemon and remote channels for two-way interaction.
//!
//! Remote interaction is disabled by default. When enabled, an agent hook can register an
//! [`InteractionRequest`] with the daemon, which shows it on a remote channel (Discord) and
//! returns the user's [`InteractionResponse`] to the waiting hook. Only actions the agent's
//! hook API supports are modelled: permission decisions, answers to multiple-choice
//! questions, and follow-up instructions after the agent stopped.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One option of a multiple-choice question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    /// Label (also the answer value).
    pub label: String,
    /// Optional explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A multiple-choice question asked by the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// Question text (also the key in the answers map).
    pub question: String,
    /// Short header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// Options.
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    /// Whether several options may be chosen.
    #[serde(default)]
    pub multi_select: bool,
}

/// What the agent is waiting for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InteractionRequest {
    /// A tool needs permission.
    Permission {
        /// Tool name (e.g. `Bash`).
        tool_name: String,
        /// Human-readable description of what the tool will do (command, file, URL…).
        details: String,
    },
    /// The agent asked one or more multiple-choice questions.
    Question {
        /// The questions.
        questions: Vec<Question>,
    },
    /// The agent stopped; the user may send follow-up instructions.
    Continue {
        /// The agent's final message, if available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_message: Option<String>,
    },
}

/// The user's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InteractionResponse {
    /// Permission decision.
    Permission {
        /// Whether the tool may run.
        allow: bool,
        /// Optional feedback for the agent (reason for a denial).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Answers keyed by question text (multi-select answers joined with ", ").
    Question {
        /// Answers.
        answers: BTreeMap<String, String>,
    },
    /// Follow-up instructions.
    Continue {
        /// The user's text.
        text: String,
    },
}

/// Body of `POST /v1/interactions` (hook → daemon).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInteraction {
    /// Agent id.
    pub agent: String,
    /// Agent display name.
    pub agent_name: String,
    /// Session id.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Project name.
    #[serde(default)]
    pub project_name: Option<String>,
    /// What is being asked.
    pub request: InteractionRequest,
    /// How long the hook will wait.
    pub timeout_secs: u64,
    /// A pending notification this prompt replaces (e.g. a delayed question ping).
    #[serde(default)]
    pub supersedes_cancel_key: Option<String>,
}

/// Response of `POST /v1/interactions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInteractionResult {
    /// Whether the prompt was routed to a remote channel. When false the hook should let the
    /// agent continue normally (local prompt).
    pub routed: bool,
    /// Interaction id (when routed).
    #[serde(default)]
    pub id: Option<String>,
    /// Why it was not routed.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Response of `GET /v1/interactions/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionState {
    /// `pending`, `resolved`, `cancelled`, `expired`.
    pub status: String,
    /// The answer, once resolved.
    #[serde(default)]
    pub response: Option<InteractionResponse>,
    /// Who answered.
    #[serde(default)]
    pub responder: Option<String>,
}

/// Builds a readable description of a tool call for permission prompts.
pub fn describe_tool_call(tool_name: &str, input: &Value) -> String {
    let s = |k: &str| {
        input
            .get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let mut out = String::new();
    match tool_name {
        "Bash" | "PowerShell" => {
            if let Some(d) = s("description") {
                out.push_str(&format!("{d}\n"));
            }
            if let Some(c) = s("command") {
                out.push_str(&format!("```\n{c}\n```"));
            }
        }
        "Edit" | "MultiEdit" | "Write" | "Read" | "NotebookEdit" => {
            if let Some(p) = s("file_path").or_else(|| s("notebook_path")) {
                out.push_str(&format!("File: `{p}`\n"));
            }
            if let (Some(old), Some(new)) = (s("old_string"), s("new_string")) {
                let diff: String = old
                    .lines()
                    .map(|l| format!("- {l}\n"))
                    .chain(new.lines().map(|l| format!("+ {l}\n")))
                    .collect();
                out.push_str(&format!("```diff\n{diff}```"));
            } else if let Some(edits) = input.get("edits").and_then(Value::as_array) {
                out.push_str(&format!("{} edit(s)\n", edits.len()));
                for e in edits {
                    let old = e.get("old_string").and_then(Value::as_str).unwrap_or("");
                    let new = e.get("new_string").and_then(Value::as_str).unwrap_or("");
                    let diff: String = old
                        .lines()
                        .map(|l| format!("- {l}\n"))
                        .chain(new.lines().map(|l| format!("+ {l}\n")))
                        .collect();
                    out.push_str(&format!("```diff\n{diff}```\n"));
                }
            } else if let Some(content) = s("content").or_else(|| s("new_source")) {
                out.push_str(&format!("```\n{content}\n```"));
            }
        }
        "WebFetch" => {
            if let Some(u) = s("url") {
                out.push_str(&format!("URL: {u}\n"));
            }
            if let Some(p) = s("prompt") {
                out.push_str(&format!("Prompt: {p}"));
            }
        }
        "WebSearch" => {
            if let Some(q) = s("query") {
                out.push_str(&format!("Search: {q}"));
            }
        }
        _ => {}
    }
    if out.trim().is_empty() && !input.is_null() {
        let pretty = serde_json::to_string_pretty(input).unwrap_or_default();
        out = format!("```json\n{pretty}\n```");
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn describes_common_tools() {
        let bash = describe_tool_call(
            "Bash",
            &json!({ "command": "rm -rf node_modules", "description": "Remove node_modules" }),
        );
        assert_eq!(bash, "Remove node_modules\n```\nrm -rf node_modules\n```");
        let edit = describe_tool_call(
            "Edit",
            &json!({ "file_path": "src/a.rs", "old_string": "a", "new_string": "b" }),
        );
        assert!(edit.contains("File: `src/a.rs`"));
        assert!(edit.contains("- a\n+ b"));
        let mcp = describe_tool_call("mcp__github__create_issue", &json!({ "title": "x" }));
        assert!(mcp.contains("\"title\": \"x\""));
    }

    #[test]
    fn requests_and_responses_roundtrip() {
        let r = InteractionRequest::Question {
            questions: vec![Question {
                question: "Which?".into(),
                header: Some("Pick".into()),
                options: vec![QuestionOption {
                    label: "A".into(),
                    description: None,
                }],
                multi_select: false,
            }],
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["kind"], "question");
        assert_eq!(serde_json::from_value::<InteractionRequest>(v).unwrap(), r);
        let resp = InteractionResponse::Permission {
            allow: false,
            message: Some("use npm ci".into()),
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["kind"], "permission");
        assert_eq!(
            serde_json::from_value::<InteractionResponse>(v).unwrap(),
            resp
        );
    }
}
