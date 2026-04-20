//! Session layer — handshake, keepalive, reconnect, notification
//! streams.
//!
//! The pieces below the [`Session`] type (`transport::CanBackend`,
//! `protocol::IsoTp*`, `protocol::Response`) are deliberately low-level:
//! one call per frame, no retry logic, no understanding of
//! "connected" vs "not connected". Real subcommands don't want to
//! drive that machinery directly — they'd reinvent the same
//! boilerplate (segment → send → reassemble → parse → check NACK →
//! refresh keepalive) every time. This module collapses all of that
//! into one type.
//!
//! ## Concurrency model
//!
//! ```text
//! ┌──────────────┐     backend.recv()     ┌──────────────┐
//! │   rx_task    │ ◄──────────────────── │   backend    │
//! │ (background) │                        └──────┬───────┘
//! │              │                               │ backend.send()
//! │   routes:    │                               ▲
//! │   ACK/NACK/  │──► reply_tx (mpsc) ──┐       │
//! │   DISCOVER   │                       │       │
//! │   NOTIFY ───►│ notification_tx       │       │
//! │              │    (broadcast)        │       │
//! └──────────────┘                       ▼       │
//!                                ┌──────────────┐│
//!                                │ send_command │├─ command_lock
//!                                │ broadcast()  │◄ serialises
//!                                │ connect()    ││ all ops
//!                                └──────────────┘│
//! ```
//!
//! - Exactly one task calls `backend.recv()`: the RX task. Everyone
//!   else consumes pre-decoded [`Response`]s from the reply channel
//!   (single-receiver mpsc — only the current command is listening)
//!   or the notification channel (broadcast — any number of
//!   subscribers).
//! - Exactly one command is in flight at a time. `command_lock`
//!   serialises `send_command`, `broadcast`, `connect` and
//!   `disconnect` so a concurrent "retry on BAD_SESSION" doesn't
//!   race with another op.
//!
//! ## What Session does NOT do
//!
//! - **Protocol encoding.** Callers build their own payload bytes via
//!   [`crate::protocol::commands`] and pass them in. Session just
//!   segments, sends, reassembles, and parses the reply — it has no
//!   opinion on what's in the payload.
//! - **Interpretation of Notify payloads.** The subscriber gets raw
//!   [`Response::Notify`]; decoding to a `NotifyOpcode` + struct is
//!   the subscriber's job (matches how the rest of the protocol
//!   module works).
//! - **Flash orchestration.** Sector-aware erase / diff / CRC
//!   verification live in `src/firmware/` (feat/12+).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc, oneshot, Mutex as TokioMutex};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::{debug, trace, warn};

use crate::protocol::commands::{
    cmd_connect, cmd_disconnect, cmd_get_health, PROTOCOL_VERSION_MAJOR, PROTOCOL_VERSION_MINOR,
};
use crate::protocol::ids::{FrameId, MessageType};
use crate::protocol::isotp::{IsoTpSegmenter, ReassembleOutcome, Reassembler};
use crate::protocol::opcodes::{CommandOpcode, NackCode};
use crate::protocol::{CanFrame, Response, BROADCAST_NODE_ID};
use crate::transport::{CanBackend, TransportError};

