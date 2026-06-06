//! Show that the synthetic data source is a pure function of `(scenario, tick)`:
//! lively, scenario-shaped, and perfectly reproducible — which is what keeps the
//! dashboard and tests deterministic for scenarios that haven't graduated to a
//! real engine yet (spawn-storm, ping-pong and fault-recovery now run live).
//!
//! Run: `cargo run -p rusm-bench --example synthetic_source`

use rusm_bench::{Scenario, SyntheticSource};

fn main() {
    let source = SyntheticSource::new(Scenario::ConnectionStorm);

    for tick in 0..5 {
        let t = source.tick(tick, 4, 3, 4);
        println!(
            "tick {tick}: {:>9.0} ops/s, {:>6} procs, {} latency samples",
            t.ops_per_sec,
            t.process_count,
            t.latencies_ns.len(),
        );
    }

    assert_eq!(source.tick(0, 4, 3, 4), source.tick(0, 4, 3, 4));
    println!("\nre-running tick 0 produced byte-identical data ✓");
}
