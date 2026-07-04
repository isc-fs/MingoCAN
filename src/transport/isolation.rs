//! Out-of-process CAN backend isolation.
//!
//! Some native driver libraries fault on their OWN threads in ways no
//! in-process Rust code can prevent or catch. The confirmed case: the
//! macOS MacCAN `libPCBUSB` driver SIGBUSes on its internal IOKit
//! run-loop thread when the PCAN adapter is unplugged — a crash that
//! takes down the whole MingoCAN app even though the fault is entirely
//! inside the third-party driver (see `pcan.rs` teardown fix + the
//! reliability audit).
//!
//! The fix is to run the crash-prone backend in a **separate helper
//! process** and bridge it over stdio:
//!
//! - The parent holds an [`IsolatedBackend`] (a [`CanBackend`]) that
//!   spawns the helper — this same binary re-invoked as the hidden
//!   `__can-host` subcommand — and talks to it over `stdin`/`stdout`.
//! - The helper opens the REAL backend in-process ([`run_host`]) and
//!   forwards frames both ways.
//! - If the driver faults, only the helper dies; the parent sees the
//!   pipe close and surfaces [`TransportError::Disconnected`]. The app
//!   stays alive and shows a clean "adapter disconnected".
//!
//! Only the native FFI backends need this (PCAN on macOS/Windows). SLCAN
//! (a serial unplug already errors cleanly) and SocketCAN (in-kernel)
//! are left in-process. See [`super::open_backend`] for the routing.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::thread;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use clap::ValueEnum;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex as TokioMutex};
use tracing::{debug, warn};

use crate::cli::InterfaceType;
use crate::protocol::CanFrame;

use super::{open_backend_direct, CanBackend, Result, TransportError};

/// Hidden subcommand name the parent invokes to spawn a helper.
pub const CAN_HOST_SUBCOMMAND: &str = "__can-host";

/// MPSC depth for the parent's RX queue. Matches the other backends.
const RX_QUEUE_DEPTH: usize = 256;

/// How long the helper's RX loop waits per `recv` before looping — short
/// enough that shutdown is prompt, long enough not to spin.
const HOST_RX_POLL: Duration = Duration::from_millis(50);

/// Grace period for the helper to exit cleanly on `stdin` EOF before we
/// SIGKILL it during teardown.
const GRACEFUL_EXIT: Duration = Duration::from_millis(250);

// ---- Wire protocol -------------------------------------------------
//
// Length-prefixed frames over the pipe: `[u32-LE body_len][body]`, where
// `body[0]` is a tag. Tiny binary encoding (not JSON) — at 500k a CAN
// frame is 11 bytes, so pipe bandwidth is a non-issue.

const TAG_TX: u8 = 0x01; // parent -> helper: transmit this frame
const TAG_RX: u8 = 0x02; // helper -> parent: received this frame
const TAG_READY: u8 = 0x03; // helper -> parent: backend opened OK
const TAG_OPEN_ERR: u8 = 0x04; // helper -> parent: open failed (utf8 reason)

/// A framed message on the stdio bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Msg {
    Frame(CanFrame),
    Ready,
    OpenErr(String),
}

/// Serialize a message with its length prefix. `frame_tag` is `TAG_TX`
/// or `TAG_RX` depending on direction.
fn encode(msg: &Msg, frame_tag: u8) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    match msg {
        Msg::Frame(f) => {
            body.push(frame_tag);
            body.extend_from_slice(&f.id.to_le_bytes());
            body.push(f.len);
            body.extend_from_slice(&f.data);
        }
        Msg::Ready => body.push(TAG_READY),
        Msg::OpenErr(reason) => {
            body.push(TAG_OPEN_ERR);
            body.extend_from_slice(reason.as_bytes());
        }
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Parse one message body (tag + payload) into a [`Msg`].
fn decode(body: &[u8]) -> Option<Msg> {
    let (&tag, rest) = body.split_first()?;
    match tag {
        TAG_TX | TAG_RX => {
            if rest.len() < 11 {
                return None;
            }
            let id = u16::from_le_bytes([rest[0], rest[1]]);
            let len = rest[2].min(8);
            let mut data = [0u8; 8];
            data.copy_from_slice(&rest[3..11]);
            Some(Msg::Frame(CanFrame { id, data, len }))
        }
        TAG_READY => Some(Msg::Ready),
        TAG_OPEN_ERR => Some(Msg::OpenErr(String::from_utf8_lossy(rest).into_owned())),
        _ => None,
    }
}

/// Blocking read of one framed message. `Ok(None)` on clean EOF (peer
/// closed) or a truncated frame (peer died mid-write) — both mean "gone".
fn read_blocking<R: Read>(r: &mut R) -> Option<Msg> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).ok()?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 || len > 4096 {
        return None; // malformed / desync — treat as gone
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).ok()?;
    decode(&body)
}

