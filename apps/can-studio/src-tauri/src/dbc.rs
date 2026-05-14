// DBC file support — Tier 2.
//
// Parses a Vector/BusMaster-exported DBC file via the `can-dbc`
// crate (schema only) and pairs it with a hand-written decoder
// that turns each incoming `CanFrame` into a list of physical
// signal values. The DBC lives in a `tauri::manage`d slot so the
// bus-monitor reader task can decode every frame in place
// without copying the parsed structure across thread boundaries.
//
// Decoder math (mirroring Vector's canonical formula):
//
//   raw   = bit-extract data[start_bit..start_bit+signal_size]
//           with sign extension if value_type=Signed and
//           byte-order swap if byte_order=LittleEndian
//   phys  = raw * factor + offset
//
// We support 11-bit standard IDs only; the transport layer hasn't
// surfaced 29-bit extended IDs yet. Multiplexed signals are kept
// in the schema but skipped at decode time — the operator picks
// up the unmultiplexed payload, multiplexed bits drop silently.
// Full mux support can land in Tier 2.1 if anyone asks.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use can_dbc::{ByteOrder, Dbc, MessageId, NumericValue, ValueType};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;

use can_flasher::protocol::CanFrame;

const STATUS_EVENT: &str = "dbc:status";

// ---- Shared state ----

#[derive(Default)]
pub struct DbcState {
    inner: Mutex<Option<LoadedDbc>>,
}

impl DbcState {
    /// Snapshot the parsed DBC for borrow-free read-only use in
    /// the bus-monitor reader task. Returns `None` when no file
    /// is loaded. The returned `Arc<Dbc>` outlives the lock so
    /// callers don't have to hold the mutex across decode work.
    pub async fn snapshot(&self) -> Option<Arc<Dbc>> {
        self.inner.lock().await.as_ref().map(|d| d.dbc.clone())
    }
}

struct LoadedDbc {
    path: PathBuf,
    dbc: Arc<Dbc>,
    /// Eager (message_id, message_index) lookup so the reader
    /// task can hit O(1) on every frame regardless of DBC size.
    by_id: Arc<HashMap<u32, usize>>,
}

// ---- Requests / responses ----

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DbcLoadRequest {
    pub path: String,
}

/// Summary returned from `dbc_load` and `dbc_status` so the UI can
/// confirm what's loaded without re-parsing the file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbcSummary {
    pub path: String,
    pub message_count: usize,
    pub signal_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DbcStatusEvent {
    Loaded(DbcSummary),
    Unloaded,
    Error { message: String },
}

/// Schema dump returned to the Signals view. A flat list of every
/// signal with the parent message attached, plus value-table
/// labels if any. The frontend renders this once at load and
/// then matches incoming live values by `signalKey` (a stable
/// `message_id|signal_name` tuple).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalSchema {
    pub signal_key: String,
    pub message_id: u32,
    pub message_name: String,
    pub signal_name: String,
    pub unit: String,
    pub factor: f64,
    pub offset: f64,
    pub min: f64,
    pub max: f64,
}

/// One decoded signal value, suitable for streaming to the
/// frontend as a `bus_monitor:signals` payload.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecodedSignal {
    pub signal_key: String,
    pub value: f64,
}

// ---- Commands ----

#[tauri::command]
pub async fn dbc_load(
    app: AppHandle,
    state: State<'_, DbcState>,
    request: DbcLoadRequest,
) -> Result<DbcSummary, String> {
    let path = PathBuf::from(&request.path);
    let raw = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("read DBC: {e}"))?;
    // Strip a UTF-8 BOM if present — some Windows-exported DBC
    // files ship with one and can-dbc is whitespace-tolerant but
    // not BOM-tolerant.
    let cleaned = raw.strip_prefix('\u{FEFF}').unwrap_or(&raw);
    let parsed = Dbc::try_from(cleaned).map_err(|e| format!("parse DBC: {e:?}"))?;

    let mut by_id: HashMap<u32, usize> = HashMap::with_capacity(parsed.messages.len());
    let mut signal_count = 0usize;
    for (idx, msg) in parsed.messages.iter().enumerate() {
        let id = message_id_to_u32(&msg.id);
        by_id.insert(id, idx);
        signal_count += msg.signals.len();
    }

    let summary = DbcSummary {
        path: path.display().to_string(),
        message_count: parsed.messages.len(),
        signal_count,
    };

    {
        let mut slot = state.inner.lock().await;
        *slot = Some(LoadedDbc {
            path,
            dbc: Arc::new(parsed),
            by_id: Arc::new(by_id),
        });
    }
    let _ = app.emit(STATUS_EVENT, &DbcStatusEvent::Loaded(summary.clone()));
    Ok(summary)
}

