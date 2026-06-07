//! # rusm-cluster — distributed RUSM over QUIC + TLS
//!
//! A [`ClusterNode`] wraps a Wasm-free [`rusm_otp::Runtime`] with a QUIC endpoint
//! (TLS 1.3, rustls + ring) so processes can message each other **across nodes**.
//! It is deliberately thin: a node is `(runtime, endpoint, peers, global registry)`,
//! and a cross-node send is "open a QUIC uni-stream, write a framed `(name, bytes)`,
//! the peer routes it into its local registry". No lattice, no brokers — the same
//! actor model as a single node, with the wire in between.
//!
//! ## Per-peer streams
//! Each link carries two kinds of stream:
//! - a single long-lived **control stream** (the bidirectional stream opened during
//!   the handshake), used for node-name exchange and **global-registry gossip**;
//! - one **uni-stream per message**, so messages never head-of-line-block each
//!   other (the reason to reach for QUIC over TCP).
//!
//! ## Addressing
//! Nodes have names. A message is addressed either explicitly as
//! `(node_name, registered_process_name)` via [`ClusterNode::send`] / the
//! [`RemoteNode`] handle, or by a **cluster-wide name** via
//! [`ClusterNode::send_global`] — the node resolves which peer owns that name from
//! its [global registry](ClusterNode::register_global) and routes there.
//!
//! ## Security
//! Every link is QUIC (TLS 1.3) and **mutually authenticated**: both ends present a
//! certificate and verify the other against a trust anchor, so a peer without a
//! trusted certificate is rejected at the handshake. Two trust models:
//! - [`ClusterCa`] issues a **per-node** certificate under a shared CA — each node
//!   holds its own key and is independently revocable (recommended);
//! - [`Identity::generate`] makes a single self-signed certificate shared across a
//!   small/trusted cluster (simpler, not per-node revocable).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once, RwLock};

use anyhow::{anyhow, Context as _, Result};
use quinn::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use quinn::{ClientConfig, Connection, Endpoint, RecvStream, SendStream, ServerConfig};
use rcgen::{BasicConstraints, Certificate, CertificateParams, IsCa};
use rusm_otp::{Pid, Runtime};
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

/// A named factory a node knows how to run on request from a peer: given the local
/// runtime and the caller's argument bytes, it spawns a process and returns its
/// pid. Registered with [`ClusterNode::register_spawnable`] and invoked remotely
/// with [`ClusterNode::spawn_remote`]. (The cluster can't ship a closure across the
/// wire, so a node spawns only work it has been taught to build.)
pub type Spawnable = Arc<dyn Fn(&Runtime, Vec<u8>) -> Pid + Send + Sync>;

/// The outcome of a control-plane RPC: the op's reply bytes, or an error string.
type RpcReply = Result<Vec<u8>, String>;

/// The TLS server name every node certificate carries as a SAN, presented and
/// verified on each connection. A fixed cluster-wide name, not a routable hostname
/// (peers are reached by socket address); peer trust comes from the mutual-TLS
/// certificate check against the shared trust anchor, and the node's real name is
/// exchanged in the handshake.
const CLUSTER_SERVER_NAME: &str = "rusm-node";

/// Largest cross-node message we will buffer off a single uni-stream (16 MiB).
const MAX_FRAME: usize = 16 << 20;

/// Upper bound on a length-prefixed control frame (node names, gossip — all small).
const MAX_CONTROL_FRAME: usize = 64 << 10;

/// rustls 0.23 needs a process-wide default crypto provider; install ring once.
fn ensure_crypto() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = quinn::rustls::crypto::ring::default_provider().install_default();
    });
}

/// A **cluster certificate authority**: issues a per-node [`Identity`] signed by a
/// shared CA. Unlike a single pre-shared cluster certificate, each node then holds
/// its **own** private key, and a compromised node can be excluded by rotating the
/// CA without re-keying every other node. Generate one CA per cluster and hand each
/// node an identity from [`issue`](ClusterCa::issue).
pub struct ClusterCa {
    cert: Certificate,
    cert_der: CertificateDer<'static>,
}

impl ClusterCa {
    /// Generate a fresh cluster CA.
    pub fn generate() -> Result<Self> {
        let mut params = CertificateParams::new(vec!["rusm-cluster-ca".to_string()]);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let cert = Certificate::from_params(params).context("generating cluster CA")?;
        let cert_der = CertificateDer::from(cert.serialize_der().context("serializing CA cert")?);
        Ok(Self { cert, cert_der })
    }

