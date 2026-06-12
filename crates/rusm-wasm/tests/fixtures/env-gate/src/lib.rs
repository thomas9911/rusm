//! Exits **Normal** iff the env var `RUSM_CAP_PROBE` == `"granted"`; otherwise it
//! panics (a trap → Crashed). Used to prove a guest-spawned **registered** component
//! runs under its OWN declared capability profile — which grants the env var — rather
//! than inheriting the spawner's caps (which do not).
#[rusm_rs::main]
fn run() {
    assert_eq!(
        std::env::var("RUSM_CAP_PROBE").ok().as_deref(),
        Some("granted"),
        "the component's declared env grant must reach it on spawn-by-name",
    );
}
