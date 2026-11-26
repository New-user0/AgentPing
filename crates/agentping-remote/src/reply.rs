//! Interpreting free-text replies and component choices as interaction responses.

use std::collections::BTreeMap;

use agentping_core::remote::{InteractionRequest, InteractionResponse, Question};

/// Maximum length of a reply forwarded to the agent.
pub const MAX_REPLY_CHARS: usize = 4000;

/// Commands understood in the remote channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Show help.
    Help,
    /// Show sessions and pending prompts.
    Status,
    /// Route prompts to Discord.
    Away,
    /// Stop routing prompts (when route = when_away).
    Back,
}

/// Recognizes a whole-message command.
pub fn parse_command(text: &str) -> Option<Command> {
    match text
        .trim()
        .trim_start_matches(['!', '/'])
        .to_ascii_lowercase()
        .as_str()
    {
        "help" | "?" => Some(Command::Help),
        "status" | "sessions" => Some(Command::Status),
        "away" | "afk" => Some(Command::Away),
        "back" | "here" => Some(Command::Back),
        _ => None,
    }
}

fn clean(text: &str) -> String {
    let t = text.trim();
    t.chars().take(MAX_REPLY_CHARS).collect()
}

/// Interprets a permission reply.
///
/// `approve`/`allow`/`yes`/`y`/`ok` approve; `deny`/`no`/`n`/`reject` deny; `deny: reason` or any
/// other text denies and passes the text to the agent as feedback.
pub fn permission_reply(text: &str) -> InteractionResponse {
    let t = clean(text);
    let lower = t.to_ascii_lowercase();
    let word = lower.trim_end_matches(['.', '!']);
    if matches!(
        word,
        "approve" | "approved" | "allow" | "yes" | "y" | "ok" | "okay" | "go" | "👍" | "✅"
    ) {
        return InteractionResponse::Permission {
            allow: true,
            message: None,
        };
    }
    if matches!(
        word,
        "deny" | "denied" | "no" | "n" | "reject" | "stop" | "👎" | "❌"
    ) {
        return InteractionResponse::Permission {
            allow: false,
            message: None,
        };
    }
    let reason = ["deny:", "no:", "reject:", "deny -", "no,"]
        .iter()
        .find_map(|p| lower.starts_with(p).then(|| t[p.len()..].trim().to_owned()))
        .unwrap_or(t);
    InteractionResponse::Permission {
        allow: false,
        message: (!reason.is_empty()).then_some(reason),
    }
}

/// Resolves the option a reply refers to: a 1-based number or a (case-insensitive) label.
fn match_option(q: &Question, text: &str) -> Option<String> {
    let t = text.trim();
    if let Ok(n) = t.parse::<usize>() {
        return q.options.get(n.checked_sub(1)?).map(|o| o.label.clone());
    }
    q.options
        .iter()
        .find(|o| o.label.eq_ignore_ascii_case(t))
        .map(|o| o.label.clone())
}

/// Interprets a free-text answer to the next unanswered question. Returns the updated answers.
///
/// Numbers and labels select options (comma separated for multi-select); anything else is
/// used verbatim as a custom ("Other") answer.
pub fn question_reply(
    questions: &[Question],
    answers: &BTreeMap<String, String>,
    text: &str,
) -> BTreeMap<String, String> {
    let mut out = answers.clone();
    let Some(q) = questions.iter().find(|q| !out.contains_key(&q.question)) else {
        return out;
    };
    let t = clean(text);
    let answer = if q.multi_select && t.contains(',') {
        let picked: Option<Vec<String>> = t.split(',').map(|p| match_option(q, p)).collect();
        picked.map(|v| v.join(", ")).unwrap_or_else(|| t.clone())
    } else {
        match_option(q, &t).unwrap_or_else(|| t.clone())
    };
    out.insert(q.question.clone(), answer);
    out
}

/// Whether every question has an answer.
pub fn all_answered(questions: &[Question], answers: &BTreeMap<String, String>) -> bool {
    questions.iter().all(|q| answers.contains_key(&q.question))
}

/// Interprets a reply for any request kind (questions take the partial `answers`).
pub fn interpret(
    request: &InteractionRequest,
    answers: &BTreeMap<String, String>,
    text: &str,
) -> Option<InteractionResponse> {
    let t = clean(text);
    if t.is_empty() {
        return None;
    }
    Some(match request {
        InteractionRequest::Permission { .. } => permission_reply(&t),
        InteractionRequest::Question { questions } => InteractionResponse::Question {
            answers: question_reply(questions, answers, &t),
        },
        InteractionRequest::Continue { .. } => InteractionResponse::Continue { text: t },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentping_core::remote::QuestionOption;

    fn q(text: &str, multi: bool) -> Question {
        Question {
            question: text.into(),
            header: None,
            options: ["Stripe", "PayPal", "Adyen"]
                .iter()
                .map(|l| QuestionOption {
                    label: (*l).into(),
                    description: None,
                })
                .collect(),
            multi_select: multi,
        }
    }

    #[test]
    fn permission_words() {
        assert_eq!(
            permission_reply("Approve"),
            InteractionResponse::Permission {
                allow: true,
                message: None
            }
        );
        assert_eq!(
            permission_reply("yes!"),
            InteractionResponse::Permission {
                allow: true,
                message: None
            }
        );
        assert_eq!(
            permission_reply("no"),
            InteractionResponse::Permission {
                allow: false,
                message: None
            }
        );
        assert_eq!(
            permission_reply("deny: use npm ci instead"),
            InteractionResponse::Permission {
                allow: false,
                message: Some("use npm ci instead".into())
            }
        );
        assert_eq!(
            permission_reply("don't delete that folder"),
            InteractionResponse::Permission {
                allow: false,
                message: Some("don't delete that folder".into())
            }
        );
    }

    #[test]
    fn question_answers_by_number_label_or_custom() {
        let qs = vec![q("Provider?", false), q("Extras?", true)];
        let a = question_reply(&qs, &BTreeMap::new(), "2");
        assert_eq!(a["Provider?"], "PayPal");
        assert!(!all_answered(&qs, &a));
        let a = question_reply(&qs, &a, "1, adyen");
        assert_eq!(a["Extras?"], "Stripe, Adyen");
        assert!(all_answered(&qs, &a));
        let custom = question_reply(&qs[..1], &BTreeMap::new(), "use Mollie");
        assert_eq!(custom["Provider?"], "use Mollie");
        let out_of_range = question_reply(&qs[..1], &BTreeMap::new(), "9");
        assert_eq!(out_of_range["Provider?"], "9");
    }

    #[test]
    fn commands_and_limits() {
        assert_eq!(parse_command(" Status "), Some(Command::Status));
        assert_eq!(parse_command("!away"), Some(Command::Away));
        assert_eq!(parse_command("away from keyboard now"), None);
        let long = "x".repeat(10_000);
        match interpret(
            &InteractionRequest::Continue { last_message: None },
            &BTreeMap::new(),
            &long,
        ) {
            Some(InteractionResponse::Continue { text }) => assert_eq!(text.len(), MAX_REPLY_CHARS),
            other => panic!("{other:?}"),
        }
        assert!(interpret(
            &InteractionRequest::Continue { last_message: None },
            &BTreeMap::new(),
            "  "
        )
        .is_none());
    }
}