    /// Issue a per-node identity: a fresh keypair plus a certificate for `node_name`
    /// **signed by this CA**. Cluster membership is established by the CA signature;
    /// the node name is embedded for traceability.
    pub fn issue(&self, node_name: &str) -> Result<Identity> {
        let params =
            CertificateParams::new(vec![CLUSTER_SERVER_NAME.to_string(), node_name.to_string()]);
        let node = Certificate::from_params(params).context("generating node certificate")?;
        let cert = CertificateDer::from(
            node.serialize_der_with_signer(&self.cert)
                .context("signing node certificate")?,
        );
        Ok(Identity {
            cert,
            key_der: Arc::new(node.serialize_private_key_der()),
            root: self.cert_der.clone(),
        })
    }
}

/// A node's TLS identity: its certificate, private key, and the trust anchor it
/// checks peers against. Every cluster link is **mutually authenticated** — both
/// ends present a certificate and verify the other against this root, so a peer
/// without a trusted certificate is rejected at the handshake.
///
/// Two ways to create one:
/// - [`ClusterCa::issue`] — a per-node certificate under a shared CA (recommended:
///   each node has its own key and is independently revocable);
/// - [`Identity::generate`] — a single self-signed certificate that is its own
///   trust root, shared across a small/trusted cluster (simpler, not per-node
///   revocable).
#[derive(Clone)]
pub struct Identity {
    cert: CertificateDer<'static>,
    key_der: Arc<Vec<u8>>,
    /// The trust anchor peers are verified against — the CA (for an issued
    /// identity) or the cert itself (for a self-signed one).
    root: CertificateDer<'static>,
}

impl Identity {
    /// Generate a single self-signed identity that is its own trust root — share one
    /// across the cluster. For per-node certificates under a CA, use [`ClusterCa`].
    pub fn generate() -> Result<Self> {
        let cert = rcgen::generate_simple_self_signed(vec![CLUSTER_SERVER_NAME.to_string()])
            .context("generating self-signed cluster certificate")?;
        let cert_der =
            CertificateDer::from(cert.serialize_der().context("serializing certificate")?);
        Ok(Self {
            key_der: Arc::new(cert.serialize_private_key_der()),
            root: cert_der.clone(),
            cert: cert_der,
        })
    }

    fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::from(PrivatePkcs8KeyDer::from(self.key_der.as_ref().clone()))
    }

    fn root_store(&self) -> Result<Arc<quinn::rustls::RootCertStore>> {
        let mut roots = quinn::rustls::RootCertStore::empty();
        roots
            .add(self.root.clone())
            .context("adding cluster trust anchor")?;
        Ok(Arc::new(roots))
    }

    /// A mutual-TLS server config: present our certificate and **require** the
    /// connecting peer to present one signed by our trust anchor.
    fn server_config(&self) -> Result<ServerConfig> {
        let verifier = quinn::rustls::server::WebPkiClientVerifier::builder(self.root_store()?)
            .build()
            .context("building client-certificate verifier")?;
        let crypto = quinn::rustls::ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![self.cert.clone()], self.key())
            .context("building server TLS config")?;
        Ok(ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(crypto)
                .context("building QUIC server config")?,
        )))
    }

    /// A mutual-TLS client config: present our certificate and verify the peer's
    /// against our trust anchor.
    fn client_config(&self) -> Result<ClientConfig> {
        let crypto = quinn::rustls::ClientConfig::builder()
            .with_root_certificates(self.root_store()?)
            .with_client_auth_cert(vec![self.cert.clone()], self.key())
            .context("building client TLS config")?;
        Ok(ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
                .context("building QUIC client config")?,
        )))
    }
}

/// Messages exchanged over a peer's control stream: global-registry gossip and a
/// request/reply control-plane RPC (remote spawn, live-attach listing — correlated
/// by `req`).
#[derive(Serialize, Deserialize)]
enum Control {
    /// `name` is now globally registered on `node`.
    Register { name: String, node: String },
    /// `name` is no longer globally registered.
    Unregister { name: String },
    /// A control-plane RPC: run `op` with `args`, correlated by `req`.
    Request { req: u64, op: String, args: Vec<u8> },
    /// The outcome of request `req`: the op's reply bytes, or why it failed.
    Reply {
        req: u64,
        result: Result<Vec<u8>, String>,
    },
}

