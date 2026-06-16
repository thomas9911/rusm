//! HTTP throughput via [`balter`]: find the **max sustained req/s** at an SLA.
//!
//! Method — a **fixed-rate sweep**. We drive balter's reliable constant-rate
//! controller at increasing target TPS and, at each level, measure what the server
//! *actually* achieved plus its tail latency and error rate. We climb until the SLA
//! breaks (latency/errors exceed budget) or throughput plateaus (a higher target no
//! longer yields more), and report the last level the server genuinely sustained.
//! Every reported number is a direct measurement at a real offered load — there is
//! no controller extrapolation. (balter's *auto*-saturation loop is deliberately
//! cautious and stalls in the sub-millisecond loopback regime, so we don't rely on
//! it; the constant-rate controller is exact.)
//!
//! balter calls the `#[scenario]` repeatedly across concurrent tasks, so there is
//! no per-task state to hold a connection in; instead a single process-wide
//! [`reqwest::Client`] pools keep-alive connections that every invocation reuses
//! (the battle-proven way to keep load realistic without re-handshaking each call).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

use balter::prelude::*;
use balter::Hint;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::Opts;

/// balter's per-run statistics (achieved tps, latency quantiles, error rate),
/// re-exported so callers needn't depend on balter directly.
pub use balter::prelude::RunStatistics;

/// Cumulative successful requests — a live counter the dashboard reads each tick to
/// chart the *achieved* rate smoothly, independent of balter's window boundaries.
static ACHIEVED: AtomicU64 = AtomicU64::new(0);
/// Live in-flight requests — bumped while a request is outstanding (sent, awaiting a
/// response) and dropped when it returns. A live **gauge** (current concurrency), not a
/// cumulative count. The HTTP-throughput scenario serves on the `wasi:http` per-request
/// path, whose instances aren't `rusm-otp` processes (so `rt.process_count()` is 0); this
/// is the real concurrency the load is actually driving, which the dashboard charts.
static INFLIGHT: AtomicU64 = AtomicU64::new(0);

/// RAII guard: decrements [`INFLIGHT`] on every exit path of a request (incl. the `?`
/// error returns), so the gauge can never leak.
struct InFlightGuard;
impl Drop for InFlightGuard {
    fn drop(&mut self) {
        INFLIGHT.fetch_sub(1, Ordering::Relaxed);
    }
}
/// Optional live latency sink (installed by the dashboard engine; `None` for the CLI
/// sweep, which reads latency from balter's `RunStatistics` instead).
static LAT_TX: OnceLock<RwLock<Option<UnboundedSender<u64>>>> = OnceLock::new();
/// Sample one request's latency every Nth into the live sink.
const LIVE_LAT_EVERY: u64 = 64;

/// Total successful requests since the last [`reset_counter`].
pub fn achieved() -> u64 {
    ACHIEVED.load(Ordering::Relaxed)
}

/// Requests currently in flight — the count of handler instances running concurrently on
/// the server right now (each in-flight request is one `wasi:http` instance handling it).
/// This is the HTTP-throughput scenario's live concurrency / "processes" reading.
pub fn inflight() -> u64 {
    INFLIGHT.load(Ordering::Relaxed)
}

/// Zeroes the live counter (call when (re)starting a live driver).
pub fn reset_counter() {
    ACHIEVED.store(0, Ordering::Relaxed);
}

/// Installs a live latency sink and returns its receiver; the transaction samples
/// into it. Replaces any previous sink.
pub fn install_latency_sink() -> UnboundedReceiver<u64> {
    let (tx, rx) = unbounded_channel();
    *LAT_TX
        .get_or_init(|| RwLock::new(None))
        .write()
        .expect("latency sink lock") = Some(tx);
    rx
}

/// Removes the live latency sink (the transaction stops sampling).
pub fn clear_latency_sink() {
    if let Some(cell) = LAT_TX.get() {
        *cell.write().expect("latency sink lock") = None;
    }
}

/// One pooled HTTP client for the whole run (keep-alive reuse across invocations).
static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
/// The target URL. Re-settable so each run/window — and each dashboard restart on a
/// fresh ephemeral port — can retarget without a new process. Only one HTTP load
/// runs at a time (the CLI is one invocation; the dashboard, one scenario).
static TARGET: OnceLock<RwLock<String>> = OnceLock::new();

fn client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .pool_max_idle_per_host(usize::MAX) // never drop idle keep-alive sockets
            .timeout(Duration::from_secs(10)) // a hung request is an error, not infinite latency
            .build()
            .expect("build reqwest client")
    })
}