/// Everything this layer can fail with. Wraps the lower-level
/// `TransportError` / `ParseError` variants and adds session-specific
/// conditions (NACK, protocol-version mismatch, not-connected, …).
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Transport(#[from] TransportError),

    #[error(transparent)]
    Parse(#[from] crate::protocol::ParseError),

    /// Device NACK'd the command. `rejected_opcode` is the opcode
    /// the device says it rejected (`0xFF` when the device didn't
    /// successfully identify what we sent).
    #[error("device NACK'd opcode 0x{rejected_opcode:02X} with code {code}")]
    Nack { rejected_opcode: u8, code: NackCode },

    /// Peer answered CONNECT with `NACK(PROTOCOL_VERSION)` or a
    /// version number we don't support.
    #[error("protocol version mismatch: host {host_major}.{host_minor}, device {device_major}.{device_minor}")]
    ProtocolVersionMismatch {
        host_major: u8,
        host_minor: u8,
        device_major: u8,
        device_minor: u8,
    },

    /// No reply arrived within the configured command timeout.
    #[error("timed out waiting for device reply after {}ms", .0.as_millis())]
    CommandTimeout(Duration),

    /// Session operation attempted without a prior successful
    /// `connect()`.
    #[error("session-gated operation attempted before connect()")]
    NotConnected,

    /// RX task has exited — the backend is closed or the session has
    /// been dropped.
    #[error("session RX task exited — backend may have disconnected")]
    RxClosed,

    /// Shouldn't reach this path; RX / keepalive task panicked.
    #[error("session task panic: {0}")]
    TaskPanic(String),

    /// Received a `CMD` frame at the host, a duplicate FF mid-
    /// reassembly, or anything else the protocol layer classifies as
    /// "garbled bus traffic".
    #[error("unexpected protocol frame: {0}")]
    Protocol(&'static str),
}

/// Per-session knobs. [`SessionConfig::default`] gives you numbers
/// that match REQUIREMENTS.md — `target_node: 0x3`, 5 s keepalive,
/// 500 ms command timeout.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub target_node: u8,
    pub keepalive_interval: Duration,
    pub command_timeout: Duration,
    pub host_major: u8,
    pub host_minor: u8,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            target_node: 0x3,
            keepalive_interval: Duration::from_millis(5_000),
            command_timeout: Duration::from_millis(500),
            host_major: PROTOCOL_VERSION_MAJOR,
            host_minor: PROTOCOL_VERSION_MINOR,
        }
    }
}

/// The handle a caller holds. Internally shares state with the RX
/// task + optional keepalive task via `Arc<SessionInner>`.
pub struct Session {
    inner: Arc<SessionInner>,
    rx_handle: TokioMutex<Option<JoinHandle<()>>>,
    rx_shutdown: Arc<AtomicBool>,
    keepalive: TokioMutex<Option<KeepaliveState>>,
    config: SessionConfig,
    /// Latched at the end of a successful `connect()`. Cleared by
    /// `disconnect()`. `send_session_gated` checks it before
    /// attempting a command.
    connected: AtomicBool,
}

struct SessionInner {
    backend: Arc<dyn CanBackend>,
    target_node: u8,
    /// Single-receiver channel for ACK / NACK / DISCOVER replies.
    /// The RX task holds the sender; the current command holder
    /// (whoever acquired `command_lock`) owns the receiver via a
    /// Mutex.
    reply_rx: TokioMutex<mpsc::Receiver<Response>>,
    /// Broadcast channel for unsolicited NOTIFYs (HEARTBEAT, DTC,
    /// LOG, LIVE_DATA). Subscribers call `session.subscribe_notifications()`.
    notification_tx: broadcast::Sender<Response>,
    /// Serialises send_command / broadcast / connect / disconnect so
    /// only one command is ever in flight. The `u8` is unused;
    /// exists to make the Mutex non-ZST (easier to print / debug).
    command_lock: TokioMutex<()>,
}

struct KeepaliveState {
    cancel_tx: oneshot::Sender<()>,
    handle: JoinHandle<()>,
}