/// Async read of one framed message (helper side). `Ok(None)` on EOF.
async fn read_async<R: AsyncReadExt + Unpin>(r: &mut R) -> Option<Msg> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await.ok()?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 || len > 4096 {
        return None;
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await.ok()?;
    decode(&body)
}

// ---- Parent side ---------------------------------------------------

/// A [`CanBackend`] that proxies to a real backend running in a helper
/// process. A driver crash kills only the helper; the parent surfaces
/// [`TransportError::Disconnected`].
pub struct IsolatedBackend {
    child: StdMutex<Option<Child>>,
    stdin: StdMutex<Option<ChildStdin>>,
    rx: Arc<TokioMutex<mpsc::Receiver<CanFrame>>>,
    /// Set on teardown so the reader thread stops trying to enqueue and
    /// can never wedge on a full queue (which would hang Drop's join).
    shutdown: Arc<AtomicBool>,
    reader_handle: StdMutex<Option<thread::JoinHandle<()>>>,
    description: String,
}

impl IsolatedBackend {
    /// Spawn a helper for `iface`/`channel`/`bitrate` and wait for it to
    /// report the backend opened. Called from the (sync) `open_backend`,
    /// so the open handshake is a blocking read of the first frame.
    pub fn spawn(iface: InterfaceType, channel: &str, bitrate: u32) -> Result<Self> {
        let exe = std::env::current_exe()
            .map_err(|e| TransportError::Other(format!("locating own executable: {e}")))?;

        let iface_arg = iface
            .to_possible_value()
            .map(|v| v.get_name().to_string())
            .ok_or_else(|| TransportError::Other("interface has no value name".into()))?;

        let mut child = Command::new(exe)
            .arg("--interface")
            .arg(&iface_arg)
            .arg("--channel")
            .arg(channel)
            .arg("--bitrate")
            .arg(bitrate.to_string())
            .arg(CAN_HOST_SUBCOMMAND)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Inherit stderr so the helper's tracing + any panic land in
            // the parent's console for debugging.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| TransportError::Other(format!("spawning CAN host helper: {e}")))?;

        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| TransportError::Other("helper stdout not piped".into()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| TransportError::Other("helper stdin not piped".into()))?;

        // Open handshake: the first frame is READY (opened) or OPEN_ERR.
        match read_blocking(&mut stdout) {
            Some(Msg::Ready) => {}
            Some(Msg::OpenErr(reason)) => {
                let _ = child.wait();
                // The helper already rendered a descriptive TransportError
                // as text; pass it straight through.
                return Err(TransportError::Other(reason));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(TransportError::Other(
                    "CAN host helper exited before reporting readiness".into(),
                ));
            }
        }

        let (tx, rx) = mpsc::channel(RX_QUEUE_DEPTH);
        let shutdown = Arc::new(AtomicBool::new(false));
        let reader_shutdown = Arc::clone(&shutdown);
        let reader_handle = thread::Builder::new()
            .name("can-host-reader".into())
            .spawn(move || reader_thread(stdout, tx, reader_shutdown))
            .map_err(|e| TransportError::Other(format!("spawn host reader: {e}")))?;

        Ok(Self {
            child: StdMutex::new(Some(child)),
            stdin: StdMutex::new(Some(stdin)),
            rx: Arc::new(TokioMutex::new(rx)),
            shutdown,
            reader_handle: StdMutex::new(Some(reader_handle)),
            description: format!("{iface_arg} (isolated helper: {channel} @ {bitrate} bps)"),
        })
    }
}

/// Drain the helper's stdout, forwarding decoded frames to the async
/// side. Exits on EOF/error (helper gone → the mpsc closes → `recv`
/// returns `Disconnected`) or when shutdown is signalled.
fn reader_thread(mut stdout: ChildStdout, tx: mpsc::Sender<CanFrame>, shutdown: Arc<AtomicBool>) {
    loop {
        match read_blocking(&mut stdout) {
            Some(Msg::Frame(frame)) => {
                // Non-blocking enqueue: retry a full queue but honour
                // shutdown, so teardown can never wedge this thread.
                let mut pending = frame;
                loop {
                    match tx.try_send(pending) {
                        Ok(()) => break,
                        Err(mpsc::error::TrySendError::Closed(_)) => return,
                        Err(mpsc::error::TrySendError::Full(f)) => {
                            if shutdown.load(Ordering::SeqCst) {
                                return;
                            }
                            pending = f;
                            thread::sleep(Duration::from_millis(1));
                        }
                    }
                }
            }
            Some(_) => {}   // unexpected control frame mid-stream — ignore
            None => return, // EOF / truncated → helper gone
        }
    }
}

