use rusm_metrics::{LatencySnapshot, TimeSeriesSnapshot};
use rusm_observer::ObserverSnapshot;
use serde::{Deserialize, Serialize};

use crate::scenario::ScenarioMeta;

/// One sampled frame of a run, broadcast to every attached client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    /// The running scenario's id, or `None` when idle.
    pub scenario: Option<String>,
    pub running: bool,
    pub uptime_ms: u64,
    pub ops_per_sec: f64,
    pub peak_concurrent: u64,
    pub latency: LatencySnapshot,
    pub throughput: TimeSeriesSnapshot,
    pub observer: ObserverSnapshot,
}

/// A command from a client (dashboard or `rusm attach`) to the node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientCommand {
    Run { scenario: String },
    Stop,
    SetObserverDetail { enabled: bool },
}

/// A message from the node to a client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Sent on connect: the scenario menu.
    Hello {
        scenarios: Vec<ScenarioMeta>,
    },
    Tick {
        frame: Box<Frame>,
    },
    Error {
        message: String,
    },
}

impl ServerMessage {
    /// The frame, if this is a [`ServerMessage::Tick`].
    pub fn tick_frame(&self) -> Option<&Frame> {
        match self {
            ServerMessage::Tick { frame } => Some(frame),
            _ => None,
        }
    }

    /// The scenario menu, if this is a [`ServerMessage::Hello`].
    pub fn scenarios(&self) -> Option<&[ScenarioMeta]> {
        match self {
            ServerMessage::Hello { scenarios } => Some(scenarios),
            _ => None,
        }
    }
}

impl ClientCommand {
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("ClientCommand always serialises")
    }
}

impl ServerMessage {
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("ServerMessage always serialises")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_command_round_trips_tagged() {
        let cmd = ClientCommand::Run {
            scenario: "connection-storm".to_string(),
        };
        let json = cmd.to_json();
        assert!(json.contains("\"type\":\"run\""));
        assert_eq!(ClientCommand::from_json(&json).unwrap(), cmd);
    }

    #[test]
    fn client_command_variants_round_trip() {
        for cmd in [
            ClientCommand::Stop,
            ClientCommand::SetObserverDetail { enabled: false },
        ] {
            assert_eq!(ClientCommand::from_json(&cmd.to_json()).unwrap(), cmd);
        }
    }

    #[test]
    fn rejects_malformed_command() {
        assert!(ClientCommand::from_json("{\"type\":\"nope\"}").is_err());
    }

    #[test]
    fn server_message_round_trips() {
        let msg = ServerMessage::Error {
            message: "boom".to_string(),
        };
        assert_eq!(ServerMessage::from_json(&msg.to_json()).unwrap(), msg);
    }

    #[test]
    fn accessors_extract_the_right_variant() {
        let hello = ServerMessage::Hello {
            scenarios: crate::scenario::Scenario::all_meta(),
        };
        assert!(hello.scenarios().is_some());
        assert!(hello.tick_frame().is_none());

        let mut runner = crate::runner::Runner::new(crate::runner::RunnerConfig::default());
        runner.start(crate::scenario::Scenario::PingPong);
        let tick = ServerMessage::Tick {
            frame: Box::new(runner.tick(0)),
        };
        assert!(tick.tick_frame().is_some());
        assert!(tick.scenarios().is_none());

        let error = ServerMessage::Error {
            message: "x".to_string(),
        };
        assert!(error.tick_frame().is_none());
        assert!(error.scenarios().is_none());
    }
}