impl Session {
    /// Attach a session over an already-opened backend. The RX task
    /// starts immediately; call [`Session::connect`] before issuing
    /// any session-gated command.
    ///
    /// Session-less commands (DISCOVER, GET_FW_INFO, GET_HEALTH,
    /// OB_READ, DTC_READ, RESET, JUMP) work without `connect()` —
    /// just `send_command`.
    pub fn attach(backend: Box<dyn CanBackend>, config: SessionConfig) -> Self {
        let backend_arc: Arc<dyn CanBackend> = Arc::from(backend);
        let (reply_tx, reply_rx) = mpsc::channel::<Response>(32);
        // 64-deep broadcast is plenty: the only reason a subscriber
        // falls behind is they're not consuming. Dropped messages
        // surface as `RecvError::Lagged`.
        let (notification_tx, _) = broadcast::channel::<Response>(64);

        let inner = Arc::new(SessionInner {
            backend: Arc::clone(&backend_arc),
            target_node: config.target_node,
            reply_rx: TokioMutex::new(reply_rx),
            notification_tx: notification_tx.clone(),
            command_lock: TokioMutex::new(()),
        });

        let shutdown = Arc::new(AtomicBool::new(false));
        let rx_handle = tokio::spawn(rx_task(
            backend_arc,
            reply_tx,
            notification_tx,
            config.target_node,
            Arc::clone(&shutdown),
        ));

        Self {
            inner,
            rx_handle: TokioMutex::new(Some(rx_handle)),
            rx_shutdown: shutdown,
            keepalive: TokioMutex::new(None),
            config,
            connected: AtomicBool::new(false),
        }
    }

    /// True after a successful `connect()` and until `disconnect()`
    /// (or the watchdog / bus drops us).
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Perform the `CMD_CONNECT` handshake. Sends `[major, minor]`,
    /// waits for an ACK carrying the device's advertised version,
    /// validates majors match, starts the keepalive task.
    pub async fn connect(&self) -> Result<(u8, u8), SessionError> {
        let _guard = self.inner.command_lock.lock().await;
        let payload = cmd_connect(self.config.host_major, self.config.host_minor);
        let response = self.send_raw(&payload, MessageType::Cmd).await?;

        match response {
            Response::Ack { opcode, payload } => {
                if opcode != CommandOpcode::Connect.as_byte() {
                    return Err(SessionError::Protocol(
                        "CONNECT ACK has unexpected opcode byte",
                    ));
                }
                if payload.len() < 2 {
                    return Err(SessionError::Protocol(
                        "CONNECT ACK payload shorter than [major, minor]",
                    ));
                }
                let device_major = payload[0];
                let device_minor = payload[1];
                if device_major != self.config.host_major {
                    return Err(SessionError::ProtocolVersionMismatch {
                        host_major: self.config.host_major,
                        host_minor: self.config.host_minor,
                        device_major,
                        device_minor,
                    });
                }

                self.connected.store(true, Ordering::SeqCst);
                self.start_keepalive().await;
                debug!(device_major, device_minor, "session connected");
                Ok((device_major, device_minor))
            }
            Response::Nack {
                rejected_opcode,
                code,
            } => {
                if code == NackCode::ProtocolVersion {
                    // We don't know the device's version for logging
                    // here; return a best-effort mismatch error.
                    Err(SessionError::ProtocolVersionMismatch {
                        host_major: self.config.host_major,
                        host_minor: self.config.host_minor,
                        device_major: 0,
                        device_minor: 0,
                    })
                } else {
                    Err(SessionError::Nack {
                        rejected_opcode,
                        code,
                    })
                }
            }
            other => Err(SessionError::Protocol(match other {
                Response::Notify { .. } => "unexpected NOTIFY during CONNECT",
                Response::Discover { .. } => "unexpected DISCOVER during CONNECT",
                _ => "unexpected response during CONNECT",
            })),
        }
    }

    /// Send a single command and wait for its ACK / NACK.
    /// ISO-TP-framed on both sides. `message_type` is almost always
    /// `MessageType::Cmd`; DISCOVER callers use [`Session::broadcast`]
    /// instead.
    pub async fn send_command(&self, payload: &[u8]) -> Result<Response, SessionError> {
        let _guard = self.inner.command_lock.lock().await;
        self.send_raw(payload, MessageType::Cmd).await
    }