#[tauri::command]
pub async fn dbc_unload(app: AppHandle, state: State<'_, DbcState>) -> Result<(), String> {
    let mut slot = state.inner.lock().await;
    *slot = None;
    let _ = app.emit(STATUS_EVENT, &DbcStatusEvent::Unloaded);
    Ok(())
}

#[tauri::command]
pub async fn dbc_status(state: State<'_, DbcState>) -> Result<Option<DbcSummary>, String> {
    let slot = state.inner.lock().await;
    Ok(slot.as_ref().map(|d| DbcSummary {
        path: d.path.display().to_string(),
        message_count: d.dbc.messages.len(),
        signal_count: d.dbc.messages.iter().map(|m| m.signals.len()).sum(),
    }))
}

/// Returns the full signal schema. Called once by the Signals view
/// on DBC load — the per-frame stream of decoded values comes via
/// the bus-monitor's signal-events channel.
#[tauri::command]
pub async fn dbc_signals(state: State<'_, DbcState>) -> Result<Vec<SignalSchema>, String> {
    let slot = state.inner.lock().await;
    let Some(d) = slot.as_ref() else {
        return Ok(vec![]);
    };
    let mut out = Vec::new();
    for msg in &d.dbc.messages {
        let mid = message_id_to_u32(&msg.id);
        for sig in &msg.signals {
            out.push(SignalSchema {
                signal_key: format!("{mid}|{}", sig.name),
                message_id: mid,
                message_name: msg.name.clone(),
                signal_name: sig.name.clone(),
                unit: sig.unit.clone(),
                factor: sig.factor,
                offset: sig.offset,
                min: numeric_value_to_f64(&sig.min),
                max: numeric_value_to_f64(&sig.max),
            });
        }
    }
    Ok(out)
}

/// `NumericValue` is `Uint(u64) | Int(i64) | Double(f64)`. The
/// frontend wants a single f64 for the Range column; we widen
/// integer variants. Tier 2.1 can shift to a tagged numeric if
/// the precision loss bites.
fn numeric_value_to_f64(v: &NumericValue) -> f64 {
    match v {
        NumericValue::Double(f) => *f,
        NumericValue::Int(i) => *i as f64,
        NumericValue::Uint(u) => *u as f64,
    }
}

/// Flatten `MessageId::Standard(u16) | Extended(u32)` to the
/// single u32 we use in the frontend's `signalKey`. Borrow-based
/// pattern matching so we don't depend on `MessageId: Copy`.
fn message_id_to_u32(id: &MessageId) -> u32 {
    match id {
        MessageId::Standard(s) => u32::from(*s),
        MessageId::Extended(e) => *e,
    }
}

// ---- Decoder ----

/// Decode a single frame against the currently-loaded DBC. Returns
/// an empty Vec when no DBC is loaded or the frame's ID isn't in
/// the schema. The bus-monitor reader task calls this on every
/// frame; the hot path is the `by_id.get(...)` lookup followed by
/// a bounded loop over the message's signals.
pub fn decode(dbc: &Dbc, by_id: &HashMap<u32, usize>, frame: &CanFrame) -> Vec<DecodedSignal> {
    let mid = u32::from(frame.id);
    let Some(&idx) = by_id.get(&mid) else {
        return vec![];
    };
    let msg = match dbc.messages.get(idx) {
        Some(m) => m,
        None => return vec![],
    };
    let mut out = Vec::with_capacity(msg.signals.len());
    for sig in &msg.signals {
        let raw = extract_bits(
            &frame.data,
            sig.start_bit as u32,
            sig.size as u32,
            &sig.byte_order,
            &sig.value_type,
        );
        let phys = raw * sig.factor + sig.offset;
        out.push(DecodedSignal {
            signal_key: format!("{mid}|{}", sig.name),
            value: phys,
        });
    }
    out
}

