//! Secret redaction for logs, errors, diagnostics and outgoing notifications.

use std::borrow::Cow;
use std::sync::OnceLock;

use regex::Regex;

/// Replacement marker for redacted content.
pub const REDACTED: &str = "[REDACTED]";

struct Pattern {
    regex: Regex,
    replacement: &'static str,
}

fn patterns() -> &'static [Pattern] {
    static PATTERNS: OnceLock<Vec<Pattern>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let specs: [(&str, &'static str); 10] = [
            // Discord webhook URLs: keep the host so errors stay understandable.
            (
                r"(?i)(https?://(?:[a-z]+\.)?discord(?:app)?\.com/api(?:/v\d+)?/webhooks/)[^\s?#]+",
                "${1}[REDACTED]",
            ),
            // Telegram bot tokens, inside URLs (`/bot<token>/`) or bare.
            (r"\d{6,12}:[A-Za-z0-9_-]{30,}", "[REDACTED]"),
            // Bearer / authorization headers.
            (r"(?i)(bearer\s+)[A-Za-z0-9._~+/=-]{8,}", "${1}[REDACTED]"),
            // Anthropic / OpenAI style API keys.
            (r"\bsk-[A-Za-z0-9_-]{16,}", "[REDACTED]"),
            // GitHub tokens.
            (r"\b(?:ghp|gho|ghu|ghs|ghr|github_pat)_[A-Za-z0-9_]{20,}", "[REDACTED]"),
            // Slack tokens.
            (r"\bxox[abposr]-[A-Za-z0-9-]{10,}", "[REDACTED]"),
            // AWS access key ids.
            (r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", "[REDACTED]"),
            // Private key blocks.
            (
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
                "[REDACTED PRIVATE KEY]",
            ),
            // Generic `password=...`, `token: ...`, `api_key="..."` assignments.
            (
                r#"(?i)\b((?:password|passwd|secret|token|api[_-]?key|access[_-]?key)\s*[:=]\s*["']?)[^\s"',;]{6,}"#,
                "${1}[REDACTED]",
            ),
            // Credentials embedded in URLs (scheme://user:pass@host).
            (r"(?i)\b([a-z][a-z0-9+.-]*://[^\s:/@]+:)[^\s@/]+@", "${1}[REDACTED]@"),
        ];
        specs
            .into_iter()
            .filter_map(|(re, replacement)| {
                Regex::new(re).ok().map(|regex| Pattern { regex, replacement })
            })
            .collect()
    })
}

/// Redacts well-known secret formats from `input`.
pub fn redact(input: &str) -> Cow<'_, str> {
    let mut out: Cow<'_, str> = Cow::Borrowed(input);
    for p in patterns() {
        if p.regex.is_match(&out) {
            out = Cow::Owned(p.regex.replace_all(&out, p.replacement).into_owned());
        }
    }
    out
}

/// Redacts well-known secret formats and the exact `known` secret values.
pub fn redact_with(input: &str, known: &[&str]) -> String {
    let mut out = input.to_owned();
    for secret in known.iter().filter(|s| s.len() >= 4) {
        if out.contains(secret) {
            out = out.replace(secret, REDACTED);
        }
    }
    redact(&out).into_owned()
}

/// Masks a value for display, keeping only a short hint (e.g. `…a1b2`).
pub fn mask(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 8 {
        return "••••".to_owned();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("••••{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_discord_webhook_but_keeps_host() {
        let s = "POST https://discord.com/api/webhooks/123456789/abcDEF-ghi_jkl failed";
        let r = redact(s);
        assert!(!r.contains("abcDEF"), "{r}");
        assert!(!r.contains("123456789"), "{r}");
        assert!(
            r.contains("https://discord.com/api/webhooks/[REDACTED]"),
            "{r}"
        );
    }

    #[test]
    fn redacts_telegram_token_in_url() {
        let s = "error sending request for url (https://api.telegram.org/bot123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw0/sendMessage)";
        let r = redact(s);
        assert!(!r.contains("AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw0"), "{r}");
    }

    #[test]
    fn redacts_common_api_keys_and_assignments() {
        let s = "key sk-ant-api03-abcdefghijklmnopqrstuv and ghp_abcdefghijklmnopqrstuvwxyz123 password=hunter2hunter";
        let r = redact(s);
        assert!(!r.contains("abcdefghijklmnopqrstuv"));
        assert!(!r.contains("ghp_abc"));
        assert!(!r.contains("hunter2hunter"));
        assert!(r.contains("password=[REDACTED]"));
    }

    #[test]
    fn redacts_url_credentials_and_private_keys() {
        let r = redact("postgres://admin:s3cretpw@db:5432/x");
        assert_eq!(r, "postgres://admin:[REDACTED]@db:5432/x");
        let key = "-----BEGIN RSA PRIVATE KEY-----\nMIIabc\n-----END RSA PRIVATE KEY-----";
        assert_eq!(redact(key), "[REDACTED PRIVATE KEY]");
    }

    #[test]
    fn leaves_ordinary_text_untouched() {
        let s = "Claude needs your permission to use Bash";
        assert!(matches!(redact(s), Cow::Borrowed(_)));
    }

    #[test]
    fn redact_with_removes_known_values() {
        let r = redact_with("token is abc-123-xyz here", &["abc-123-xyz"]);
        assert_eq!(r, "token is [REDACTED] here");
    }

    #[test]
    fn mask_hides_most_of_value() {
        assert_eq!(mask("short"), "••••");
        assert_eq!(mask("abcdefghijkl"), "••••ijkl");
    }
}