/// Points the HTTP scenario at `url`; call before driving load.
pub fn set_target(url: impl Into<String>) {
    let cell = TARGET.get_or_init(|| RwLock::new(String::new()));
    *cell.write().expect("target lock") = url.into();
}

fn target() -> String {
    TARGET
        .get()
        .expect("target set before scenario")
        .read()
        .expect("target lock")
        .clone()
}

/// Runs one fixed-rate window and returns balter's measured stats. The dashboard
/// polls this repeatedly: each window **completes**, so balter cleanly shuts down
/// its worker tasks (it only aborts them on completion, never on drop) — nothing
/// leaks between windows. Call [`set_target`] first.
pub async fn run_window(target_tps: u32, concurrency: usize, window: Duration) -> RunStatistics {
    http_load()
        .tps(target_tps)
        .hint(Hint::Concurrency(concurrency))
        .duration(window)
        .await
}

/// One measured level of the sweep: the rate we asked for and what happened.
struct Level {
    goal: u32,
    stats: RunStatistics,
}

impl Level {
    /// Did the server keep up at this level under the SLA? It must achieve ~the
    /// offered rate (≥98%), stay within the error budget, and hold tail latency.
    fn sustained(&self, opts: &Opts) -> bool {
        self.stats.actual_tps >= self.goal as f64 * 0.98 && self.within_sla(opts)
    }

    /// Within the error + tail-latency budget (the `--quantile` percentile).
    fn within_sla(&self, opts: &Opts) -> bool {
        self.stats.error_rate <= opts.error_rate && tail(&self.stats, opts.quantile) <= opts.latency
    }

    /// Throughput stopped rising — a higher target yields ≤5% more. The server is
    /// saturated; this is its ceiling regardless of the SLA headroom.
    fn plateaued_vs(&self, prev_achieved: f64) -> bool {
        self.stats.actual_tps < prev_achieved * 1.05
    }
}

/// Drives the HTTP sweep on its own multi-threaded Tokio runtime. With `--tps` it
/// instead measures one explicit fixed rate (no sweep).
pub fn run(opts: Opts, url: String) {
    set_target(&url);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    rt.block_on(async move {
        if let Some(tps) = opts.tps {
            println!("HTTP fixed-rate: {tps} req/s for {:?} → {url}", opts.duration);
            let level = measure(tps, opts.duration).await;
            report_level(&level);
            verdict(&url, &level, &opts);
            return;
        }

        // Sweep: each step is `step_secs` long; double the offered rate until the
        // server can no longer sustain it under the SLA, then report the ceiling.
        let step = Duration::from_secs((opts.duration.as_secs() / 5).max(5));
        println!(
            "HTTP sweep: doubling from {} req/s ({}s/step) until SLA breaks (p{:.0} ≤ {:?}, err ≤ {:.0}%) → {url}",
            opts.start_tps,
            step.as_secs(),
            opts.quantile * 100.0,
            opts.latency,
            opts.error_rate * 100.0,
        );

        let mut goal = opts.start_tps;
        let mut best: Option<Level> = None;
        let mut prev_achieved = 0.0;
        loop {
            let level = measure(goal, step).await;
            report_level(&level);
            let sustained = level.sustained(&opts);
            let plateaued = level.plateaued_vs(prev_achieved);
            prev_achieved = level.stats.actual_tps;

            // Keep the better-throughput level as the earned result. A level that
            // missed its goal but still pushed more bytes (higher achieved tps) at an
            // acceptable SLA is a legitimately higher sustained number.
            let better = best
                .as_ref()
                .is_none_or(|b| level.stats.actual_tps > b.stats.actual_tps);
            let acceptable = level.within_sla(&opts);
            if acceptable && better {
                best = Some(level);
            }

            // Stop once the server stops keeping up *and* throughput has plateaued, or
            // the SLA is breached outright — pushing harder only adds latency/errors.
            if (!sustained && plateaued) || !acceptable {
                break;
            }
            goal = goal.saturating_mul(2);
        }

        match best {
            Some(level) => verdict(&url, &level, &opts),
            None => println!("\nno level met the SLA — lower the offered rate or relax --latency-ms"),
        }
    });
}

