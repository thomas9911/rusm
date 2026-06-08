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

use std::sync::OnceLock;
use std::time::Duration;

use balter::prelude::*;
use balter::Hint;

use crate::Opts;

/// One pooled HTTP client for the whole run (keep-alive reuse across invocations).
static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
/// The target URL, set once before the scenario starts (scenarios take no args).
static TARGET: OnceLock<String> = OnceLock::new();

fn client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .pool_max_idle_per_host(usize::MAX) // never drop idle keep-alive sockets
            .timeout(Duration::from_secs(10)) // a hung request is an error, not infinite latency
            .build()
            .expect("build reqwest client")
    })
}

fn target() -> &'static str {
    TARGET.get().expect("target set before scenario").as_str()
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
    TARGET.set(url.clone()).expect("target set once");
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
    let resp = client()
        .get(target())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let ok = resp.status().is_success();
    // Drain the body so the connection returns to the keep-alive pool for reuse.
    let _ = resp.bytes().await.map_err(|e| e.to_string())?;
    if ok {
        Ok(())
    } else {
        Err("non-success status".to_string())
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