    /// Like [`Session::send_command`] but sends to `dst` instead of
    /// the session's configured `target_node`. Used by `discover`:
    /// after a broadcast collects replies from multiple nodes, each
    /// responder gets follow-up `GET_FW_INFO` / `GET_HEALTH` pings
    /// routed individually without reattaching the session.
    ///
    /// The broader session state (keepalive, reconnect-on-BAD_SESSION,
    /// notification routing) is unaffected — this is purely a
    /// per-call destination override.
    pub async fn send_command_to(&self, dst: u8, payload: &[u8]) -> Result<Response, SessionError> {
        let _guard = self.inner.command_lock.lock().await;
        self.send_frames(payload, MessageType::Cmd, dst).await?;
        let mut rx = self.inner.reply_rx.lock().await;
        match timeout(self.config.command_timeout, rx.recv()).await {
            Ok(Some(response)) => Ok(response),
            Ok(None) => Err(SessionError::RxClosed),
            Err(_) => Err(SessionError::CommandTimeout(self.config.command_timeout)),
        }
    }

    /// Like [`Session::send_command`], but for session-gated opcodes:
    /// on `NACK(BAD_SESSION)`, reconnect and retry the command once.
    /// If the retry still fails (or the reconnect fails) the error
    /// bubbles up.
    pub async fn send_session_gated(&self, payload: &[u8]) -> Result<Response, SessionError> {
        // Grab the command lock for the whole "try → reconnect → retry"
        // sequence so another caller can't interleave mid-reconnect.
        let _guard = self.inner.command_lock.lock().await;
        let first = self.send_raw(payload, MessageType::Cmd).await?;
        match &first {
            Response::Nack { code, .. } if *code == NackCode::BadSession => {
                debug!("received BAD_SESSION — reconnecting + retrying");
                self.connected.store(false, Ordering::SeqCst);
                self.stop_keepalive_locked().await;
                // Inline CONNECT — we already hold command_lock.
                let connect_payload = cmd_connect(self.config.host_major, self.config.host_minor);
                let reply = self.send_raw(&connect_payload, MessageType::Cmd).await?;
                match reply {
                    Response::Ack {
                        opcode,
                        payload: ack_payload,
                    } if opcode == CommandOpcode::Connect.as_byte()
                        && ack_payload.len() >= 2
                        && ack_payload[0] == self.config.host_major =>
                    {
                        self.connected.store(true, Ordering::SeqCst);
                        self.start_keepalive_locked().await;
                    }
                    _ => {
                        return Err(SessionError::Protocol(
                            "reconnect after BAD_SESSION failed — peer didn't accept CONNECT",
                        ));
                    }
                }
                self.send_raw(payload, MessageType::Cmd).await
            }
            _ => Ok(first),
        }
    }