#[async_trait]
impl CanBackend for IsolatedBackend {
    async fn send(&self, frame: CanFrame) -> Result<()> {
        let bytes = encode(&Msg::Frame(frame), TAG_TX);
        let mut guard = self
            .stdin
            .lock()
            .map_err(|_| TransportError::Other("host stdin lock poisoned".into()))?;
        let stdin = guard.as_mut().ok_or(TransportError::Disconnected)?;
        // A small write to a pipe with a multi-KB kernel buffer is
        // effectively instantaneous at our frame rate; a broken pipe
        // (helper died) surfaces as Disconnected.
        stdin
            .write_all(&bytes)
            .and_then(|()| stdin.flush())
            .map_err(|_| TransportError::Disconnected)
    }

    async fn recv(&self, timeout: Duration) -> Result<CanFrame> {
        let mut rx = self.rx.lock().await;
        match tokio::time::timeout(timeout, rx.recv()).await {
            Ok(Some(frame)) => Ok(frame),
            Ok(None) => Err(TransportError::Disconnected),
            Err(_elapsed) => Err(TransportError::Timeout(timeout)),
        }
    }

    async fn set_bitrate(&self, _nominal_bps: u32) -> Result<()> {
        // Bitrate is fixed at spawn (the helper opened at that rate); a
        // change means respawning the helper. No mid-session use today.
        Ok(())
    }

    fn description(&self) -> String {
        self.description.clone()
    }
}

impl Drop for IsolatedBackend {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);

        // 1. Close stdin → the helper sees EOF, drops its backend (clean
        //    CAN_Uninitialize), and exits.
        if let Ok(mut g) = self.stdin.lock() {
            g.take();
        }

        // 2. Reap the child: give it a moment to exit gracefully on the
        //    EOF, then SIGKILL as a fallback so teardown is bounded.
        if let Ok(mut g) = self.child.lock() {
            if let Some(mut child) = g.take() {
                let deadline = Instant::now() + GRACEFUL_EXIT;
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) if Instant::now() < deadline => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        _ => {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                    }
                }
            }
        }

        // 3. The reader thread exits on stdout EOF (child now dead); join.
        if let Ok(mut g) = self.reader_handle.lock() {
            if let Some(h) = g.take() {
                let _ = h.join();
            }
        }
    }
}

// ---- Helper side (`__can-host`) ------------------------------------

/// Early-dispatch guard for **every** binary that can be `open_backend`'s
/// `current_exe()` — the `can-flasher` CLI *and* the `can-studio` Tauri
/// app. Call it as the FIRST thing in `main`, before any clap parsing or
/// GUI init.
///
/// If this process was spawned as an isolation helper (argv contains
/// `__can-host`), it parses the `--interface` / `--channel` / `--bitrate`
/// flags, runs the [`run_host`] bridge to completion on a fresh runtime,
/// and returns `true` — the caller must then exit immediately (do NOT
/// launch the UI / parse subcommands). Returns `false` on a normal launch.
#[must_use]
pub fn maybe_run_as_host() -> bool {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == CAN_HOST_SUBCOMMAND) {
        return false;
    }

    let mut iface = InterfaceType::Pcan;
    let mut channel: Option<String> = None;
    let mut bitrate: u32 = 500_000;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--interface" => {
                if let Some(v) = args.get(i + 1) {
                    if let Ok(x) = InterfaceType::from_str(v, true) {
                        iface = x;
                    }
                }
                i += 1;
            }
            "--channel" => {
                channel = args.get(i + 1).cloned();
                i += 1;
            }
            "--bitrate" => {
                if let Some(v) = args.get(i + 1) {
                    if let Ok(b) = v.parse() {
                        bitrate = b;
                    }
                }
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("can-host: failed to build runtime: {e}");
            std::process::exit(1);
        }
    };
    let _ = rt.block_on(run_host(iface, channel.as_deref(), bitrate));
    true
}