/// A live link to one peer node: the connection (for opening message streams) and
/// the shared sender half of its control stream (for pushing gossip).
struct Peer {
    conn: Connection,
    control: Arc<AsyncMutex<SendStream>>,
}

struct Inner {
    name: String,
    rt: Runtime,
    endpoint: Endpoint,
    client_config: ClientConfig,
    /// node name → live peer link. A `RwLock` because sends (reads) far outnumber
    /// peer churn (writes).
    peers: RwLock<HashMap<String, Peer>>,
    /// cluster-wide registered name → the node that owns it (including ourselves).
    globals: RwLock<HashMap<String, String>>,
    /// factory name → how to spawn it when a peer asks.
    spawnables: RwLock<HashMap<String, Spawnable>>,
    /// in-flight control-plane RPCs, by `req` id, awaiting their reply.
    pending: Mutex<HashMap<u64, oneshot::Sender<RpcReply>>>,
    /// monotonic source of control-plane `req` ids.
    next_req: AtomicU64,
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
                globals: RwLock::new(HashMap::new()),
                spawnables: RwLock::new(HashMap::new()),
                pending: Mutex::new(HashMap::new()),
                next_req: AtomicU64::new(0),
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

    /// The local runtime this node serves to the cluster.
    pub fn runtime(&self) -> &Runtime {
        &self.inner.rt
    }

