use rusm_bench::{summarize_frame, ServerMessage};

/// Renders a server message as a line (or block) for the REPL display.
pub fn render_message(message: &ServerMessage) -> String {
    match message {
        ServerMessage::Hello { scenarios, .. } => {
            let mut out = String::from("connected. scenarios:");
            for s in scenarios {
                out.push_str(&format!("\n  {:<20} {}", s.id, s.label));
            }
            out
        }
        ServerMessage::Tick { frame } => summarize_frame(frame),
        ServerMessage::Error { message } => format!("error: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusm_bench::{Node, Runner, RunnerConfig, Scenario};

    #[test]
    fn renders_hello_with_scenarios() {
        let node = Node::new(RunnerConfig::default());
        let out = render_message(&node.hello());
        assert!(out.starts_with("connected. scenarios:"));
        for s in Scenario::ALL {
            assert!(out.contains(s.id()));
        }
    }

    #[test]
    fn renders_tick_as_summary() {
        let mut runner = Runner::new(RunnerConfig::default());
        runner.start(Scenario::DistributedFanout);
        let message = ServerMessage::Tick {
            frame: Box::new(runner.tick(10)),
        };
        assert!(render_message(&message).contains("distributed-fanout"));
    }

    #[test]
    fn renders_error() {
        let message = ServerMessage::Error {
            message: "nope".to_string(),
        };
        assert_eq!(render_message(&message), "error: nope");
    }
}
