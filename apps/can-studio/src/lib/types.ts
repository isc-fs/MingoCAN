// Wire types shared between the Tauri Rust commands and the Svelte
// frontend. Each interface here mirrors a `#[derive(Serialize)]`
// struct in `apps/can-studio/src-tauri/src/lib.rs` (and the
// underlying `can_flasher::cli::adapters::AdapterReport` types).
//
// Keeping the shapes in lockstep is enforced by hand for now. If we
// ever wire up `ts-rs` or `tauri-specta` for generated bindings,
// this file becomes the generation output.

export type InterfaceType =
    | 'slcan'
    | 'socketcan'
    | 'pcan'
    | 'vector'
    | 'virtual';

export interface AdapterReport {
    slcan: SlcanEntry[];
    socketcan: SocketCanEntry[];
    pcan: PcanEntry[];
    vector: VectorEntry[];
}

export interface SlcanEntry {
    channel: string;
    description: string;
    vid?: string;
    pid?: string;
}

export interface SocketCanEntry {
    interface: string;
}

export interface PcanEntry {
    channel: string;
    channel_byte: string;
}

export interface VectorEntry {
    channel: string;
    name: string;
    transceiver: string;
}

// ---- Flattened adapter row ----
//
// `AdapterReport` is keyed by backend. The picker / device tree
// generally wants one flat list with the backend stamped onto each
// row. This struct + the `flattenReport()` helper in cli.ts produce
// the flat shape from the wire shape.

export interface AdapterEntry {
    /** Backend kind — matches `InterfaceType` and `--interface`. */
    interface: InterfaceType;
    /** Channel string — exact value for `--channel`. */
    channel: string;
    /** Operator-facing label (e.g. "VN1610 Channel 1", "CANable 2.0"). */
    label: string;
    /** Optional detail line (USB VID:PID, transceiver name, hex byte). */
    detail?: string;
}
