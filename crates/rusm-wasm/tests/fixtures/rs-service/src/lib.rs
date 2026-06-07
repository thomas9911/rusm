//! Exercises `#[rusm_rs::service]`: the `calc` module's functions become a
//! `serve()` dispatch loop **and** a typed `Client`. To test both ends in one
//! component, `run` plays two roles by its first message: `"serve"` → run the
//! service; otherwise (a collector pid) → spawn a sibling `calc` and call it
//! through the generated client, forwarding the result.

#[rusm_rs::service]
pub mod calc {
    pub fn add(a: i64, b: i64) -> i64 {
        a + b
    }
    pub fn greet(name: String) -> String {
        format!("hi {name}")
    }
}

wit_bindgen::generate!({
    world: "process",
    path: "wit",
    with: { "rusm:runtime/actor@0.1.0": rusm_rs::rusm::runtime::actor },
});

struct Component;

impl Guest for Component {
    fn run() {
        let first = rusm_rs::receive_bytes();
        if first == b"serve" {
            calc::serve(); // never returns
        }
        // Commander role: `first` is the collector pid (decimal).
        let collector = rusm_rs::Pid(String::from_utf8(first).unwrap().parse().unwrap());
        let client = calc::Client::spawn("calc").unwrap();
        rusm_rs::send_bytes(client.pid, b"serve"); // put the sibling into serve mode
        let sum = client.add(2, 3).unwrap();
        let hi = client.greet("RUSM".to_string()).unwrap();
        rusm_rs::send_bytes(collector, format!("sum={sum} {hi}").as_bytes());
    }
}

export!(Component);