/// Body of the hidden `__can-host` subcommand: open the real backend
/// in-process and bridge it to the parent over stdio. Runs until the
/// parent closes `stdin` (EOF) or the backend disconnects.
///
/// stdout carries ONLY the binary protocol — all logging goes to stderr.
pub async fn run_host(iface: InterfaceType, channel: Option<&str>, bitrate: u32) -> Result<()> {
    // Open the backend WITHOUT isolation (open_backend_direct), so we
    // don't recursively spawn another helper.
    let backend = match open_backend_direct(iface, channel, bitrate) {
        Ok(b) => Arc::<dyn CanBackend>::from(b),
        Err(e) => {
            // Report the failure to the parent, then exit cleanly — the
            // parent turns OPEN_ERR into the real error for the operator.
            let mut out = std::io::stdout();
            let _ = out.write_all(&encode(&Msg::OpenErr(e.to_string()), 0));
            let _ = out.flush();
            return Ok(());
        }
    };

    // RX task: READY, then forward every received frame to stdout.
    let rx_backend = Arc::clone(&backend);
    let rx_task = tokio::spawn(async move {
        let mut out = tokio::io::stdout();
        if out.write_all(&encode(&Msg::Ready, 0)).await.is_err() || out.flush().await.is_err() {
            return;
        }
        loop {
            match rx_backend.recv(HOST_RX_POLL).await {
                Ok(frame) => {
                    let bytes = encode(&Msg::Frame(frame), TAG_RX);
                    if out.write_all(&bytes).await.is_err() || out.flush().await.is_err() {
                        return; // parent gone
                    }
                }
                Err(TransportError::Timeout(_)) => {} // keep polling
                Err(_) => return,                     // Disconnected / fatal → stop
            }
        }
    });

    // TX loop: read framed frames from stdin, transmit them. Ends on EOF
    // (parent closed) — the trigger for a clean shutdown.
    let mut stdin = tokio::io::stdin();
    while let Some(msg) = read_async(&mut stdin).await {
        if let Msg::Frame(frame) = msg {
            // A send error (adapter hiccup) isn't fatal to the bridge;
            // let the RX side decide when the device is truly gone.
            if let Err(err) = backend.send(frame).await {
                if matches!(err, TransportError::Disconnected) {
                    break;
                }
                debug!(?err, "can-host: send failed");
            }
        }
    }

    // Parent closed stdin (or backend gone): stop the RX task and drop
    // the backend so it uninitializes cleanly before we exit.
    rx_task.abort();
    warn!("can-host: bridge closing (parent disconnected or backend gone)");
    drop(backend);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(id: u16, bytes: &[u8]) -> CanFrame {
        CanFrame::new(id, bytes).unwrap()
    }

    #[test]
    fn encode_decode_frame_roundtrip_tx_and_rx() {
        for f in [
            frame(0x003, &[1, 2, 3]),
            frame(0x7FF, &[0xAB, 0xCD]),
            frame(0x100, &[]),
            frame(0x1EF, &[0xAA; 8]),
        ] {
            for tag in [TAG_TX, TAG_RX] {
                let wire = encode(&Msg::Frame(f), tag);
                // strip the 4-byte length prefix before decode()
                let body = &wire[4..];
                assert_eq!(
                    decode(body),
                    Some(Msg::Frame(f)),
                    "roundtrip {f:?} tag {tag:#x}"
                );
            }
        }
    }

    #[test]
    fn encode_decode_control_messages() {
        let ready = encode(&Msg::Ready, 0);
        assert_eq!(decode(&ready[4..]), Some(Msg::Ready));
        let err = encode(&Msg::OpenErr("no adapter".into()), 0);
        assert_eq!(decode(&err[4..]), Some(Msg::OpenErr("no adapter".into())));
    }

    #[test]
    fn read_blocking_streams_frames_then_eof() {
        // Two frames back-to-back, then EOF.
        let mut buf = Vec::new();
        buf.extend_from_slice(&encode(&Msg::Frame(frame(0x123, &[9, 8, 7])), TAG_RX));
        buf.extend_from_slice(&encode(&Msg::Frame(frame(0x456, &[1])), TAG_RX));
        let mut cursor = std::io::Cursor::new(buf);
        assert_eq!(
            read_blocking(&mut cursor),
            Some(Msg::Frame(frame(0x123, &[9, 8, 7])))
        );
        assert_eq!(
            read_blocking(&mut cursor),
            Some(Msg::Frame(frame(0x456, &[1])))
        );
        assert_eq!(read_blocking(&mut cursor), None, "clean EOF -> None");
    }

    #[test]
    fn read_blocking_truncated_frame_is_none() {
        // Length says 11 bytes of body but only 4 provided → truncated.
        let mut buf = 11u32.to_le_bytes().to_vec();
        buf.extend_from_slice(&[TAG_RX, 0, 0, 0]);
        let mut cursor = std::io::Cursor::new(buf);
        assert_eq!(read_blocking(&mut cursor), None);
    }

    #[test]
    fn decode_rejects_garbage_tag_and_short_frame() {
        assert_eq!(decode(&[0xFF]), None); // unknown tag
        assert_eq!(decode(&[TAG_TX, 0, 1]), None); // frame body too short
        assert_eq!(decode(&[]), None); // empty
    }
}
