//! Consistent terminal output. Colors are stripped automatically when stdout is not a
//! terminal, when `NO_COLOR` is set, or with `--no-color`.

use std::io::IsTerminal;

use anstyle::{AnsiColor, Style};

const GREEN: Style = AnsiColor::Green.on_default();
const RED: Style = AnsiColor::Red.on_default();
const YELLOW: Style = AnsiColor::Yellow.on_default();
const CYAN: Style = AnsiColor::Cyan.on_default();
const BOLD: Style = Style::new().bold();
const DIM: Style = Style::new().dimmed();

/// Disables colors globally.
pub fn disable_color() {
    anstream::ColorChoice::Never.write_global();
}

/// Whether we can prompt the user.
pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

fn paint(style: Style, text: &str) -> String {
    format!("{style}{text}{style:#}")
}

/// Bold heading.
pub fn heading(text: &str) {
    anstream::println!("{}", paint(BOLD, text));
}

/// A blank line.
pub fn blank() {
    anstream::println!();
}

/// `✓ text`
pub fn ok(text: &str) {
    anstream::println!("{} {text}", paint(GREEN, "✓"));
}

/// `! text`
pub fn warn(text: &str) {
    anstream::println!("{} {text}", paint(YELLOW, "!"));
}

/// `✗ text`
pub fn fail(text: &str) {
    anstream::println!("{} {text}", paint(RED, "✗"));
}

/// `• text`
pub fn info(text: &str) {
    anstream::println!("{} {text}", paint(CYAN, "•"));
}

/// Indented, dimmed hint (`  → text`).
pub fn hint(text: &str) {
    anstream::println!("  {} {}", paint(DIM, "→"), paint(DIM, text));
}

/// `  Key: value`
pub fn kv(key: &str, value: &str) {
    anstream::println!("  {} {value}", paint(DIM, &format!("{key}:")));
}

/// Plain line.
pub fn line(text: &str) {
    anstream::println!("{text}");
}

/// Dimmed text.
pub fn dim(text: &str) -> String {
    paint(DIM, text)
}

/// Bold text.
pub fn bold(text: &str) -> String {
    paint(BOLD, text)
}

/// Colored by outcome.
pub fn status_word(text: &str, good: Option<bool>) -> String {
    match good {
        Some(true) => paint(GREEN, text),
        Some(false) => paint(RED, text),
        None => paint(YELLOW, text),
    }
}

/// Prints an error with an optional suggestion to stderr.
pub fn error(message: &str) {
    anstream::eprintln!("{} {message}", paint(RED, "error:"));
}

/// Prints a value as pretty JSON.
pub fn json<T: serde::Serialize>(value: &T) -> anyhow::Result<()> {
    anstream::println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
