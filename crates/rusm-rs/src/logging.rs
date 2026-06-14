//! Platform logging for Rust guests — the Rust twin of the TS `console.*` hijack.
//!
//! A guest just writes `log::info!` / `warn!` / `error!` / `debug!` (the standard `log`
//! crate facade); the records route to the host `log` op, which stamps the time, this
//! process's component name (its label) + pid, and the severity colour, and writes the
//! line to the node's log stream — gated by the node `[log] level`. There is no init to
//! call and no name/pid/level to wire: the entry-point macros ([`crate::main`] /
//! [`crate::service`] / [`crate::handlers`]) install this logger once at startup.

use crate::actor;

/// Routes `log` crate records to the host `log` op (one line per record, the host owns
/// the format + gating).
struct PlatformLogger;

impl log::Log for PlatformLogger {
    /// Always enabled at the facade — the host gates by the node `[log] level`, the single
    /// source of truth, so the guest never second-guesses it.
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        let level = match record.level() {
            log::Level::Error => actor::LogLevel::Error,
            log::Level::Warn => actor::LogLevel::Warn,
            log::Level::Info => actor::LogLevel::Info,
            // `log` has Trace below Debug; RUSM's coarser scale folds it into debug.
            log::Level::Debug | log::Level::Trace => actor::LogLevel::Debug,
        };
        actor::log(level, &record.args().to_string());
    }

    fn flush(&self) {}
}

static LOGGER: PlatformLogger = PlatformLogger;

/// Install the platform logger as the global `log` sink. Idempotent — safe to call from
/// every entry point (the macros do); a second call is a no-op. The max level is left at
/// `Trace` so every record reaches the host, which applies the node `[log] level` gate.
#[doc(hidden)]
pub fn init() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Trace);
    }
}
