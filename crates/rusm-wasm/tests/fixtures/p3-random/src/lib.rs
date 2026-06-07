//! A minimal **WASI preview3** guest: on `run` it calls `wasi:random@0.3.0`'s
//! `get-random-u64` (a p3 import) a few times. If the wasip3 bridge didn't resolve
//! and execute p3 imports, the component would fail to instantiate or trap; a clean
//! return proves p3 works end to end. The result is fed into a trap-guard so the
//! optimizer can't elide the call.

wit_bindgen::generate!({
    world: "p3-random",
    path: "wit",
    generate_all,
});

use wasi::random::random::get_random_u64;

struct Component;

impl Guest for Component {
    fn run() {
        // Mix several p3 random draws; they must not all be identical (a stubbed
        // import returning a constant would be caught here) — trap if they are.
        let a = get_random_u64();
        let b = get_random_u64();
        let c = get_random_u64();
        if a == b && b == c {
            unreachable!("p3 random returned a constant");
        }
    }
}

export!(Component);
