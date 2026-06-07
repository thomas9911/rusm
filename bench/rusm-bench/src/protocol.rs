use rusm_metrics::{LatencySnapshot, TimeSeriesSnapshot};
use rusm_observer::ObserverSnapshot;
use serde::{Deserialize, Serialize};

use crate::profile::ResourceProfileMeta;
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
    /// The active resource profile's id (`light` / `balanced` / `max`).
    pub profile: String,
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
    SetResourceProfile { profile: String },
}

/// A message from the node to a client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Sent on connect: the scenario menu, the resource-profile menu, and the
    /// Wasm pool capacity (the reserved ceiling the Observer shows live usage against).
    Hello {
        scenarios: Vec<ScenarioMeta>,
        profiles: Vec<ResourceProfileMeta>,
        instance_capacity: u32,
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
            ServerMessage::Hello { scenarios, .. } => Some(scenarios),
            _ => None,
        }
    }

    /// The resource-profile menu, if this is a [`ServerMessage::Hello`].
    pub fn profiles(&self) -> Option<&[ResourceProfileMeta]> {
        match self {
            ServerMessage::Hello { profiles, .. } => Some(profiles),
            _ => None,
        }
    }

    /// The Wasm pool capacity, if this is a [`ServerMessage::Hello`].
    pub fn instance_capacity(&self) -> Option<u32> {
        match self {
            ServerMessage::Hello {
                instance_capacity, ..
            } => Some(*instance_capacity),
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
            ClientCommand::SetResourceProfile {
                profile: "max".to_string(),
            },
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
            profiles: crate::profile::ResourceProfile::all_meta(),
            instance_capacity: 1024,
        };
        assert!(hello.scenarios().is_some());
        assert!(hello.profiles().is_some());
        assert_eq!(hello.instance_capacity(), Some(1024));
        assert!(hello.tick_frame().is_none());

        let mut runner = crate::runner::Runner::new(crate::runner::RunnerConfig::default());
        runner.start_synthetic(crate::scenario::Scenario::DistributedFanout);
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
        assert!(error.profiles().is_none());
    }
}