/// Runs one fixed-rate level and returns its measurement. We seed the starting
/// concurrency to the offered rate (assuming ~1ms service, so `tps/1000` in-flight)
/// so balter doesn't spend the step ramping up from its default of 10 — it still
/// auto-adjusts from there, but starts in the right neighbourhood, which keeps each
/// level a clean apples-to-apples measurement rather than a ramp artifact.
async fn measure(tps: u32, duration: Duration) -> Level {
    let concurrency = ((tps / 1000) as usize).clamp(16, 1024);
    let stats = http_load()
        .tps(tps)
        .hint(Hint::Concurrency(concurrency))
        .duration(duration)
        .await;
    Level { goal: tps, stats }
}

#[scenario]
async fn http_load() {
    let _ = get_root().await;
}

#[transaction]
async fn get_root() -> Result<(), String> {
    one_request().await
}

/// One GET against the target: send, drain the body (returns the keep-alive socket to the
/// pool), record latency + the achieved/in-flight counters. Shared by balter's transaction
/// (the CLI sweep) and the closed-loop driver (the dashboard).
async fn one_request() -> Result<(), String> {
    INFLIGHT.fetch_add(1, Ordering::Relaxed);
    let _inflight = InFlightGuard; // decremented on return (incl. the `?` paths below)
    let started = Instant::now();
    let resp = client()
        .get(target())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let ok = resp.status().is_success();
    let _ = resp.bytes().await.map_err(|e| e.to_string())?;
    if !ok {
        return Err("non-success status".to_string());
    }
    // Live observability for the dashboard; the CLI sweep installs no sink, so this
    // is a single relaxed increment there (latency comes from balter's RunStatistics).
    let n = ACHIEVED.fetch_add(1, Ordering::Relaxed);
    if n % LIVE_LAT_EVERY == 0 {
        if let Some(cell) = LAT_TX.get() {
            if let Some(tx) = cell.read().expect("latency sink lock").as_ref() {
                let _ = tx.send(started.elapsed().as_nanos() as u64);
            }
        }
    }
    Ok(())
}

/// Drive `concurrency` **closed-loop** workers (each: request → await → repeat) until
/// `stop`. Closed-loop self-limits to the server's real capacity: with a fixed number of
/// outstanding requests it can never flood or spiral the way an open-loop rate chase does
/// against a slower-than-target server, so throughput holds steady at the true ceiling
/// regardless of how fast the guest is — and there's no balter process-global state to
/// wedge when the dashboard switches scenarios. This is the dashboard's HTTP driver; the
/// CLI sweep still uses balter's controller (an explicit max-rate measurement).
pub async fn run_closed_loop(
    concurrency: usize,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let workers: Vec<_> = (0..concurrency.max(1))
        .map(|_| {
            let stop = std::sync::Arc::clone(&stop);
            tokio::spawn(async move {
                while !stop.load(Ordering::Relaxed) {
                    let _ = one_request().await;
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.await;
    }
}

/// The tail-latency reading for a quantile (RunStatistics exposes p50/90/95/99;
/// pick the nearest at-or-above the request, defaulting to p99).
fn tail(s: &RunStatistics, quantile: f64) -> Duration {
    match quantile {
        q if q <= 0.50 => s.latency_p50,
        q if q <= 0.90 => s.latency_p90,
        q if q <= 0.95 => s.latency_p95,
        _ => s.latency_p99,
    }
}

/// One line per sweep step: offered → achieved, with tail latency and errors.
fn report_level(level: &Level) {
    let s = &level.stats;
    println!(
        "  offered {:>7} → achieved {:>7.0} req/s · p99 {:>9?} · err {:.2}% · {} conns",
        level.goal,
        s.actual_tps,
        s.latency_p99,
        s.error_rate * 100.0,
        s.concurrency,
    );
}

/// The earned headline: the highest rate the server genuinely sustained at the SLA.
fn verdict(url: &str, level: &Level, opts: &Opts) {
    let s = &level.stats;
    println!("\n── earned result ── {url}");
    println!("  sustained        {:.0} req/s", s.actual_tps);
    println!("  at offered       {} req/s", level.goal);
    println!("  concurrency      {} in-flight", s.concurrency);
    println!("  error rate       {:.3}%", s.error_rate * 100.0);
    println!(
        "  latency          p50 {:?} · p90 {:?} · p95 {:?} · p99 {:?}",
        s.latency_p50, s.latency_p90, s.latency_p95, s.latency_p99
    );
    println!(
        "  SLA              p{:.0} ≤ {:?}, err ≤ {:.0}%",
        opts.quantile * 100.0,
        opts.latency,
        opts.error_rate * 100.0
    );
}
