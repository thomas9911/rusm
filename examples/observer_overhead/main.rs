//! Demonstrate the observer's detail on/off switch — the basis of the
//! "observer-on vs observer-off" overhead proof. With detail off, aggregate
//! counters still update, but the costly per-instance table is skipped entirely.
//!
//! Run: `cargo run -p rusm-bench --example observer_overhead`

use rusm_bench::{Runner, RunnerConfig, Scenario};

fn main() {
    let mut runner = Runner::new(RunnerConfig::default());
    runner.start(Scenario::SpawnStorm);

    let on = runner.tick(0);
    println!(
        "detail ON : process_count={:<7} table_rows={}",
        on.observer.process_count,
        on.observer.processes.len(),
    );

    runner.set_observer_detail(false);
    let off = runner.tick(50);
    println!(
        "detail OFF: process_count={:<7} table_rows={}  <- table suppressed, aggregates intact",
        off.observer.process_count,
        off.observer.processes.len(),
    );
}
