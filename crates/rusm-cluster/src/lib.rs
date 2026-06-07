//! # rusm-cluster — distributed RUSM over QUIC + TLS
//!
//! A [`ClusterNode`] wraps a Wasm-free [`rusm_otp::Runtime`] with a QUIC endpoint
//! (TLS 1.3, rustls + ring) so processes can message each other **across nodes**.
//! It is deliberately thin: a node is `(runtime, endpoint, peer table)`, and a
//! cross-node send is "open a QUIC uni-stream, write a framed `(name, bytes)`, the
//! peer routes it into its local registry". No lattice, no brokers — the same
//! actor model as a single node, with the wire in between.
//!
//! ## Security
//! Every link is QUIC, i.e. TLS 1.3 — encrypted and authenticated. For now a
//! cluster shares one self-signed [`Identity`] (a *pre-shared cluster certificate*):
//! a node only completes a handshake with a peer presenting the same cert, and the
//! client pins that cert as its sole trust root. Per-node certificates signed by a
//! cluster CA are a later refinement; the transport seam does not change.
//!
//! ## Addressing
//! Nodes have names. On connect both ends exchange a `Hello`, so each side learns
//! the other's node name and keeps the connection in a peer table. A message is
//! then addressed as `(node_name, registered_process_name)` — or sent directly
//! through the [`RemoteNode`] handle returned by [`ClusterNode::connect`].
//!
//! This is the Phase 9 transport foundation; remote spawn and a global registry
//! build on top of it.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Once, RwLock};

use anyhow::{anyhow, Context as _, Result};
use quinn::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use quinn::{ClientConfig, Connection, Endpoint, ServerConfig};
use rusm_otp::Runtime;

/// The TLS server name presented and verified across a cluster. Connections pin
/// the shared [`Identity`] as their trust root, so this is a fixed SAN, not a
/// routable hostname (peers are reached by socket address).
const CLUSTER_SERVER_NAME: &str = "rusm-node";

/// Largest cross-node message we will buffer off a single uni-stream (16 MiB).
const MAX_FRAME: usize = 16 << 20;

/// Upper bound on a node name read during the handshake — names are short labels.
const MAX_NODE_NAME: usize = 1 << 10;

/// rustls 0.23 needs a process-wide default crypto provider; install ring once.
fn ensure_crypto() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = quinn::rustls::crypto::ring::default_provider().install_default();
    });
}

/// A self-signed node identity (certificate + private key). A whole cluster shares
/// one `Identity` today — see the [module docs](crate#security).
#[derive(Clone)]
pub struct Identity {
    cert: CertificateDer<'static>,
    key_der: Arc<Vec<u8>>,
}

impl Identity {
    /// Generate a fresh self-signed identity for the cluster.
    pub fn generate() -> Result<Self> {
        let cert = rcgen::generate_simple_self_signed(vec![CLUSTER_SERVER_NAME.to_string()])
            .context("generating self-signed cluster certificate")?;
        let key_der = cert.serialize_private_key_der();
        let cert_der = cert.serialize_der().context("serializing certificate")?;
        Ok(Self {
            cert: CertificateDer::from(cert_der),
            key_der: Arc::new(key_der),
        })
    }

    fn server_config(&self) -> Result<ServerConfig> {
        let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(self.key_der.as_ref().clone()));
        ServerConfig::with_single_cert(vec![self.cert.clone()], key)
            .context("building QUIC server config")
    }

    fn client_config(&self) -> Result<ClientConfig> {
        let mut roots = quinn::rustls::RootCertStore::empty();
        roots
            .add(self.cert.clone())
            .context("pinning cluster certificate as trust root")?;
        ClientConfig::with_root_certificates(Arc::new(roots)).context("building QUIC client config")
    }
}

struct Inner {
    name: String,
    rt: Runtime,
    endpoint: Endpoint,
    client_config: ClientConfig,
    /// node name → live connection, populated as handshakes complete. A `RwLock`
    /// because cross-node sends (reads) far outnumber peer churn (writes).
    peers: RwLock<HashMap<String, Connection>>,
}