/// Extract a signal's raw integer value from an 8-byte frame.
///
/// CAN DBC byte-order semantics follow the Vector convention:
///
/// - **LittleEndian (Intel)**: `start_bit` is the LSB of the
///   field; bits are numbered LSB-first within each byte and
///   then ascending across bytes. Read straight out.
/// - **BigEndian (Motorola)**: `start_bit` is the MSB of the
///   field; bits run from the start bit downward through the
///   current byte's LSB, then continue at MSB of the next byte.
///
/// `Signed` values are sign-extended into f64 after extraction.
/// Anything wider than 64 bits is clamped — we don't yet have
/// real signals that need bigints.
fn extract_bits(
    data: &[u8; 8],
    start_bit: u32,
    size: u32,
    byte_order: &ByteOrder,
    value_type: &ValueType,
) -> f64 {
    let size = size.min(64);
    if size == 0 {
        return 0.0;
    }

    let mut raw: u64 = 0;
    match byte_order {
        ByteOrder::LittleEndian => {
            for i in 0..size {
                let bit = start_bit + i;
                let byte_idx = (bit / 8) as usize;
                let bit_idx = bit % 8;
                if byte_idx >= 8 {
                    break;
                }
                let v = (data[byte_idx] >> bit_idx) & 1;
                raw |= u64::from(v) << i;
            }
        }
        ByteOrder::BigEndian => {
            // Motorola walk: start at start_bit (MSB), step down
            // through the byte's LSB (bit 0), then jump to MSB
            // of the next byte (bit 7) and repeat.
            let mut bit = start_bit as i32;
            for i in 0..size {
                let byte_idx = (bit / 8) as usize;
                let bit_idx = (bit % 8) as u32;
                if byte_idx >= 8 || bit < 0 {
                    break;
                }
                let v = (data[byte_idx] >> bit_idx) & 1;
                raw |= u64::from(v) << (size - 1 - i);
                if bit_idx == 0 {
                    // Wrap to MSB of next byte.
                    bit += 15;
                } else {
                    bit -= 1;
                }
            }
        }
    }

    match value_type {
        ValueType::Signed => {
            // Sign-extend from `size` bits into i64, then to f64.
            let sign_bit = 1u64 << (size - 1);
            if raw & sign_bit != 0 {
                let extend = !0u64 << size;
                let signed = (raw | extend) as i64;
                signed as f64
            } else {
                raw as f64
            }
        }
        ValueType::Unsigned => raw as f64,
    }
}

/// Helper: borrow the indexed lookup map for the bus-monitor's
/// hot path. The reader task takes one snapshot when DBC is
/// loaded and uses it until DBC changes.
pub async fn snapshot_lookup(state: &DbcState) -> Option<(Arc<Dbc>, Arc<HashMap<u32, usize>>)> {
    let slot = state.inner.lock().await;
    slot.as_ref().map(|d| (d.dbc.clone(), d.by_id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_frame(data: [u8; 8]) -> CanFrame {
        CanFrame {
            id: 0x100,
            data,
            len: 8,
        }
    }

    #[test]
    fn little_endian_byte_aligned_unsigned() {
        // bits 0..8 of byte 0 = 0xA5
        let frame = make_frame([0xA5, 0, 0, 0, 0, 0, 0, 0]);
        let raw = extract_bits(
            &frame.data,
            0,
            8,
            &ByteOrder::LittleEndian,
            &ValueType::Unsigned,
        );
        assert_eq!(raw, 0xA5 as f64);
    }

    #[test]
    fn little_endian_16bit_unsigned_crosses_byte() {
        // bits 0..16, LE → little-endian u16. data 0x34 0x12 → 0x1234.
        let frame = make_frame([0x34, 0x12, 0, 0, 0, 0, 0, 0]);
        let raw = extract_bits(
            &frame.data,
            0,
            16,
            &ByteOrder::LittleEndian,
            &ValueType::Unsigned,
        );
        assert_eq!(raw, 0x1234 as f64);
    }

    #[test]
    fn little_endian_signed_sign_extends() {
        // 8-bit signed -1 in byte 0.
        let frame = make_frame([0xFF, 0, 0, 0, 0, 0, 0, 0]);
        let raw = extract_bits(
            &frame.data,
            0,
            8,
            &ByteOrder::LittleEndian,
            &ValueType::Signed,
        );
        assert_eq!(raw, -1.0);
    }

    #[test]
    fn big_endian_16bit_motorola_walk() {
        // Motorola u16 starting at bit 7 of byte 0 (MSB) — that's
        // the conventional "byte 0 MSB first" big-endian u16.
        // data 0x12 0x34 → 0x1234.
        let frame = make_frame([0x12, 0x34, 0, 0, 0, 0, 0, 0]);
        let raw = extract_bits(
            &frame.data,
            7,
            16,
            &ByteOrder::BigEndian,
            &ValueType::Unsigned,
        );
        assert_eq!(raw, 0x1234 as f64);
    }
}
