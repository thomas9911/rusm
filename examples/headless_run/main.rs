//! Drive the benchmark runner directly — no node, no network — and print a few
//! sampled ticks. The smallest possible taste of the harness API.
//!
//! Run: `cargo run -p rusm-bench --example headless_run`

use rusm_bench::{summarize_frame, Runner, RunnerConfig, Scenario};

fn main() {
    let mut runner = Runner::new(RunnerConfig::default());
    runner.start(Scenario::ConnectionStorm);

    println!("running `connection-storm` for 10 ticks:\n");
    for tick in 0..10 {
        let frame = runner.tick(tick * 50);
        println!("{}", summarize_frame(&frame));
    }
}
