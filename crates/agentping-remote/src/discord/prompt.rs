//! Discord messages for interactive prompts, and parsing of component ids.

use agentping_core::redact::redact;
use agentping_core::remote::{InteractionRequest, InteractionResponse, Question};
use agentping_notifiers::render::{char_len, split_text, truncate};
use serde_json::{json, Value};

const DESCRIPTION_LIMIT: usize = 3900;
const PREFIX: &str = "ap";

/// What a component press means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentAction {
    /// Approve a permission.
    Allow,
    /// Deny a permission.
    Deny,
    /// Pick option `option` of question `question`.
    Option {
        /// Question index.
        question: usize,
        /// Option index.
        option: usize,
    },
    /// Select-menu values for question `question`.
    Select {
        /// Question index.
        question: usize,
    },
}

/// Builds a component id: `ap|<interaction>|<action>`.
pub fn custom_id(interaction_id: &str, action: &ComponentAction) -> String {
    let a = match action {
        ComponentAction::Allow => "allow".to_owned(),
        ComponentAction::Deny => "deny".to_owned(),
        ComponentAction::Option { question, option } => format!("q{question}o{option}"),
        ComponentAction::Select { question } => format!("q{question}s"),
    };
    format!("{PREFIX}|{interaction_id}|{a}")
}

/// Parses a component id produced by [`custom_id`].
pub fn parse_custom_id(id: &str) -> Option<(String, ComponentAction)> {
    let mut parts = id.splitn(3, '|');
    if parts.next()? != PREFIX {
        return None;
    }
    let interaction = parts.next()?.to_owned();
    let action = parts.next()?;
    let parsed = match action {
        "allow" => ComponentAction::Allow,
        "deny" => ComponentAction::Deny,
        a => {
            let rest = a.strip_prefix('q')?;
            if let Some(q) = rest.strip_suffix('s') {
                ComponentAction::Select {
                    question: q.parse().ok()?,
                }
            } else {
                let (q, o) = rest.split_once('o')?;
                ComponentAction::Option {
                    question: q.parse().ok()?,
                    option: o.parse().ok()?,
                }
            }
        }
    };
    Some((interaction, parsed))
}

fn question_text(i: usize, q: &Question, total: usize) -> String {
    let mut out = String::new();
    let head = q
        .header
        .as_deref()
        .map(|h| format!(" · {h}"))
        .unwrap_or_default();
    if total > 1 {
        out.push_str(&format!("**Q{}{head}**\n", i + 1));
    }
    out.push_str(&format!("{}\n", q.question));
    for (n, o) in q.options.iter().enumerate() {
        match &o.description {
            Some(d) if !d.trim().is_empty() => {
                out.push_str(&format!("`{}` **{}** — {d}\n", n + 1, o.label))
            }
            _ => out.push_str(&format!("`{}` **{}**\n", n + 1, o.label)),
        }
    }
    if q.multi_select {
        out.push_str("_(multiple choices allowed)_\n");
    }
    out
}

/// Builds the messages for a prompt. Long details are split over several messages; the
/// buttons are attached to the last one, which is the one to track.
pub fn prompt_messages(
    interaction_id: &str,
    request: &InteractionRequest,
    agent_name: &str,
    project: Option<&str>,
    timeout_minutes: u64,
) -> Vec<Value> {
    let project_label = project.map(|p| format!(" · {p}")).unwrap_or_default();
    let (title, body, components, hint, color) = match request {
        InteractionRequest::Permission { tool_name, details } => (
            format!("🔐 {agent_name} wants to use {tool_name}{project_label}"),
            redact(details).into_owned(),
            vec![json!({
                "type": 1,
                "components": [
                    { "type": 2, "style": 3, "label": "Approve", "emoji": { "name": "✅" }, "custom_id": custom_id(interaction_id, &ComponentAction::Allow) },
                    { "type": 2, "style": 4, "label": "Deny", "emoji": { "name": "❌" }, "custom_id": custom_id(interaction_id, &ComponentAction::Deny) },
                ]
            })],
            "Tap a button, or reply to this message: `approve`, `deny`, or a reason (\"deny: use npm ci\") that Claude will see.".to_owned(),
            0xE67E22,
        ),
        InteractionRequest::Question { questions } => {
            let body: String = questions
                .iter()
                .enumerate()
                .map(|(i, q)| question_text(i, q, questions.len()))
                .collect::<Vec<_>>()
                .join("\n");
            let mut rows = Vec::new();
            for (qi, q) in questions.iter().enumerate().take(5) {
                if q.multi_select {
                    let options: Vec<Value> = q
                        .options
                        .iter()
                        .enumerate()
                        .take(25)
                        .map(|(oi, o)| {
                            json!({ "label": truncate(&o.label, 100), "value": oi.to_string(),
                                    "description": o.description.as_deref().map(|d| truncate(d, 100)) })
                        })
                        .collect();
                    let n = options.len().max(1);
                    rows.push(json!({ "type": 1, "components": [{
                        "type": 3,
                        "custom_id": custom_id(interaction_id, &ComponentAction::Select { question: qi }),
                        "placeholder": truncate(&format!("Q{}: choose one or more", qi + 1), 150),
                        "min_values": 1, "max_values": n, "options": options
                    }]}));
                } else {
                    let buttons: Vec<Value> = q
                        .options
                        .iter()
                        .enumerate()
                        .take(5)
                        .map(|(oi, o)| {
                            let label = if questions.len() > 1 {
                                format!("Q{} · {}", qi + 1, o.label)
                            } else {
                                o.label.clone()
                            };
                            json!({ "type": 2, "style": 1, "label": truncate(&label, 80),
                                    "custom_id": custom_id(interaction_id, &ComponentAction::Option { question: qi, option: oi }) })
                        })
                        .collect();
                    if !buttons.is_empty() {
                        rows.push(json!({ "type": 1, "components": buttons }));
                    }
                }
            }
            (
                format!("💬 {agent_name} is asking{project_label}"),
                body,
                rows,
                "Tap an option, or reply to this message with a number or your own answer.".to_owned(),
                0x9B59B6,
            )
        }
        InteractionRequest::Continue { .. } => (
            format!("↩️ {agent_name} is waiting{project_label}"),
            String::new(),
            Vec::new(),
            "Reply with new instructions.".to_owned(),
            0x95A5A6,
        ),
    };
    let mut chunks = split_text(&body, DESCRIPTION_LIMIT, &char_len);
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    let parts = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let last = i + 1 == parts;
            let footer = if last {
                format!("{hint} Expires in {timeout_minutes} min.")
            } else {
                format!("part {}/{parts}", i + 1)
            };
            let mut msg = json!({
                "embeds": [{
                    "title": truncate(&title, 256),
                    "description": chunk,
                    "color": color,
                    "footer": { "text": truncate(&footer, 2048) },
                }],
                "allowed_mentions": { "parse": [] },
            });
            if i == 0 {
                msg["content"] = Value::String(truncate(&title, 2000));
            }
            if last {
                msg["components"] = Value::Array(components.clone());
            }
            msg
        })
        .collect()
}

