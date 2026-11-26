//! Timestamp formatting helpers shared by storage and the CLI.

use chrono::{DateTime, SecondsFormat, Utc};

/// Formats a timestamp as fixed-width RFC 3339 UTC with milliseconds.
///
/// The fixed width makes stored timestamps sort correctly as text.
pub fn format_ts(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Parses a timestamp produced by [`format_ts`] (or any RFC 3339 string).
pub fn parse_ts(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Formats a duration in a compact human form: `12s`, `4m`, `2h 14m`, `3d 2h`.
pub fn humanize_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds().max(0);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => {
            let m = (secs % 3600) / 60;
            if m == 0 {
                format!("{}h", secs / 3600)
            } else {
                format!("{}h {}m", secs / 3600, m)
            }
        }
        _ => {
            let h = (secs % 86_400) / 3600;
            if h == 0 {
                format!("{}d", secs / 86_400)
            } else {
                format!("{}d {}h", secs / 86_400, h)
            }
        }
    }
}

/// `"12s ago"`-style relative time.
pub fn ago(ts: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let d = now - ts;
    if d.num_seconds() < 2 {
        "just now".to_owned()
    } else {
        format!("{} ago", humanize_duration(d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_roundtrips_and_sorts() {
        let a = Utc::now();
        let b = a + chrono::Duration::milliseconds(5);
        let (fa, fb) = (format_ts(a), format_ts(b));
        assert!(fa < fb);
        assert_eq!(fa.len(), "2026-01-01T00:00:00.000Z".len());
        assert_eq!(format_ts(parse_ts(&fa).unwrap()), fa);
    }

    #[test]
    fn humanize() {
        assert_eq!(humanize_duration(chrono::Duration::seconds(12)), "12s");
        assert_eq!(humanize_duration(chrono::Duration::seconds(130)), "2m");
        assert_eq!(
            humanize_duration(chrono::Duration::seconds(2 * 3600 + 14 * 60)),
            "2h 14m"
        );
        assert_eq!(
            humanize_duration(chrono::Duration::seconds(3 * 86_400)),
            "3d"
        );
    }
}
