use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;

use crate::profile::ResourceProfile;
use crate::protocol::{ClientCommand, ServerMessage};
use crate::runner::{Runner, RunnerConfig};
use crate::scenario::{Scenario, ScenarioMeta};

/// A running RUSM benchmark node: owns the [`Runner`] and answers client
/// commands. Networking lives in [`serve_on`]; everything decision-making is on
/// `Node` so it is unit-testable without a socket.
pub struct Node {
    runner: Mutex<Runner>,
    scenarios: Vec<ScenarioMeta>,
    started: Instant,
    tick_period: Duration,
}

impl Node {
    pub fn new(config: RunnerConfig) -> Arc<Self> {
        let tick_period = Duration::from_millis(1_000 / u64::from(config.ticks_per_second.max(1)));
        Arc::new(Self {
            runner: Mutex::new(Runner::new(config)),
            scenarios: Scenario::all_meta(),
            started: Instant::now(),
            tick_period,
        })
    }

    pub fn hello(&self) -> ServerMessage {
        ServerMessage::Hello {
            scenarios: self.scenarios.clone(),
            profiles: ResourceProfile::all_meta(),
            instance_capacity: rusm_wasm::DEFAULT_MAX_INSTANCES,
        }
    }

    pub fn uptime_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    pub fn tick_period(&self) -> Duration {
        self.tick_period
    }

    /// Whether a scenario is currently running — the ticker broadcasts at full rate
    /// only while running, and just heartbeats when idle (no flood of empty frames).
    pub fn is_running(&self) -> bool {
        self.runner
            .lock()
            .expect("runner mutex poisoned")
            .is_running()
    }

    /// Applies a client command; `Err` carries a human-readable reason.
    pub fn apply(&self, command: ClientCommand) -> Result<(), String> {
        let mut runner = self.runner.lock().expect("runner mutex poisoned");
        match command {
            ClientCommand::Run { scenario } => {
                let scenario = Scenario::from_id(&scenario)
                    .ok_or_else(|| format!("unknown scenario: {scenario}"))?;
                runner.start(scenario);
            }
            ClientCommand::Stop => runner.stop(),
            ClientCommand::SetObserverDetail { enabled } => runner.set_observer_detail(enabled),
            ClientCommand::SetResourceProfile { profile } => {
                let profile = ResourceProfile::from_id(&profile)
                    .ok_or_else(|| format!("unknown profile: {profile}"))?;
                runner.set_resource_profile(profile);
            }
        }
        Ok(())
    }

    pub fn tick_message(&self) -> ServerMessage {
        let uptime = self.uptime_ms();
        let frame = self
            .runner
            .lock()
            .expect("runner mutex poisoned")
            .tick(uptime);
        ServerMessage::Tick {
            frame: Box::new(frame),
        }
    }
}

/// Binds `addr` and serves until error. See [`serve_on`].
pub async fn serve(addr: &str, node: Arc<Node>) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    serve_on(listener, node).await
}

/// Serves on an already-bound listener (lets tests bind an ephemeral port).
///
/// A single ticker task drives the node and broadcasts each frame; every
/// connection subscribes to that broadcast, so one tick fans out to all clients.
pub async fn serve_on(listener: TcpListener, node: Arc<Node>) -> std::io::Result<()> {
    let (tx, _) = broadcast::channel(64);
    tokio::spawn(ticker(node.clone(), tx.clone()));

    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(handle_connection(stream, node.clone(), tx.subscribe()));
    }
}

async fn ticker(node: Arc<Node>, tx: broadcast::Sender<ServerMessage>) {
    let mut interval = tokio::time::interval(node.tick_period());
    // Broadcast every tick while a scenario runs (the live chart needs it), plus the
    // single running→idle frame the moment it stops. An idle node is then **silent** —
    // no stream at all (a client renders the idle state from no frame).
    let mut was_running = false;
    loop {
        interval.tick().await;
        let running = node.is_running();
        if running || was_running {
            let _ = tx.send(node.tick_message());
        }
        was_running = running;
    }
}

