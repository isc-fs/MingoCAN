//! Integration test for the `discover` subcommand's enrichment
//! pipeline against a live [`StubDevice`].
//!
//! The stub implements `CMD_DISCOVER` but NACKs `GET_FW_INFO` and
//! `GET_HEALTH` with `UNSUPPORTED` — so the per-responder enrichment
//! step exercises the degrade-gracefully path end-to-end. When the
//! stub grows real `GET_FW_INFO` / `GET_HEALTH` handlers (later feat
//! branches), this test will keep passing and additionally assert
//! against the populated fields.
//!
//! The bottom section spawns the real binary via
//! `CARGO_BIN_EXE_can-flasher` to pin the CLI-level enrichment
//! contract — that regressions in the `cli::discover::run` payload
//! slicing (fix/7) get caught before they ship.

use std::time::Duration;

use tokio::sync::oneshot;

use can_flasher::protocol::commands::{cmd_discover, cmd_get_fw_info, cmd_get_health};
use can_flasher::protocol::ids::MessageType;
use can_flasher::protocol::opcodes::NackCode;
use can_flasher::protocol::records::HealthRecord;
use can_flasher::protocol::Response;
use can_flasher::session::{Session, SessionConfig};
use can_flasher::transport::{CanBackend, StubDevice, VirtualBus};

const STUB_NODE: u8 = 0x3;

async fn spawn_session_and_stub() -> (Session, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let bus = VirtualBus::new();
    let host = bus.host_backend();
    let device: Box<dyn CanBackend> = Box::new(bus.device_backend());
    drop(bus);

    let stub = StubDevice::new(device, STUB_NODE);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let handle = tokio::spawn(async move {
        let _ = stub.run(cancel_rx).await;
    });

    let config = SessionConfig {
        target_node: 0x0,
        keepalive_interval: Duration::from_millis(5_000),
        command_timeout: Duration::from_millis(200),
        ..SessionConfig::default()
    };
    let session = Session::attach(Box::new(host), config);
    (session, cancel_tx, handle)
}