    /// Broadcast a command (dst = `BROADCAST_NODE_ID`) and collect
    /// every reply that arrives within `collect_for`. Intended for
    /// `CMD_DISCOVER`; the usual reply is `Response::Discover`, but
    /// any reply the device emits in the window lands in the
    /// returned vec so callers can see stray ACKs / NACKs too.
    pub async fn broadcast(
        &self,
        payload: &[u8],
        message_type: MessageType,
        collect_for: Duration,
    ) -> Result<Vec<Response>, SessionError> {
        let _guard = self.inner.command_lock.lock().await;
        self.send_frames(payload, message_type, BROADCAST_NODE_ID)
            .await?;

        let deadline = Instant::now() + collect_for;
        let mut collected = Vec::new();
        let mut rx = self.inner.reply_rx.lock().await;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match timeout(remaining, rx.recv()).await {
                Ok(Some(resp)) => collected.push(resp),
                Ok(None) => return Err(SessionError::RxClosed),
                Err(_elapsed) => break,
            }
        }
        Ok(collected)
    }

    /// Subscribe to the notification stream (`NOTIFY_HEARTBEAT`,
    /// `NOTIFY_DTC`, `NOTIFY_LOG`, `NOTIFY_LIVE_DATA`). Every active
    /// subscriber receives every notification; a slow subscriber
    /// receives `RecvError::Lagged` and can resync by calling
    /// `subscribe_notifications` again.
    pub fn subscribe_notifications(&self) -> broadcast::Receiver<Response> {
        self.inner.notification_tx.subscribe()
    }

    /// Send `CMD_DISCONNECT`, stop keepalive, tear down the RX task.
    /// Idempotent once called; after this the `Session` is unusable.
    ///
    /// Fire-and-forget on the wire: we don't wait for the device's ACK,
    /// because every single caller of `disconnect()` already discards
    /// whatever reply would come back. Waiting would cost a full
    /// `command_timeout` in the one case that matters — when the peer
    /// just jumped to the application via `CMD_JUMP` and the BL is
    /// no longer on the bus to ACK us. Firing CMD_DISCONNECT and
    /// tearing down locally without blocking matches the existing
    /// `let _ = …` pattern and makes the post-jump path finish in
    /// milliseconds instead of `command_timeout` seconds.
    pub async fn disconnect(self) -> Result<(), SessionError> {
        // Best-effort: acquire the command lock to play nicely with
        // concurrent send_command, but don't deadlock if we can't.
        let _guard = self.inner.command_lock.lock().await;
        if self.connected.load(Ordering::SeqCst) {
            let payload = cmd_disconnect();
            // Send the frame but don't wait for a reply. If the BL is
            // still alive it sees CMD_DISCONNECT and clears its session
            // latch; if the BL has jumped to the application the
            // frame's lost in the ether and that's fine. Any ACK that
            // does come back lands in the reply mpsc and is dropped
            // when we drop `self` below.
            let _ = self
                .send_frames(&payload, MessageType::Cmd, self.inner.target_node)
                .await;
            self.connected.store(false, Ordering::SeqCst);
        }
        self.stop_keepalive_locked().await;
        self.rx_shutdown.store(true, Ordering::SeqCst);
        if let Some(handle) = self.rx_handle.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }
        Ok(())
    }

    // ---- Internals ----

    /// Segment `payload` and transmit, then await a single reply
    /// within the configured command timeout. Caller must hold
    /// `command_lock`.
    async fn send_raw(
        &self,
        payload: &[u8],
        message_type: MessageType,
    ) -> Result<Response, SessionError> {
        self.send_frames(payload, message_type, self.inner.target_node)
            .await?;
        let mut rx = self.inner.reply_rx.lock().await;
        match timeout(self.config.command_timeout, rx.recv()).await {
            Ok(Some(response)) => Ok(response),
            Ok(None) => Err(SessionError::RxClosed),
            Err(_) => Err(SessionError::CommandTimeout(self.config.command_timeout)),
        }
    }

    async fn send_frames(
        &self,
        payload: &[u8],
        message_type: MessageType,
        dst: u8,
    ) -> Result<(), SessionError> {
        // New wire format (fix/12): the ID no longer carries the
        // message type — it's prepended as the first byte of the
        // payload. Every frame (FF + CFs) shares the same
        // host→node ID; the PCI byte tells the receiver which frame
        // of the ISO-TP sequence it's looking at.
        let mut framed = Vec::with_capacity(1 + payload.len());
        framed.push(message_type.as_byte());
        framed.extend_from_slice(payload);

        let segmenter = IsoTpSegmenter::new(&framed).map_err(|e| {
            SessionError::Transport(TransportError::Other(format!(
                "session: payload rejected by segmenter: {e}"
            )))
        })?;
        let id = FrameId::from_host(dst)
            .expect("dst fits in 4 bits")
            .encode();

        for frame_bytes in segmenter {
            let frame = CanFrame {
                id,
                data: frame_bytes,
                len: frame_bytes.len() as u8,
            };
            self.inner.backend.send(frame).await?;
        }
        Ok(())
    }

    async fn start_keepalive(&self) {
        let mut guard = self.keepalive.lock().await;
        if guard.is_some() {
            return;
        }
        *guard = Some(self.spawn_keepalive_task());
    }

    async fn start_keepalive_locked(&self) {
        // Same body as start_keepalive but callable from contexts
        // that already hold command_lock (the reconnect path).
        let mut guard = self.keepalive.lock().await;
        if guard.is_some() {
            return;
        }
        *guard = Some(self.spawn_keepalive_task());
    }

    fn spawn_keepalive_task(&self) -> KeepaliveState {
        let inner = Arc::clone(&self.inner);
        let interval = self.config.keepalive_interval;
        let cmd_timeout = self.config.command_timeout;
        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = &mut cancel_rx => {
                        trace!("keepalive: cancel signalled");
                        return;
                    }
                    _ = tokio::time::sleep(interval) => {
                        if let Err(err) = keepalive_tick(&inner, cmd_timeout).await {
                            warn!(?err, "keepalive tick failed — stopping");
                            return;
                        }
                    }
                }
            }
        });
        KeepaliveState { cancel_tx, handle }
    }

    async fn stop_keepalive_locked(&self) {
        let taken = {
            let mut guard = self.keepalive.lock().await;
            guard.take()
        };
        if let Some(state) = taken {
            let _ = state.cancel_tx.send(());
            let _ = state.handle.await;
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.rx_shutdown.store(true, Ordering::SeqCst);
        // Best-effort abort of the RX task. Can't .await in Drop; the
        // RX task's shutdown check + AbortHandle combine to kill it
        // promptly on the tokio executor.
        if let Ok(mut guard) = self.rx_handle.try_lock() {
            if let Some(h) = guard.take() {
                h.abort();
            }
        }
        if let Ok(mut guard) = self.keepalive.try_lock() {
            if let Some(state) = guard.take() {
                let _ = state.cancel_tx.send(());
                state.handle.abort();
            }
        }
    }
}

