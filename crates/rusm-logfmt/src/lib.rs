//! Shared formatting for RUSM's coloured, columnar log lines — the **single source** for
//! the timestamp format, the ANSI palette, the column widths, and tty-gated colouring.
//!
//! Used by the platform log ([`rusm_otp`](../rusm_otp/index.html)'s `lifecycle`) and by
//! app loggers (e.g. genius's `domain::log`), so their lines share one look and line up
//! when interleaved — in particular the identifier (`who`) column is the same colour for
//! the `rusm` tag and a `component#pid`. TS guests mirror this palette by hand (a
//! language boundary — there is no Rust code a TS file can import).
//!
//! Pure and dependency-free: compiles for the host and for `wasm32-wasip2` guests alike.

use std::io::IsTerminal;

/// Gray — the timestamp (leads every line).
pub const TIME: &str = "90";
/// Dim magenta — the identifier (`who`) column: the `rusm` tag or a `component#pid`. One
/// colour for both, so platform and app lines' identifiers match.
pub const WHO: &str = "2;35";
/// Cyan — an app log level, or a neutral platform verb (`spawn` / `census`).
pub const LEVEL: &str = "36";
/// Red — a crash / error.
pub const ERROR: &str = "31";
/// Yellow — a kill / warning.
pub const WARN: &str = "33";
/// Green — a clean exit / info.
pub const OK: &str = "32";
/// Bold — a name/label (not a colour, an attribute).
pub const BOLD: &str = "1";
/// Dim — secondary detail (`#pid`, capability summaries, separators).
pub const DIM: &str = "2";

/// Width of the identifier (`who`) column. A `component#pid` (name truncated to 10) or the
/// `rusm` tag pads to this, so the action column that follows lines up across streams.
pub const WHO_WIDTH: usize = 14;
/// Width of the action/verb column (`spawn` / `census` / `info` / …).
pub const ACTION_WIDTH: usize = 6;

/// Wrap `text` in ANSI colour `code`, but only when stderr is a terminal — piped or
/// redirected logs stay plain (no escape soup in a file).
pub fn paint(code: &str, text: &str) -> String {
    if std::io::stderr().is_terminal() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

/// `HH:MM:SS` (UTC) for a UNIX-epoch seconds value — pure (no clock read) so the
/// formatting is unit-testable; [`now_hms`] supplies "now".
pub fn hms(unix_secs: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        (unix_secs / 3600) % 24,
        (unix_secs / 60) % 60,
        unix_secs % 60
    )
}

/// `HH:MM:SS` (UTC) for the current wall clock (`wasi:clocks` in a guest).
pub fn now_hms() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    hms(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hms_formats_utc_clock_with_wraparound() {
        assert_eq!(hms(0), "00:00:00");
        assert_eq!(hms(3661), "01:01:01");
        assert_eq!(hms(86_399), "23:59:59");
        assert_eq!(hms(90_061), "01:01:01"); // wraps past 24h
    }
}