#[tokio::test]
async fn broadcast_discover_finds_stub() {
    let (session, cancel, handle) = spawn_session_and_stub().await;
    let replies = session
        .broadcast(
            &cmd_discover(),
            MessageType::DiscoverRequest,
            Duration::from_millis(150),
        )
        .await
        .expect("broadcast");
    assert_eq!(replies.len(), 1);
    match &replies[0] {
        Response::Discover { node_id, .. } => assert_eq!(*node_id, STUB_NODE),
        other => panic!("expected Discover, got {other:?}"),
    }
    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn stub_nacks_fw_info_with_unsupported() {
    // Exercises the degrade-gracefully path of discover's enrichment
    // loop. The stub represents a bootloader with no app installed,
    // so GET_FW_INFO returns NACK(UNSUPPORTED) (functionally
    // equivalent to NoValidApp from the host's point of view). The
    // discover subcommand maps this to fw_error; this test asserts
    // the NACK surfaces correctly at the session layer.
    let (session, cancel, handle) = spawn_session_and_stub().await;

    let fw = session
        .send_command_to(STUB_NODE, &cmd_get_fw_info())
        .await
        .expect("GET_FW_INFO");
    match fw {
        Response::Nack { code, .. } => assert_eq!(code, NackCode::Unsupported),
        other => panic!("expected Nack for GET_FW_INFO against stub, got {other:?}"),
    }

    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn stub_answers_get_health_with_synthetic_record() {
    // As of feat/11 the stub answers GET_HEALTH with a realistic
    // 32-byte HealthRecord (uptime from the monotonic clock, reset
    // cause POWER_ON, session flag reflecting internal state). This
    // test confirms the wire format round-trips cleanly.
    let (session, cancel, handle) = spawn_session_and_stub().await;

    let resp = session
        .send_command_to(STUB_NODE, &cmd_get_health())
        .await
        .expect("GET_HEALTH");
    match resp {
        Response::Ack { opcode, payload } => {
            assert_eq!(opcode, 0x05, "GET_HEALTH ACK opcode");
            let record = HealthRecord::parse(&payload).expect("parse HealthRecord");
            // Fresh session — not connected. `session_active` should
            // be false; reset cause should be POWER_ON (stub's
            // latched default).
            assert!(!record.session_active());
            assert_eq!(record.reset_cause().map(|r| r.as_str()), Some("POWER_ON"),);
        }
        other => panic!("expected Ack for GET_HEALTH against stub, got {other:?}"),
    }

    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}

#[tokio::test]
async fn send_command_to_an_absent_node_times_out() {
    // No node 0x7 on the bus — the stub is 0x3. The session should
    // time out cleanly instead of wedging.
    let (session, cancel, handle) = spawn_session_and_stub().await;
    let err = session
        .send_command_to(0x7, &cmd_get_fw_info())
        .await
        .expect_err("expected timeout");
    match err {
        can_flasher::session::SessionError::CommandTimeout(d) => {
            assert_eq!(d, Duration::from_millis(200));
        }
        other => panic!("expected CommandTimeout, got {other:?}"),
    }
    let _ = session.disconnect().await;
    let _ = cancel.send(());
    let _ = handle.await;
}

// ---- CLI-level regression test ----

/// Spawn the real binary against `--interface virtual`, run
/// `discover --json`, and confirm the enrichment pipeline doesn't
/// regress. Fix/7 corrected a payload-offset bug in
/// `enrich_with_fw_info` / `enrich_with_health`: both sliced
/// `payload[1..]` even though `Response::Ack.payload` already has
/// the opcode stripped. Symptom was a 32-byte HealthRecord showing
/// up as `"GET_HEALTH ACK too short: got 32 bytes"` in the CLI
/// output.
///
/// The existing `stub_answers_get_health_with_synthetic_record`
/// test covered the wire path correctly (it calls
/// `HealthRecord::parse(&payload)` without the extra slice) so it
/// couldn't catch the CLI-side bug. This test spawns the binary
/// end-to-end to close the gap.
#[test]
fn discover_cli_does_not_emit_ack_too_short_on_enrichment() {
    use std::process::Command;
    let out = Command::new(env!("CARGO_BIN_EXE_can-flasher"))
        .args([
            "--interface",
            "virtual",
            "--node-id",
            "0x3",
            "--timeout",
            "400",
            "discover",
            "--json",
        ])
        .output()
        .expect("spawn can-flasher");

    assert!(
        out.status.success(),
        "discover --json failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();

    // The regression symptom: "ACK too short" leaked into either
    // stdout (the JSON error field) or stderr. Neither should
    // happen on a healthy enrichment round-trip.
    assert!(
        !stdout.contains("ACK too short"),
        "stdout should not contain 'ACK too short' after fix/7:\n{stdout}"
    );
    assert!(
        !stderr.contains("ACK too short"),
        "stderr should not contain 'ACK too short' after fix/7:\n{stderr}"
    );

    // Sanity: there's a responder row for the stub. The stub's
    // GET_FW_INFO / GET_HEALTH responses round-trip cleanly now,
    // so the row should carry a populated reset_cause field.
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("discover --json must emit valid JSON");
    let rows = parsed.as_array().expect("top-level must be an array");
    assert!(
        !rows.is_empty(),
        "expected at least one responder row, got empty array"
    );
    // Stub's bl_health_init default reset cause is POWER_ON. If
    // enrichment parsed cleanly the field is populated; if it
    // failed it'd be null or carry an error string.
    let first = &rows[0];
    let reset = first.get("reset_cause").and_then(|v| v.as_str());
    assert!(
        reset.is_some() && reset != Some(""),
        "reset_cause should be populated by successful GET_HEALTH; row={first}"
    );
}