    /// The socket address this node is actually bound to.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.inner
            .endpoint
            .local_addr()
            .context("reading local addr")
    }

    /// Connect to a peer node at `addr`. Completes the handshake — both ends learn
    /// each other's name over the control stream — before returning a handle to the
    /// peer.
    pub async fn connect(&self, addr: SocketAddr) -> Result<RemoteNode> {
        let conn = self
            .inner
            .endpoint
            .connect_with(self.inner.client_config.clone(), addr, CLUSTER_SERVER_NAME)
            .context("dialing peer")?
            .await
            .context("establishing QUIC connection")?;
        let (peer, send, recv) = self.handshake_as_dialer(&conn).await?;
        self.serve_peer(peer.clone(), conn, send, recv);
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
            .map(|p| p.conn.clone())
            .ok_or_else(|| anyhow!("no connection to node {to_node:?}"))?;
        send_message(&conn, to_name, payload).await
    }

    /// Register `pid` under a **cluster-wide** `name`: it is registered locally and
    /// the registration is gossiped to every connected peer, so any node can reach
    /// it with [`send_global`](Self::send_global). Returns `false` if the local
    /// registry already holds `name` (mirroring [`Runtime::register`]).
    pub fn register_global(&self, name: impl Into<String>, pid: Pid) -> bool {
        let name = name.into();
        if !self.inner.rt.register(&name, pid) {
            return false;
        }
        self.inner
            .globals
            .write()
            .unwrap()
            .insert(name.clone(), self.inner.name.clone());
        let node = self.clone();
        let control = Control::Register {
            name,
            node: self.inner.name.clone(),
        };
        tokio::spawn(async move { node.broadcast(&control).await });
        true
    }

    /// Resolve a cluster-wide `name` to the node that currently owns it.
    pub fn whereis_global(&self, name: &str) -> Option<String> {
        self.inner.globals.read().unwrap().get(name).cloned()
    }

    /// Send `payload` to a **cluster-wide** registered `name`, wherever it lives —
    /// delivered locally if we own it, otherwise routed to the owning node. Errors
    /// if the name is unknown or (when local) its process has gone.
    pub async fn send_global(&self, name: &str, payload: &[u8]) -> Result<()> {
        let owner = self
            .whereis_global(name)
            .ok_or_else(|| anyhow!("no global registration for {name:?}"))?;
        if owner == self.inner.name {
            let pid = self
                .inner
                .rt
                .whereis(name)
                .ok_or_else(|| anyhow!("global {name:?} has no live local process"))?;
            self.inner.rt.send(pid, payload.to_vec());
            Ok(())
        } else {
            self.send(&owner, name, payload).await
        }
    }

    /// Teach this node how to spawn `name` on a peer's request — see
    /// [`Spawnable`] and [`spawn_remote`](Self::spawn_remote). Replacing an existing
    /// factory of the same name is allowed (last registration wins).
    pub fn register_spawnable(
        &self,
        name: impl Into<String>,
        factory: impl Fn(&Runtime, Vec<u8>) -> Pid + Send + Sync + 'static,
    ) {
        self.inner
            .spawnables
            .write()
            .unwrap()
            .insert(name.into(), Arc::new(factory));
    }

    /// Ask node `to_node` to run its spawnable `factory` with `args`, and return the
    /// pid it spawned (a handle valid *on that node*). Errors if we are not
    /// connected to `to_node`, the peer has no such factory, or the link drops
    /// before it replies.
    pub async fn spawn_remote(&self, to_node: &str, factory: &str, args: Vec<u8>) -> Result<Pid> {
        let mut payload = Vec::with_capacity(4 + factory.len() + args.len());
        payload.extend_from_slice(&(factory.len() as u32).to_le_bytes());
        payload.extend_from_slice(factory.as_bytes());
        payload.extend_from_slice(&args);

        let reply = self.request(to_node, "spawn", payload).await?;
        let raw = u64::from_le_bytes(
            reply
                .as_slice()
                .try_into()
                .context("remote spawn: reply was not a pid")?,
        );
        Ok(Pid::from_raw(raw))
    }

    /// List the pids currently alive on a connected peer — the cluster primitive
    /// behind **live attach** (point at a running node and see its processes).
    /// Errors if we are not connected to `to_node`.
    pub async fn remote_pids(&self, to_node: &str) -> Result<Vec<Pid>> {
        let reply = self.request(to_node, "list", Vec::new()).await?;
        if reply.len() % 8 != 0 {
            return Err(anyhow!("remote list: reply was not a pid array"));
        }
        Ok(reply
            .chunks_exact(8)
            .map(|c| Pid::from_raw(u64::from_le_bytes(c.try_into().unwrap())))
            .collect())
    }

    /// The names of peer nodes this node currently has a live connection to.
    pub fn peers(&self) -> Vec<String> {
        self.inner.peers.read().unwrap().keys().cloned().collect()
    }

    /// Tear the node down: stop accepting and close every connection. The accept
    /// loop and per-peer loops then end (their `accept`/`recv` calls return), so the
    /// node's background tasks drain and its `Arc` state can be reclaimed once any
    /// processes holding it exit. Idempotent.
    pub fn shutdown(&self) {
        self.inner.endpoint.close(0u32.into(), b"node shutdown");
    }

    /// Issue a control-plane RPC to `to_node` and await its reply bytes.
    async fn request(&self, to_node: &str, op: &str, args: Vec<u8>) -> Result<Vec<u8>> {
        let req = self.inner.next_req.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(req, tx);

        let request = Control::Request {
            req,
            op: op.to_string(),
            args,
        };
        if let Err(err) = self.send_control(to_node, &request).await {
            self.inner.pending.lock().unwrap().remove(&req);
            return Err(err);
        }

        rx.await
            .context("control request: peer dropped before replying")?
            .map_err(|err| anyhow!(err))
    }

    /// Serve a peer's control-plane RPC. Sync and quick: a factory only *starts* a
    /// process (its body runs on the scheduler), and `list` is a registry read.
    fn handle_request(&self, op: &str, args: &[u8]) -> Result<Vec<u8>, String> {
        match op {
            "spawn" => {
                let (factory, args) = parse_message(args).ok_or("malformed spawn request")?;
                let spawnable = self
                    .inner
                    .spawnables
                    .read()
                    .unwrap()
                    .get(factory)
                    .cloned()
                    .ok_or_else(|| {
                        format!(
                            "node {:?} has no spawnable named {factory:?}",
                            self.inner.name
                        )
                    })?;
                Ok(spawnable(&self.inner.rt, args.to_vec())
                    .raw()
                    .to_le_bytes()
                    .to_vec())
            }
            "list" => {
                let mut out = Vec::new();
                for pid in self.inner.rt.list() {
                    out.extend_from_slice(&pid.raw().to_le_bytes());
                }
                Ok(out)
            }
            other => Err(format!("unknown control op {other:?}")),
        }
    }

    async fn accept_loop(self) {
        while let Some(incoming) = self.inner.endpoint.accept().await {
            let node = self.clone();
            tokio::spawn(async move {
                let err = match incoming.await {
                    Ok(conn) => match node.handshake_as_acceptor(&conn).await {
                        Ok((peer, send, recv)) => {
                            node.serve_peer(peer, conn, send, recv);
                            return;
                        }
                        Err(err) => err,
                    },
                    Err(err) => err.into(),
                };
                tracing::warn!(%err, "cluster: incoming peer failed");
            });
        }
    }

    /// The dialer opens the control stream, announces itself, then reads the
    /// acceptor's name. A dedicated bidirectional stream makes the handshake
    /// unambiguous and independent of how data streams happen to interleave.
    async fn handshake_as_dialer(
        &self,
        conn: &Connection,
    ) -> Result<(String, SendStream, RecvStream)> {
        let (mut send, mut recv) = conn.open_bi().await.context("opening control stream")?;
        write_frame(&mut send, self.inner.name.as_bytes())
            .await
            .context("announcing node name")?;
        let peer = read_node_name(&mut recv).await?;
        Ok((peer, send, recv))
    }

    /// The acceptor reads the dialer's name off the control stream, then announces
    /// its own — the mirror of [`handshake_as_dialer`](Self::handshake_as_dialer).
    async fn handshake_as_acceptor(
        &self,
        conn: &Connection,
    ) -> Result<(String, SendStream, RecvStream)> {
        let (mut send, mut recv) = conn.accept_bi().await.context("accepting control stream")?;
        let peer = read_node_name(&mut recv).await?;
        write_frame(&mut send, self.inner.name.as_bytes())
            .await
            .context("announcing node name")?;
        Ok((peer, send, recv))
    }

    /// Record a connected peer, start routing its messages, read its gossip, and
    /// tell it which global names we own.
    fn serve_peer(&self, peer: String, conn: Connection, send: SendStream, recv: RecvStream) {
        let control = Arc::new(AsyncMutex::new(send));
        self.inner.peers.write().unwrap().insert(
            peer.clone(),
            Peer {
                conn: conn.clone(),
                control: control.clone(),
            },
        );

        let node = self.clone();
        tokio::spawn(async move { node.delivery_loop(conn).await });

        let node = self.clone();
        tokio::spawn(async move { node.control_loop(peer, recv).await });

        let node = self.clone();
        tokio::spawn(async move { node.bootstrap_globals(control).await });
    }

    /// Read messages off `conn`'s uni-streams and route each into the local
    /// registry. Each message is its own stream — independent, no head-of-line
    /// blocking between them.
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

    /// Apply a peer's global-registry gossip until its control stream closes; then
    /// drop the peer and prune the global names it owned.
    async fn control_loop(self, peer: String, mut recv: RecvStream) {
        while let Ok(buf) = read_frame(&mut recv, MAX_CONTROL_FRAME).await {
            match serde_json::from_slice::<Control>(&buf) {
                Ok(Control::Register { name, node }) => {
                    self.inner.globals.write().unwrap().insert(name, node);
                }
                Ok(Control::Unregister { name }) => {
                    self.inner.globals.write().unwrap().remove(&name);
                }
                Ok(Control::Request { req, op, args }) => {
                    // Handle off the control loop so a slow op or a back-pressured
                    // reply never stalls this peer's gossip.
                    let node = self.clone();
                    let peer = peer.clone();
                    tokio::spawn(async move {
                        let result = node.handle_request(&op, &args);
                        let reply = Control::Reply { req, result };
                        let _ = node.send_control(&peer, &reply).await;
                    });
                }
                Ok(Control::Reply { req, result }) => {
                    if let Some(tx) = self.inner.pending.lock().unwrap().remove(&req) {
                        let _ = tx.send(result);
                    }
                }
                Err(err) => tracing::warn!(%err, "cluster: malformed control frame"),
            }
        }
        self.inner.peers.write().unwrap().remove(&peer);
        self.inner
            .globals
            .write()
            .unwrap()
            .retain(|_, owner| owner != &peer);
        tracing::debug!(%peer, "cluster: peer disconnected");
    }

    /// Tell a freshly-connected peer about every global name we own.
    async fn bootstrap_globals(self, control: Arc<AsyncMutex<SendStream>>) {
        let mine: Vec<Control> = {
            let globals = self.inner.globals.read().unwrap();
            globals
                .iter()
                .filter(|(_, owner)| *owner == &self.inner.name)
                .map(|(name, node)| Control::Register {
                    name: name.clone(),
                    node: node.clone(),
                })
                .collect()
        };
        let mut send = control.lock().await;
        for control in &mine {
            if let Ok(json) = serde_json::to_vec(control) {
                let _ = write_frame(&mut send, &json).await;
            }
        }
    }

    /// Push one control frame to every connected peer.
    async fn broadcast(&self, control: &Control) {
        let Ok(json) = serde_json::to_vec(control) else {
            return;
        };
        let senders: Vec<_> = self
            .inner
            .peers
            .read()
            .unwrap()
            .values()
            .map(|p| p.control.clone())
            .collect();
        for sender in senders {
            let mut send = sender.lock().await;
            let _ = write_frame(&mut send, &json).await;
        }
    }

    /// Send one control frame to a single named peer.
    async fn send_control(&self, to_node: &str, control: &Control) -> Result<()> {
        let sender = self
            .inner
            .peers
            .read()
            .unwrap()
            .get(to_node)
            .map(|p| p.control.clone())
            .ok_or_else(|| anyhow!("no connection to node {to_node:?}"))?;
        let json = serde_json::to_vec(control).context("encoding control frame")?;
        let mut send = sender.lock().await;
        write_frame(&mut send, &json).await
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

/// Read a length-prefixed node name off the control stream.
async fn read_node_name(recv: &mut RecvStream) -> Result<String> {
    let bytes = read_frame(recv, MAX_NODE_NAME)
        .await
        .context("reading peer node name")?;
    String::from_utf8(bytes).context("peer node name was not valid UTF-8")
}

/// Upper bound on a node name (a short label).
const MAX_NODE_NAME: usize = 1 << 10;

/// Write a `[len: u32 LE][payload]` frame to a stream.
async fn write_frame(send: &mut SendStream, payload: &[u8]) -> Result<()> {
    send.write_all(&(payload.len() as u32).to_le_bytes())
        .await
        .context("writing frame length")?;
    send.write_all(payload)
        .await
        .context("writing frame body")?;
    Ok(())
}

/// Read one `[len: u32 LE][payload]` frame, rejecting frames larger than `max`.
async fn read_frame(recv: &mut RecvStream, max: usize) -> Result<Vec<u8>> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len)
        .await
        .context("reading frame length")?;
    let n = u32::from_le_bytes(len) as usize;
    if n > max {
        return Err(anyhow!("frame of {n} bytes exceeds {max}-byte limit"));
    }
    let mut buf = vec![0u8; n];
    recv.read_exact(&mut buf)
        .await
        .context("reading frame body")?;
    Ok(buf)
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

    /// Spawn a process that forwards its first message over a oneshot, so a test
    /// can await cross-node delivery. Not registered — the caller chooses local
    /// ([`Runtime::register`]) or cluster-wide ([`ClusterNode::register_global`]).
    fn spawn_inbox(rt: &Runtime) -> (Pid, oneshot::Receiver<Vec<u8>>) {
        let (tx, rx) = oneshot::channel();
        let handle = rt.spawn(|mut ctx| async move {
            let msg = ctx.recv().await.message().unwrap();
            let _ = tx.send(msg);
        });
        (handle.pid(), rx)
    }

    /// `spawn_inbox` plus a local registration under `name`.
    fn inbox(rt: &Runtime, name: &str) -> (Pid, oneshot::Receiver<Vec<u8>>) {
        let (pid, rx) = spawn_inbox(rt);
        assert!(rt.register(name, pid));
        (pid, rx)
    }

    /// Await a cross-node delivery with a generous ceiling (loopback is instant;
    /// the timeout only guards against a hang).
    async fn recv(rx: oneshot::Receiver<Vec<u8>>) -> Vec<u8> {
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .expect("delivery timed out")
            .unwrap()
    }

    /// Poll until `node` reports `cond`. Handshake/gossip settle in well under a
    /// millisecond on loopback; this only avoids a race.
    async fn eventually(mut cond: impl FnMut() -> bool) {
        for _ in 0..500 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("condition never became true");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_node_messages_a_process_on_another_node() {
        let id = Identity::generate().unwrap();

        let rt_b = Runtime::new();
        let (_, rx) = inbox(&rt_b, "inbox");
        let node_b = ClusterNode::bind("B", rt_b, localhost(), &id).unwrap();
        let addr_b = node_b.local_addr().unwrap();

        let node_a = ClusterNode::bind("A", Runtime::new(), localhost(), &id).unwrap();
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
        let (_, rx) = inbox(&rt_b, "worker");
        let node_b = ClusterNode::bind("beta", rt_b, localhost(), &id).unwrap();
        let addr_b = node_b.local_addr().unwrap();

        let node_a = ClusterNode::bind("alpha", Runtime::new(), localhost(), &id).unwrap();
        node_a.connect(addr_b).await.unwrap();

        node_a.send("beta", "worker", b"by name").await.unwrap();
        assert_eq!(recv(rx).await, b"by name");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_single_link_carries_messages_both_ways() {
        let id = Identity::generate().unwrap();

        let rt_a = Runtime::new();
        let (_, rx_a) = inbox(&rt_a, "a-inbox");
        let node_a = ClusterNode::bind("A", rt_a, localhost(), &id).unwrap();

        let rt_b = Runtime::new();
        let (_, rx_b) = inbox(&rt_b, "b-inbox");
        let node_b = ClusterNode::bind("B", rt_b, localhost(), &id).unwrap();

        node_a.connect(node_b.local_addr().unwrap()).await.unwrap();
        node_a.send("B", "b-inbox", b"a->b").await.unwrap();

        eventually(|| node_b.peers() == vec!["A".to_string()]).await;
        node_b.send("A", "a-inbox", b"b->a").await.unwrap();

        assert_eq!(recv(rx_b).await, b"a->b");
        assert_eq!(recv(rx_a).await, b"b->a");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_global_name_registered_before_connect_is_gossiped_on_handshake() {
        let id = Identity::generate().unwrap();

        let rt_b = Runtime::new();
        let (pid, rx) = spawn_inbox(&rt_b);
        let node_b = ClusterNode::bind("B", rt_b, localhost(), &id).unwrap();
        assert!(node_b.register_global("svc", pid));

        let node_a = ClusterNode::bind("A", Runtime::new(), localhost(), &id).unwrap();
        node_a.connect(node_b.local_addr().unwrap()).await.unwrap();

        // A learns where "svc" lives from B's bootstrap, then reaches it by name.
        eventually(|| node_a.whereis_global("svc").as_deref() == Some("B")).await;
        node_a.send_global("svc", b"global hello").await.unwrap();
        assert_eq!(recv(rx).await, b"global hello");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_global_name_registered_after_connect_is_broadcast() {
        let id = Identity::generate().unwrap();

        let rt_b = Runtime::new();
        let (pid, rx) = spawn_inbox(&rt_b);
        let node_b = ClusterNode::bind("B", rt_b, localhost(), &id).unwrap();
        let addr_b = node_b.local_addr().unwrap();

        let node_a = ClusterNode::bind("A", Runtime::new(), localhost(), &id).unwrap();
        node_a.connect(addr_b).await.unwrap();

        // Register only after the link is up — A must hear about it via broadcast.
        assert!(node_b.register_global("late", pid));
        eventually(|| node_a.whereis_global("late").is_some()).await;
        node_a.send_global("late", b"after connect").await.unwrap();
        assert_eq!(recv(rx).await, b"after connect");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_global_delivers_to_a_locally_owned_name() {
        let node = ClusterNode::bind(
            "solo",
            Runtime::new(),
            localhost(),
            &Identity::generate().unwrap(),
        )
        .unwrap();
        let (pid, rx) = spawn_inbox(node.runtime());
        assert!(node.register_global("here", pid));

        node.send_global("here", b"local path").await.unwrap();
        assert_eq!(recv(rx).await, b"local path");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sending_to_an_unknown_node_or_global_errors() {
        let node = ClusterNode::bind(
            "solo",
            Runtime::new(),
            localhost(),
            &Identity::generate().unwrap(),
        )
        .unwrap();
        assert!(node
            .send("ghost", "inbox", b"x")
            .await
            .unwrap_err()
            .to_string()
            .contains("ghost"));
        assert!(node
            .send_global("nowhere", b"x")
            .await
            .unwrap_err()
            .to_string()
            .contains("nowhere"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_node_spawns_a_process_on_a_remote_node() {
        let id = Identity::generate().unwrap();

        let rt_b = Runtime::new();
        let node_b = ClusterNode::bind("B", rt_b, localhost(), &id).unwrap();
        // B is taught one factory; invoking it records the args and spawns a process.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        node_b.register_spawnable("recorder", move |rt, args| {
            tx.send(args).unwrap();
            rt.spawn(|mut ctx| async move {
                let _ = ctx.recv().await;
            })
            .pid()
        });

        let node_a = ClusterNode::bind("A", Runtime::new(), localhost(), &id).unwrap();
        node_a.connect(node_b.local_addr().unwrap()).await.unwrap();

        let pid = node_a
            .spawn_remote("B", "recorder", b"build me".to_vec())
            .await
            .unwrap();
        assert!(node_b.runtime().is_alive(pid));

        let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("factory never ran")
            .unwrap();
        assert_eq!(got, b"build me");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_attach_lists_a_remote_nodes_processes() {
        let id = Identity::generate().unwrap();

        // B is running three processes when A attaches.
        let rt_b = Runtime::new();
        for _ in 0..3 {
            spawn_inbox(&rt_b);
        }
        let node_b = ClusterNode::bind("B", rt_b, localhost(), &id).unwrap();

        let node_a = ClusterNode::bind("A", Runtime::new(), localhost(), &id).unwrap();
        node_a.connect(node_b.local_addr().unwrap()).await.unwrap();

        let remote_pids = node_a.remote_pids("B").await.unwrap();
        assert_eq!(remote_pids.len(), 3);
        // The pids A sees are real handles on B.
        assert!(remote_pids
            .iter()
            .all(|&pid| node_b.runtime().is_alive(pid)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remote_spawn_of_an_unknown_factory_errors() {
        let id = Identity::generate().unwrap();
        let node_b = ClusterNode::bind("B", Runtime::new(), localhost(), &id).unwrap();
        let node_a = ClusterNode::bind("A", Runtime::new(), localhost(), &id).unwrap();
        node_a.connect(node_b.local_addr().unwrap()).await.unwrap();

        let err = node_a
            .spawn_remote("B", "missing", vec![])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_wrong_certificate_is_rejected() {
        let node_b = ClusterNode::bind(
            "B",
            Runtime::new(),
            localhost(),
            &Identity::generate().unwrap(),
        )
        .unwrap();
        let addr_b = node_b.local_addr().unwrap();

        // A different identity → neither side's mutual-TLS trust anchor matches.
        let node_a = ClusterNode::bind(
            "A",
            Runtime::new(),
            localhost(),
            &Identity::generate().unwrap(),
        )
        .unwrap();
        assert!(node_a.connect(addr_b).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nodes_issued_by_one_ca_form_a_cluster() {
        let ca = ClusterCa::generate().unwrap();

        let rt_b = Runtime::new();
        let (_, rx) = inbox(&rt_b, "inbox");
        let node_b = ClusterNode::bind("B", rt_b, localhost(), &ca.issue("B").unwrap()).unwrap();
        let addr_b = node_b.local_addr().unwrap();

        // A's per-node cert is signed by the same CA → mutual TLS succeeds.
        let node_a =
            ClusterNode::bind("A", Runtime::new(), localhost(), &ca.issue("A").unwrap()).unwrap();
        let remote = node_a.connect(addr_b).await.unwrap();
        remote.send("inbox", b"ca-signed hello").await.unwrap();
        assert_eq!(recv(rx).await, b"ca-signed hello");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_node_from_a_foreign_ca_is_rejected() {
        let ca = ClusterCa::generate().unwrap();
        let foreign = ClusterCa::generate().unwrap();

        let node_b =
            ClusterNode::bind("B", Runtime::new(), localhost(), &ca.issue("B").unwrap()).unwrap();
        let addr_b = node_b.local_addr().unwrap();

        // An intruder issued by a *different* CA: neither side trusts the other.
        let node_a = ClusterNode::bind(
            "A",
            Runtime::new(),
            localhost(),
            &foreign.issue("A").unwrap(),
        )
        .unwrap();
        assert!(node_a.connect(addr_b).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_drops_the_peer_link() {
        let id = Identity::generate().unwrap();
        let node_b = ClusterNode::bind("B", Runtime::new(), localhost(), &id).unwrap();
        let node_a = ClusterNode::bind("A", Runtime::new(), localhost(), &id).unwrap();
        node_a.connect(node_b.local_addr().unwrap()).await.unwrap();
        eventually(|| node_a.peers() == vec!["B".to_string()]).await;

        // Closing B's endpoint tears the link down; A's send then fails.
        node_b.shutdown();
        for _ in 0..500 {
            if node_a.send("B", "x", b"y").await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("link to a shut-down node never failed");
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
