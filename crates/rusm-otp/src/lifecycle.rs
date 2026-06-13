//! Opt-in process **lifecycle logging** — the "see what's happening" switch a node
//! turns on explicitly (`rusm.toml [log] spawns = true`). Off by default: the spawn
//! hot path does nothing. When on, the runtime logs each **labeled** process's spawn
//! and exit to stderr, coloured — so the signal is *components* (which the host
//! labels), not internal plumbing (responders, writers — left unlabeled).
//!
//! This module owns only the *format*; the runtime owns the *gate* (the flag) and the
//! *when* (a spawn site, and `deregister` on exit). Lines are `component#pid` so a log
//! reader can tell instances apart, with the spawn line carrying the process's
//! effective capabilities — the thing that's otherwise invisible.
//!
//! Every line is tagged **`rusm`** so these *platform* events (the runtime spawning and
//! ending processes) are visually distinct from an app's own domain logs.

use std::collections::BTreeMap;
use std::io::IsTerminal;

use crate::exit::ExitReason;
use crate::pid::Pid;

/// Platform log verbosity, declared via `rusm.toml [log] level`. Ordered, cumulative:
/// a configured level shows every event at or below it. Each lifecycle event maps to a
/// distinct level — `Error`: a **crash** (a trap / OOM); `Warn`: + a **kill** (or
/// cascade); `Info`: + a **clean exit**; `Debug`: + every **spawn**. So a restart reads
/// as a crash `exit` (Error) then a fresh `spawn` (Debug).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Debug)]
pub enum LogLevel {
    /// No platform logging (the default — zero hot-path cost).
    #[default]
    Off,
    /// Crashes only (a guest trap / OOM).
    Error,
    /// + kills and link cascades.
    Warn,
    /// + clean (normal) exits — every process *ending*.
    Info,
    /// + every spawn — full lifecycle visibility.
    Debug,
}

impl LogLevel {
    /// Parse a manifest string (`off`/`error`/`warn`/`info`/`debug`); anything else is
    /// `Off`, so a typo silently quiets rather than crashes.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "error" => Self::Error,
            "warn" | "warning" => Self::Warn,
            "info" => Self::Info,
            "debug" | "trace" => Self::Debug,
            _ => Self::Off,
        }
    }

    /// The level of a process **exit** — the single source of truth for both the gate
    /// (which level shows it) and the colour: a crash is `Error`, a kill/cascade `Warn`,
    /// a clean exit `Info`.
    pub fn for_exit(reason: ExitReason) -> Self {
        match reason {
            ExitReason::Crashed => Self::Error,
            ExitReason::Killed | ExitReason::NoProc => Self::Warn,
            ExitReason::Normal => Self::Info,
        }
    }

    /// The ANSI colour code for this level (red / yellow / green; cyan for the rest).
    fn colour(self) -> &'static str {
        match self {
            Self::Error => "31",
            Self::Warn => "33",
            Self::Info => "32",
            _ => "36",
        }
    }
}

/// Wrap `text` in an ANSI colour `code`, but only when stderr is a terminal — piped or
/// redirected logs stay plain (no escape soup in a file).
fn paint(code: &str, text: &str) -> String {
    if std::io::stderr().is_terminal() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

/// The dim `rusm` tag that marks a line as a **platform** event (vs an app's logs).
fn tag() -> String {
    paint("2;35", "rusm") // dim magenta
}

/// `<id>` rendered as a bold name + dim `#pid` — the identifier shared by spawn/exit.
fn ident(label: &str, pid: Pid) -> String {
    format!(
        "{}{}",
        paint("1", label),
        paint("2", &format!("#{}", pid.0))
    )
}

/// Log a component **spawn**: `rusm spawn <label>#<pid>  <detail>` (detail = its
/// effective capabilities, so a reader sees exactly what the process can do).
pub fn log_spawn(pid: Pid, label: &str, detail: &str) {
    eprintln!(
        "{} {} {}  {}",
        tag(),
        paint("36", "spawn"), // cyan
        ident(label, pid),
        paint("2", detail), // dim
    );
}

/// Log a process **exit**: `rusm exit  <label>#<pid>  <reason>` — coloured by the
/// exit's level (red crash / yellow kill / green clean), the same mapping that gated it.
pub fn log_exit(pid: Pid, label: &str, reason: ExitReason) {
    let code = LogLevel::for_exit(reason).colour();
    eprintln!(
        "{} {}  {}  {}",
        tag(),
        paint(code, "exit"),
        ident(label, pid),
        paint(code, &format!("{reason:?}").to_lowercase()),
    );
}

/// Log a process **census**: `rusm <hh:mm:ss> census  <comp>=<n>  …` — the count of
/// live processes per component (by label), timestamped (UTC) and emitted debounced
/// after process state settles. Bold names, cyan counts; an idle node reads `(none)`.
pub fn log_census(counts: &BTreeMap<String, u64>) {
    let body = if counts.is_empty() {
        paint("2", "(none)")
    } else {
        counts
            .iter()
            .map(|(name, n)| {
                format!(
                    "{}{}{}",
                    paint("1", name),
                    paint("2", "="),
                    paint("36", &n.to_string())
                )
            })
            .collect::<Vec<_>>()
            .join("  ")
    };
    eprintln!(
        "{} {} {}  {}",
        tag(),
        paint("2", &now_hms()),
        paint("36", "census"),
        body
    );
}

/// `HH:MM:SS` (UTC) for a UNIX-epoch seconds value — pure (no clock read) so the
/// formatting is unit-testable; [`now_hms`] supplies "now".
fn hms(unix_secs: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        (unix_secs / 3600) % 24,
        (unix_secs / 60) % 60,
        unix_secs % 60
    )
}

/// `HH:MM:SS` (UTC) for the current wall clock — dependency-free (no chrono in the
/// Wasm-free core); falls back to `00:00:00` if the clock is before the epoch.
fn now_hms() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    hms(secs)
}

// A supervisor **restart** intentionally has no dedicated event: it reads as the
// crashed instance's abnormal `exit` line followed by a fresh `spawn` line for the
// same component — carrying the crash reason and the new pid, which a bare "restart"
// line could not. (`LogLevel::Info` sits between `Warn` and `Debug` for that reason.)

#[cfg(test)]
mod tests {
    use super::LogLevel;

    #[test]
    fn parse_maps_known_levels_and_quiets_the_rest() {
        assert_eq!(LogLevel::parse("debug"), LogLevel::Debug);
        assert_eq!(LogLevel::parse("INFO"), LogLevel::Info);
        assert_eq!(LogLevel::parse("warning"), LogLevel::Warn);
        assert_eq!(LogLevel::parse("error"), LogLevel::Error);
        // Unset or unrecognised quiets to Off — a typo never accidentally goes loud.
        assert_eq!(LogLevel::parse(""), LogLevel::Off);
        assert_eq!(LogLevel::parse("loud"), LogLevel::Off);
    }

    #[test]
    fn hms_formats_utc_clock_with_wraparound() {
        assert_eq!(super::hms(0), "00:00:00");
        assert_eq!(super::hms(3661), "01:01:01");
        assert_eq!(super::hms(86_399), "23:59:59");
        assert_eq!(super::hms(90_061), "01:01:01", "wraps past 24h");
    }

    #[test]
    fn levels_are_ordered_off_to_debug() {
        assert!(LogLevel::Off < LogLevel::Error);
        assert!(LogLevel::Error < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Debug);
    }
}