/// A node in a RUSM cluster: a local runtime plus a QUIC endpoint that connects to
/// peers and routes cross-node messages into the local registry. Cheap to clone
/// (shares one `Arc` of state), so process bodies can capture it to message peers.
#[derive(Clone)]
pub struct ClusterNode {
    inner: Arc<Inner>,
}

impl ClusterNode {
    /// Bind a node named `name` on `addr`, serving `rt`'s registry to the cluster.
    /// Pass `127.0.0.1:0` for an OS-assigned port (read it back with
    /// [`local_addr`](Self::local_addr)).
    pub fn bind(
        name: impl Into<String>,
        rt: Runtime,
        addr: SocketAddr,
        id: &Identity,
    ) -> Result<Self> {
        ensure_crypto();
        let endpoint = Endpoint::server(id.server_config()?, addr)
            .with_context(|| format!("binding QUIC endpoint on {addr}"))?;
        let node = Self {
            inner: Arc::new(Inner {
                name: name.into(),
                rt,
                endpoint,
                client_config: id.client_config()?,
                peers: RwLock::new(HashMap::new()),
            }),
        };
        let acceptor = node.clone();
        tokio::spawn(async move { acceptor.accept_loop().await });
        Ok(node)
    }

    /// This node's name.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// The socket address this node is actually bound to.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.inner
            .endpoint
            .local_addr()
            .context("reading local addr")
    }

    /// Connect to a peer node at `addr`. Completes the handshake — both ends learn
    /// each other's name over a dedicated control stream — before returning a
    /// handle to the peer.
    pub async fn connect(&self, addr: SocketAddr) -> Result<RemoteNode> {
        let conn = self
            .inner
            .endpoint
            .connect_with(self.inner.client_config.clone(), addr, CLUSTER_SERVER_NAME)
            .context("dialing peer")?
            .await
            .context("establishing QUIC connection")?;
        let peer = self.handshake_as_dialer(&conn).await?;
        self.serve_peer(peer.clone(), conn);
        Ok(RemoteNode {
            node: self.clone(),
            name: peer,
        })
    }

    /// Send `payload` to the process registered as `to_name` on node `to_node`.
    /// Errors if we have no live connection to `to_node`.
    pub async fn send(&self, to_node: &str, to_name: &str, payload: &[u8]) -> Result<()> {
        let conn = self
            .inner
            .peers
            .read()
            .unwrap()
            .get(to_node)
            .cloned()
            .ok_or_else(|| anyhow!("no connection to node {to_node:?}"))?;
        send_message(&conn, to_name, payload).await
    }

    /// The names of peer nodes this node currently has a live connection to.
    pub fn peers(&self) -> Vec<String> {
        self.inner.peers.read().unwrap().keys().cloned().collect()
    }

    async fn accept_loop(self) {
        while let Some(incoming) = self.inner.endpoint.accept().await {
            let node = self.clone();
            tokio::spawn(async move {
                let peer = match incoming.await {
                    Ok(conn) => match node.handshake_as_acceptor(&conn).await {
                        Ok(peer) => {
                            node.serve_peer(peer, conn);
                            return;
                        }
                        Err(err) => err,
                    },
                    Err(err) => err.into(),
                };
                tracing::warn!(%peer, "cluster: peer connection failed");
            });
        }
    }

    /// The dialer opens the control stream, announces itself, then reads the
    /// acceptor's name. A bidirectional stream makes the handshake unambiguous and
    /// independent of how data streams happen to interleave.
    async fn handshake_as_dialer(&self, conn: &Connection) -> Result<String> {
        let (mut send, mut recv) = conn.open_bi().await.context("opening control stream")?;
        send.write_all(self.inner.name.as_bytes())
            .await
            .context("announcing node name")?;
        send.finish().context("finishing control stream")?;
        read_node_name(&mut recv).await
    }

    /// The acceptor reads the dialer's name off the control stream, then announces
    /// its own — the mirror of [`handshake_as_dialer`](Self::handshake_as_dialer).
    async fn handshake_as_acceptor(&self, conn: &Connection) -> Result<String> {
        let (mut send, mut recv) = conn.accept_bi().await.context("accepting control stream")?;
        let peer = read_node_name(&mut recv).await?;
        send.write_all(self.inner.name.as_bytes())
            .await
            .context("announcing node name")?;
        send.finish().context("finishing control stream")?;
        Ok(peer)
    }

    /// Record a connected peer and start routing its messages into the registry.
    fn serve_peer(&self, peer: String, conn: Connection) {
        self.inner.peers.write().unwrap().insert(peer, conn.clone());
        let node = self.clone();
        tokio::spawn(async move { node.delivery_loop(conn).await });
    }

    /// Read messages off `conn`'s uni-streams and route each into the local
    /// registry. Each message is its own stream — independent, with no
    /// head-of-line blocking between them (the reason to use QUIC over TCP).
    async fn delivery_loop(self, conn: Connection) {
        while let Ok(mut recv) = conn.accept_uni().await {
            let node = self.clone();
            tokio::spawn(async move {
                let Ok(frame) = recv.read_to_end(MAX_FRAME).await else {
                    return;
                };
                if let Some((name, payload)) = parse_message(&frame) {
                    if let Some(pid) = node.inner.rt.whereis(name) {
                        node.inner.rt.send(pid, payload.to_vec());
                    }
                }
            });
        }
    }
}