async fn handle_connection(
    stream: TcpStream,
    node: Arc<Node>,
    mut frames: broadcast::Receiver<ServerMessage>,
) {
    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let (mut write, mut read) = ws.split();

    if send(&mut write, node.hello()).await.is_err() {
        return;
    }
    // No snapshot frame here: an idle client renders correctly from no frame, and a
    // client connecting mid-run gets the next broadcast within one tick. Crucially,
    // `tick()` mutates the rate state, so it must be called *only* by the ticker —
    // never per-connection, or a reconnect would perturb the live throughput.

    loop {
        tokio::select! {
            frame = frames.recv() => match frame {
                Ok(message) => {
                    if send(&mut write, message).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
            incoming = read.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if let Err(reason) = ClientCommand::from_json(text.as_str())
                        .map_err(|e| e.to_string())
                        .and_then(|cmd| node.apply(cmd))
                    {
                        let _ = send(&mut write, ServerMessage::Error { message: reason }).await;
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(_)) => break,
            },
        }
    }
}

async fn send<S>(write: &mut S, message: ServerMessage) -> Result<(), ()>
where
    S: SinkExt<Message> + Unpin,
{
    write
        .send(Message::Text(message.to_json().into()))
        .await
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_lists_scenarios_and_profiles() {
        let node = Node::new(RunnerConfig::default());
        let hello = node.hello();
        assert_eq!(hello.scenarios().unwrap().len(), Scenario::ALL.len());
        assert_eq!(hello.profiles().unwrap().len(), ResourceProfile::ALL.len());
    }

    #[test]
    fn an_idle_node_is_not_running() {
        // The ticker uses this to heartbeat (not flood) when nothing is running.
        let node = Node::new(RunnerConfig::default());
        assert!(!node.is_running());
        assert!(!node.tick_message().tick_frame().unwrap().running);
    }

    #[test]
    fn apply_set_resource_profile() {
        let node = Node::new(RunnerConfig::default());
        node.apply(ClientCommand::SetResourceProfile {
            profile: "light".to_string(),
        })
        .unwrap();
        assert_eq!(node.tick_message().tick_frame().unwrap().profile, "light");

        let err = node
            .apply(ClientCommand::SetResourceProfile {
                profile: "nope".to_string(),
            })
            .unwrap_err();
        assert!(err.contains("unknown profile"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_run_starts_known_scenario() {
        // Run starts the scenario's real engine, so this needs a Tokio runtime.
        let node = Node::new(RunnerConfig::default());
        node.apply(ClientCommand::Run {
            scenario: "distributed-fanout".to_string(),
        })
        .unwrap();
        let message = node.tick_message();
        let frame = message.tick_frame().unwrap();
        assert!(frame.running);
        assert_eq!(frame.scenario.as_deref(), Some("distributed-fanout"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn node_serves_http_throughput_over_the_websocket() {
        // The EXACT dashboard path: serve_on + ticker + a WebSocket client sending
        // Run and reading frames. Reproduces what `make dashboard` does, end to end.
        let node = Node::new(RunnerConfig::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve_on(listener, node));

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/"))
            .await
            .unwrap();
        ws.send(Message::Text(
            ClientCommand::Run {
                scenario: "http-throughput".to_string(),
            }
            .to_json()
            .into(),
        ))
        .await
        .unwrap();

        let start = Instant::now();
        let mut max_ops = 0.0_f64;
        while start.elapsed() < Duration::from_secs(6) {
            let Some(Ok(Message::Text(text))) = ws.next().await else {
                continue;
            };
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if v["type"] == "tick" && v["frame"]["scenario"] == "http-throughput" {
                    max_ops = v["frame"]["ops_per_sec"]
                        .as_f64()
                        .unwrap_or(0.0)
                        .max(max_ops);
                }
            }
        }
        assert!(
            max_ops > 1000.0,
            "node served http over the WebSocket (max {max_ops:.0}/s)"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn run_stop_run_still_produces_throughput() {
        // The dashboard flow over the node API: Run → Stop → Run. The second run must
        // still produce throughput, and an idle node must report not-running.
        let node = Node::new(RunnerConfig::default());
        async fn run_until_throughput(node: &Node) -> bool {
            node.apply(ClientCommand::Run {
                scenario: "spawn-storm".to_string(),
            })
            .unwrap();
            for _ in 0..200 {
                tokio::time::sleep(Duration::from_millis(10)).await;
                if node.tick_message().tick_frame().unwrap().ops_per_sec > 0.0 {
                    return true;
                }
            }
            false
        }
        assert!(
            run_until_throughput(&node).await,
            "first run produces throughput"
        );
        node.apply(ClientCommand::Stop).unwrap();
        assert!(!node.is_running(), "stopped node is idle");
        assert!(
            run_until_throughput(&node).await,
            "second run (after stop) still produces throughput"
        );
    }

    #[test]
    fn apply_run_rejects_unknown_scenario() {
        let node = Node::new(RunnerConfig::default());
        let err = node
            .apply(ClientCommand::Run {
                scenario: "nope".to_string(),
            })
            .unwrap_err();
        assert!(err.contains("unknown scenario"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_stop_and_detail_toggle() {
        let node = Node::new(RunnerConfig::default());
        node.apply(ClientCommand::Run {
            scenario: "distributed-fanout".to_string(),
        })
        .unwrap();
        node.apply(ClientCommand::SetObserverDetail { enabled: false })
            .unwrap();
        node.apply(ClientCommand::Stop).unwrap();
        assert!(!node.tick_message().tick_frame().unwrap().running);
    }

    #[test]
    fn tick_period_derives_from_rate() {
        let node = Node::new(RunnerConfig {
            ticks_per_second: 20,
            ..RunnerConfig::default()
        });
        assert_eq!(node.tick_period(), Duration::from_millis(50));
    }

    #[tokio::test]
    async fn serve_returns_error_for_unbindable_address() {
        let node = Node::new(RunnerConfig::default());
        assert!(serve("definitely not an address", node).await.is_err());
    }
}
