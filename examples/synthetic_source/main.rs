//! Show that the synthetic data source is a pure function of `(scenario, tick)`:
//! lively, scenario-shaped, and perfectly reproducible — which is what keeps the
//! Phase 0 dashboard and tests deterministic.
//!
//! Run: `cargo run -p rusm-bench --example synthetic_source`

use rusm_bench::{Scenario, SyntheticSource};

fn main() {
    let source = SyntheticSource::new(Scenario::SpawnStorm);

    for tick in 0..5 {
        let t = source.tick(tick, 4, 3, 4);
        println!(
            "tick {tick}: {:>9.0} ops/s, peak {:>6}, {} latency samples",
            t.ops_per_sec,
            t.peak_concurrent,
            t.latencies_ns.len(),
        );
    }

    assert_eq!(source.tick(0, 4, 3, 4), source.tick(0, 4, 3, 4));
    println!("\nre-running tick 0 produced byte-identical data ✓");
}
