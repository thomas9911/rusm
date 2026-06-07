//! A supervisor component: spawn + monitor the `flaky` child and restart it
//! one-for-one when it dies.

wit_bindgen::generate!({
    world: "process",
    path: "wit",
    with: { "rusm:runtime/actor@0.1.0": rusm_rs::rusm::runtime::actor },
});

struct Component;

impl Guest for Component {
    fn run() {
        rusm_rs::Supervisor::new(rusm_rs::Strategy::OneForOne)
            .child("flaky")
            .run();
    }
}

export!(Component);