/// A handle to a connected peer node, returned by [`ClusterNode::connect`].
#[derive(Clone)]
pub struct RemoteNode {
    node: ClusterNode,
    name: String,
}

impl RemoteNode {
    /// The peer node's name (learned during the handshake).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Send `payload` to the process registered as `to_name` on this peer.
    pub async fn send(&self, to_name: &str, payload: &[u8]) -> Result<()> {
        self.node.send(&self.name, to_name, payload).await
    }
}

/// Read a node name announced on a control stream (the whole stream is the name).
async fn read_node_name(recv: &mut quinn::RecvStream) -> Result<String> {
    let bytes = recv
        .read_to_end(MAX_NODE_NAME)
        .await
        .context("reading peer node name")?;
    String::from_utf8(bytes).context("peer node name was not valid UTF-8")
}

/// Send one message — `[name_len: u32 LE][name][payload]` — on its own uni-stream.
async fn send_message(conn: &Connection, name: &str, payload: &[u8]) -> Result<()> {
    let mut frame = Vec::with_capacity(4 + name.len() + payload.len());
    frame.extend_from_slice(&(name.len() as u32).to_le_bytes());
    frame.extend_from_slice(name.as_bytes());
    frame.extend_from_slice(payload);

    let mut send = conn.open_uni().await.context("opening message stream")?;
    send.write_all(&frame).await.context("writing message")?;
    send.finish().context("finishing message stream")?;
    Ok(())
}

