// A stock command component: no RUSM actor API, just `wasi:cli/run` + `wasi:filesystem`.
fn main() {
    std::fs::write("/out/ran.txt", b"command component ran").expect("write /out/ran.txt");
}