/// Single keepalive tick: issue `CMD_GET_HEALTH`, drop the response.
/// Refreshes the bootloader's 30 s session watchdog. Runs under its
/// own borrow of `command_lock` so it serialises with user-driven
/// commands.
async fn keepalive_tick(
    inner: &Arc<SessionInner>,
    command_timeout: Duration,
) -> Result<(), SessionError> {
    let _guard = inner.command_lock.lock().await;
    let payload = cmd_get_health();

    // Mirrors `send_frames` — prepend msg_type, single ID for FF+CFs.
    let mut framed = Vec::with_capacity(1 + payload.len());
    framed.push(MessageType::Cmd.as_byte());
    framed.extend_from_slice(&payload);

    let segmenter = IsoTpSegmenter::new(&framed).map_err(|e| {
        SessionError::Transport(TransportError::Other(format!(
            "keepalive: payload rejected by segmenter: {e}"
        )))
    })?;
    let id = FrameId::from_host(inner.target_node)
        .expect("target_node fits in 4 bits")
        .encode();
    for bytes in segmenter {
        let frame = CanFrame {
            id,
            data: bytes,
            len: bytes.len() as u8,
        };
        inner.backend.send(frame).await?;
    }

    let mut rx = inner.reply_rx.lock().await;
    match timeout(command_timeout, rx.recv()).await {
        Ok(Some(_resp)) => Ok(()),
        Ok(None) => Err(SessionError::RxClosed),
        Err(_) => Err(SessionError::CommandTimeout(command_timeout)),
    }
}