/// Replacement content for a prompt after it was answered or withdrawn (buttons removed).
pub fn closed_prompt(title: &str, outcome: &str, color: u32) -> Value {
    json!({
        "embeds": [{ "title": truncate(title, 256), "description": outcome, "color": color }],
        "components": [],
        "allowed_mentions": { "parse": [] },
    })
}

/// Human-readable summary of a response.
pub fn describe_response(response: &InteractionResponse) -> String {
    match response {
        InteractionResponse::Permission { allow: true, .. } => "✅ Approved".to_owned(),
        InteractionResponse::Permission {
            allow: false,
            message,
        } => match message {
            Some(m) => format!("❌ Denied — \"{m}\""),
            None => "❌ Denied".to_owned(),
        },
        InteractionResponse::Question { answers } => answers
            .iter()
            .map(|(q, a)| format!("**{q}** → {a}"))
            .collect::<Vec<_>>()
            .join("\n"),
        InteractionResponse::Continue { text } => {
            format!("↩️ Sent to Claude: {}", truncate(text, 1500))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentping_core::remote::QuestionOption;

    #[test]
    fn custom_ids_roundtrip() {
        for a in [
            ComponentAction::Allow,
            ComponentAction::Deny,
            ComponentAction::Option {
                question: 1,
                option: 3,
            },
            ComponentAction::Select { question: 2 },
        ] {
            let id = custom_id("abc123", &a);
            assert!(id.len() <= 100);
            assert_eq!(parse_custom_id(&id), Some(("abc123".to_owned(), a)));
        }
        assert_eq!(parse_custom_id("other|x|allow"), None);
        assert_eq!(parse_custom_id("ap|x|q1"), None);
    }

    #[test]
    fn permission_prompt_has_buttons_on_last_part() {
        let req = InteractionRequest::Permission {
            tool_name: "Bash".into(),
            details: format!("```\n{}\n```", "echo hi\n".repeat(1500)),
        };
        let msgs = prompt_messages("id1", &req, "Claude Code", Some("api"), 10);
        assert!(msgs.len() > 1);
        assert!(msgs[..msgs.len() - 1]
            .iter()
            .all(|m| m.get("components").is_none()));
        let buttons = &msgs.last().unwrap()["components"][0]["components"];
        assert_eq!(buttons[0]["custom_id"], "ap|id1|allow");
        assert_eq!(buttons[1]["custom_id"], "ap|id1|deny");
    }

    #[test]
    fn question_prompt_uses_buttons_or_select() {
        let q = |multi| Question {
            question: "Which provider?".into(),
            header: Some("Payments".into()),
            options: vec![
                QuestionOption {
                    label: "Stripe".into(),
                    description: Some("cards".into()),
                },
                QuestionOption {
                    label: "PayPal".into(),
                    description: None,
                },
            ],
            multi_select: multi,
        };
        let msgs = prompt_messages(
            "i",
            &InteractionRequest::Question {
                questions: vec![q(false), q(true)],
            },
            "Claude Code",
            None,
            10,
        );
        let rows = msgs.last().unwrap()["components"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(rows[0]["components"][0]["custom_id"], "ap|i|q0o0");
        assert_eq!(rows[1]["components"][0]["type"], 3);
        assert_eq!(rows[1]["components"][0]["max_values"], 2);
        let text = msgs[0]["embeds"][0]["description"].as_str().unwrap();
        assert!(text.contains("`1` **Stripe** — cards"));
    }
}