/// Parse a message frame into its `(registered_name, payload)`. Returns `None` for
/// a truncated or non-UTF-8 name rather than trusting unvalidated wire bytes.
fn parse_message(buf: &[u8]) -> Option<(&str, &[u8])> {
    let (len_bytes, rest) = buf.split_at_checked(4)?;
    let nlen = u32::from_le_bytes(len_bytes.try_into().ok()?) as usize;
    let (name, payload) = rest.split_at_checked(nlen)?;
    Some((std::str::from_utf8(name).ok()?, payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::oneshot;

    fn localhost() -> SocketAddr {
        "127.0.0.1:0".parse().unwrap()
    }

    /// Spawn a process on `rt` registered as `name` that forwards its first
    /// message over a oneshot, so a test can await cross-node delivery.
    fn inbox(rt: &Runtime, name: &str) -> oneshot::Receiver<Vec<u8>> {
        let (tx, rx) = oneshot::channel();
        let handle = rt.spawn(|mut ctx| async move {
            let msg = ctx.recv().await.message().unwrap();
            let _ = tx.send(msg);
        });
        assert!(rt.register(name, handle.pid()));
        rx
    }

    /// Await a cross-node delivery with a generous ceiling (loopback is instant;
    /// the timeout only guards against a hang).
    async fn recv(rx: oneshot::Receiver<Vec<u8>>) -> Vec<u8> {
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .expect("delivery timed out")
            .unwrap()
    }

    /// Poll until `node` has registered a live connection to `peer`. Handshakes
    /// complete in well under a millisecond on loopback; this only avoids a race.
    async fn await_peer(node: &ClusterNode, peer: &str) {
        for _ in 0..500 {
            if node.peers().iter().any(|p| p == peer) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("node {:?} never connected to {peer:?}", node.name());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_node_messages_a_process_on_another_node() {
        let id = Identity::generate().unwrap();

        let rt_b = Runtime::new();
        let rx = inbox(&rt_b, "inbox");
        let node_b = ClusterNode::bind("B", rt_b, localhost(), &id).unwrap();
        let addr_b = node_b.local_addr().unwrap();

        let rt_a = Runtime::new();
        let node_a = ClusterNode::bind("A", rt_a, localhost(), &id).unwrap();
        let remote = node_a.connect(addr_b).await.unwrap();

        // The handshake taught each side the other's name.
        assert_eq!(remote.name(), "B");
        assert_eq!(node_a.peers(), vec!["B".to_string()]);

        remote
            .send("inbox", b"hello across the cluster")
            .await
            .unwrap();
        assert_eq!(recv(rx).await, b"hello across the cluster");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn messages_route_by_node_name() {
        let id = Identity::generate().unwrap();

        let rt_b = Runtime::new();
        let rx = inbox(&rt_b, "worker");
        let node_b = ClusterNode::bind("beta", rt_b, localhost(), &id).unwrap();
        let addr_b = node_b.local_addr().unwrap();

        let node_a = ClusterNode::bind("alpha", Runtime::new(), localhost(), &id).unwrap();
        node_a.connect(addr_b).await.unwrap();

        // Address the peer by node name rather than holding its handle.
        node_a.send("beta", "worker", b"by name").await.unwrap();
        assert_eq!(recv(rx).await, b"by name");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_single_link_carries_messages_both_ways() {
        let id = Identity::generate().unwrap();

        let rt_a = Runtime::new();
        let rx_a = inbox(&rt_a, "a-inbox");
        let node_a = ClusterNode::bind("A", rt_a, localhost(), &id).unwrap();

        let rt_b = Runtime::new();
        let rx_b = inbox(&rt_b, "b-inbox");
        let node_b = ClusterNode::bind("B", rt_b, localhost(), &id).unwrap();

        // A dials B once; that one link is usable in both directions.
        node_a.connect(node_b.local_addr().unwrap()).await.unwrap();
        node_a.send("B", "b-inbox", b"a->b").await.unwrap();

        // B learns about A as the handshake settles on its side, then replies.
        await_peer(&node_b, "A").await;
        node_b.send("A", "a-inbox", b"b->a").await.unwrap();

        assert_eq!(recv(rx_b).await, b"a->b");
        assert_eq!(recv(rx_a).await, b"b->a");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sending_to_an_unknown_node_errors() {
        let node = ClusterNode::bind(
            "solo",
            Runtime::new(),
            localhost(),
            &Identity::generate().unwrap(),
        )
        .unwrap();
        let err = node.send("ghost", "inbox", b"x").await.unwrap_err();
        assert!(err.to_string().contains("ghost"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_wrong_certificate_is_rejected() {
        let server_id = Identity::generate().unwrap();
        let node_b = ClusterNode::bind("B", Runtime::new(), localhost(), &server_id).unwrap();
        let addr_b = node_b.local_addr().unwrap();

        // A different identity → the pinned trust root won't match the peer's cert.
        let other_id = Identity::generate().unwrap();
        let node_a = ClusterNode::bind("A", Runtime::new(), localhost(), &other_id).unwrap();
        assert!(node_a.connect(addr_b).await.is_err());
    }

    #[test]
    fn parse_message_rejects_malformed_input() {
        assert!(parse_message(&[]).is_none()); // no length prefix
        assert!(parse_message(&[0, 0]).is_none()); // truncated length prefix
        assert!(parse_message(&[9, 0, 0, 0, b'h', b'i']).is_none()); // name longer than buffer
        assert!(parse_message(&[2, 0, 0, 0, 0xff, 0xfe]).is_none()); // non-UTF-8 name
    }

    #[test]
    fn parse_message_round_trips_a_well_formed_frame() {
        let mut frame = Vec::new();
        frame.extend_from_slice(&3u32.to_le_bytes());
        frame.extend_from_slice(b"job");
        frame.extend_from_slice(b"payload-bytes");
        assert_eq!(parse_message(&frame), Some(("job", &b"payload-bytes"[..])));
    }
}