/// Background RX task. Owns the only read path into the backend,
/// decodes ISO-TP frames, routes completed `Response`s onto the
/// appropriate channel.
async fn rx_task(
    backend: Arc<dyn CanBackend>,
    reply_tx: mpsc::Sender<Response>,
    notification_tx: broadcast::Sender<Response>,
    _node_id: u8,
    shutdown: Arc<AtomicBool>,
) {
    let mut reasm = Reassembler::new();
    let mut tick_start = Instant::now();
    let tick_ms = move || tick_start.elapsed().as_millis() as u64;
    // Re-borrow because closures that capture mutable state can't
    // return it. Keep as a cheap inline expression.
    let _ = &mut tick_start;

    loop {
        if shutdown.load(Ordering::SeqCst) {
            trace!("session rx: shutdown flag set, exiting");
            return;
        }

        let frame = match backend.recv(Duration::from_millis(50)).await {
            Ok(f) => f,
            Err(TransportError::Timeout(_)) => continue,
            Err(TransportError::Disconnected) => {
                trace!("session rx: backend disconnected");
                return;
            }
            Err(err) => {
                warn!(?err, "session rx: backend error");
                return;
            }
        };

        // Drop frames with an invalid or not-for-us ID. Under the
        // Proposal-A layout, the host only cares about NodeToHost
        // frames (direction bit set). Host-originated frames on the
        // bus (our own TX echo) and malformed IDs get silently
        // dropped here.
        let id = match FrameId::decode(frame.id) {
            Ok(id) => id,
            Err(_) => continue,
        };
        if !matches!(
            id.direction,
            crate::protocol::ids::FrameDirection::NodeToHost
        ) {
            // Either our own TX echo (HostToNode) or a malformed
            // frame; the reassembler should never see these.
            continue;
        }

        let payload = frame.payload();

        match reasm.feed(payload, tick_ms()) {
            Ok(ReassembleOutcome::Ongoing) => continue,
            Ok(ReassembleOutcome::Complete(bytes)) => {
                // Every reassembled SF/FF message starts with the
                // msg_type byte (fix/12 wire format). Decode it,
                // then hand the remaining bytes to the response
                // parser. An empty reassembly is a bug; log and drop.
                if bytes.is_empty() {
                    warn!("session rx: empty reassembly — dropping");
                    continue;
                }
                let msg_type = match MessageType::from_byte(bytes[0]) {
                    Ok(mt) => mt,
                    Err(err) => {
                        warn!(
                            ?err,
                            byte = bytes[0],
                            "session rx: unknown msg_type — dropping"
                        );
                        continue;
                    }
                };
                let inner_bytes = &bytes[1..];
                match Response::parse(msg_type, inner_bytes) {
                    Ok(Response::Notify { .. }) => {
                        let response = Response::parse(msg_type, inner_bytes).unwrap();
                        // Lagged subscribers get RecvError::Lagged on
                        // their next recv; we don't treat overflow as
                        // an error here.
                        let _ = notification_tx.send(response);
                    }
                    Ok(other) => {
                        if reply_tx.send(other).await.is_err() {
                            // No one is listening — command holder
                            // has dropped. Not fatal; keep running
                            // for notifications.
                        }
                    }
                    Err(err) => {
                        warn!(?err, "session rx: Response::parse failed; dropping frame");
                    }
                }
            }
            Err(err) => {
                warn!(?err, "session rx: reassembler error; resetting");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{StubDevice, VirtualBus};

    const STUB_NODE: u8 = 0x3;

    fn test_config() -> SessionConfig {
        SessionConfig {
            target_node: STUB_NODE,
            // Tight timings so tests run fast.
            keepalive_interval: Duration::from_millis(250),
            command_timeout: Duration::from_millis(200),
            host_major: PROTOCOL_VERSION_MAJOR,
            host_minor: PROTOCOL_VERSION_MINOR,
        }
    }

    async fn spawn_session_and_stub() -> (Session, oneshot::Sender<()>, JoinHandle<()>) {
        let bus = VirtualBus::new();
        let host = bus.host_backend();
        let device: Box<dyn CanBackend> = Box::new(bus.device_backend());
        drop(bus);

        let stub = StubDevice::new(device, STUB_NODE);
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            let _ = stub.run(cancel_rx).await;
        });
        let session = Session::attach(Box::new(host), test_config());
        (session, cancel_tx, handle)
    }

    #[tokio::test]
    async fn connect_succeeds_against_stub() {
        let (session, cancel, handle) = spawn_session_and_stub().await;
        let (major, minor) = session.connect().await.unwrap();
        assert_eq!(major, PROTOCOL_VERSION_MAJOR);
        assert_eq!(minor, PROTOCOL_VERSION_MINOR);
        assert!(session.is_connected());
        session.disconnect().await.unwrap();
        let _ = cancel.send(());
        let _ = handle.await;
    }

    #[tokio::test]
    async fn connect_with_bad_major_errors() {
        let (session, cancel, handle) = {
            let bus = VirtualBus::new();
            let host = bus.host_backend();
            let device: Box<dyn CanBackend> = Box::new(bus.device_backend());
            drop(bus);
            let stub = StubDevice::new(device, STUB_NODE);
            let (cancel_tx, cancel_rx) = oneshot::channel();
            let handle = tokio::spawn(async move {
                let _ = stub.run(cancel_rx).await;
            });
            // Override the host major to something the stub will NACK.
            let mut cfg = test_config();
            cfg.host_major = 99;
            let session = Session::attach(Box::new(host), cfg);
            (session, cancel_tx, handle)
        };

        let err = session.connect().await.unwrap_err();
        assert!(matches!(err, SessionError::ProtocolVersionMismatch { .. }));
        assert!(!session.is_connected());
        // Don't call disconnect() (session never connected); just drop.
        drop(session);
        let _ = cancel.send(());
        let _ = handle.await;
    }

    #[tokio::test]
    async fn send_command_to_unknown_opcode_returns_nack() {
        let (session, cancel, handle) = spawn_session_and_stub().await;
        session.connect().await.unwrap();
        // Opcode 0x20 isn't defined in `CommandOpcode` — the stub's
        // dispatch takes the `Err(_) → NACK(UNSUPPORTED)` branch.
        // (Every defined opcode has a stub handler now; testing the
        // "unknown opcode" fallthrough means reaching for a raw byte.)
        let payload = vec![0x20u8];
        let resp = session.send_command(&payload).await.unwrap();
        match resp {
            Response::Nack { code, .. } => assert_eq!(code, NackCode::Unsupported),
            other => panic!("expected Nack(Unsupported), got {other:?}"),
        }
        session.disconnect().await.unwrap();
        let _ = cancel.send(());
        let _ = handle.await;
    }

    #[tokio::test]
    async fn broadcast_discover_collects_single_reply() {
        let (session, cancel, handle) = spawn_session_and_stub().await;
        let payload = crate::protocol::commands::cmd_discover();
        let replies = session
            .broadcast(
                &payload,
                MessageType::DiscoverRequest,
                Duration::from_millis(150),
            )
            .await
            .unwrap();
        assert_eq!(replies.len(), 1, "one stub means one discover reply");
        match &replies[0] {
            Response::Discover { node_id, .. } => assert_eq!(*node_id, STUB_NODE),
            other => panic!("expected Discover, got {other:?}"),
        }
        let _ = cancel.send(());
        let _ = handle.await;
    }

    #[tokio::test]
    async fn command_times_out_when_no_peer() {
        // VirtualBus with both endpoints but no stub running.
        let bus = VirtualBus::new();
        let host = bus.host_backend();
        let _device = bus.device_backend(); // keep endpoint alive for channel
        drop(bus);

        let session = Session::attach(Box::new(host), test_config());
        let err = session.connect().await.unwrap_err();
        match err {
            SessionError::CommandTimeout(d) => assert_eq!(d, Duration::from_millis(200)),
            other => panic!("expected CommandTimeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn disconnect_is_idempotent_when_never_connected() {
        let (session, cancel, handle) = spawn_session_and_stub().await;
        session.disconnect().await.unwrap();
        let _ = cancel.send(());
        let _ = handle.await;
    }
}
