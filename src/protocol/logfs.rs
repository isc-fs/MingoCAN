//! LOGFS — microSD log-file extraction over the existing diag session.
//!
//! Spec of record: **IFS08-CE-AMS#406** (host-side alignment: #506). The
//! service rides the *existing* CONNECT session + ISO-TP transport — it is
//! a new application command group (`0x21`..=`0x25`), not a new bus.
//!
//! - All multi-byte fields are **little-endian**.
//! - A LIST entry is a **fixed 22 B** record; `name` is FAT 8.3,
//!   null-padded.
//! - `LOGFS_READ` is clamped by the firmware to [`MAX_READ_LEN`]; a
//!   **short read means EOF** (see [`ReadOutcome`]).
//! - **v1 is read-only** — `0x26 LOGFS_DELETE` is deliberately not
//!   implemented here.
//!
//! Two host-side gotchas the firmware spec calls out:
//!
//! - **Node id is not hardcoded** — the target node comes from
//!   `SessionConfig::target_node`, so the AMS `0x01 → 0x02` move
//!   (IFS08-CE-AMS#403) is a config change, not a code change.
//! - **[`LogEntry::mtime`] is boot-relative / monotonic**, NOT wall-clock
//!   (the AMS has no set RTC). Render it as an ordering / uptime value —
//!   never as a calendar date.
//!
//! NACK reasons for this group are still unassigned upstream (the values
//! named in #506 collide with the shipped table: `0x04`/`0x05` are the
//! reserved Phase-5 signature/replay codes and `0x08` is already `Busy`),
//! so LOGFS failures currently decode through `NackCode::Unknown` until
//! IFS08-CE-AMS#406 publishes a final table clear of `0x01..=0x10`/`0xFE`.

use super::ParseError;

/// Max bytes the firmware will return for one `LOGFS_READ`. Asking for
/// less is fine and returns exactly that; asking for more is clamped.
pub const MAX_READ_LEN: u16 = 512;

/// `next_cursor` value meaning "the listing is complete".
///
/// Not `0`: `0` is the *first* cursor, so a firmware that answered a
/// one-page listing with `next_cursor = 0` would be indistinguishable
/// from "start over" (IFS08-CE-AMS `logfs_server.hpp`, `CursorEnd`).
/// Sending `0xFFFF` *as* a cursor is rejected with `OUT_OF_BOUNDS`.
pub const CURSOR_END: u16 = 0xFFFF;

/// On-wire size of one LIST entry: `index:u16 + size:u32 + mtime:u32 +
/// name[12]`.
pub const ENTRY_LEN: usize = 22;

/// Width of the FAT 8.3 name field, null-padded.
pub const NAME_LEN: usize = 12;

/// LIST response header: `next_cursor:u16 + count:u8`.
const LIST_HEADER_LEN: usize = 3;

/// ISO-TP message ceiling shared with the bootloader (`BL_ISOTP_MAX_MSG`).
const MAX_MSG_LEN: usize = 1024;

/// Most entries that fit in one LIST page — the host must paginate on
/// `next_cursor` beyond this.
pub const MAX_ENTRIES_PER_PAGE: usize = (MAX_MSG_LEN - LIST_HEADER_LEN) / ENTRY_LEN;

/// One log file as advertised by `LOGFS_LIST`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    /// Opaque handle-selector passed back to `LOGFS_OPEN`.
    pub index: u16,
    /// File size in bytes.
    pub size: u32,
    /// **Monotonic / boot-relative** timestamp — the AMS has no set RTC.
    /// Use for ordering or as an uptime value; do NOT format as a date.
    pub mtime: u32,
    /// FAT 8.3 name with the null padding stripped (e.g. `LOG0001.CSV`).
    pub name: String,
}

impl LogEntry {
    /// Decode one fixed-size entry.
    #[must_use]
    pub fn parse(bytes: &[u8; ENTRY_LEN]) -> Self {
        let name_raw = &bytes[10..10 + NAME_LEN];
        let end = name_raw.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
        Self {
            index: u16::from_le_bytes([bytes[0], bytes[1]]),
            size: u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]),
            mtime: u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]),
            name: String::from_utf8_lossy(&name_raw[..end]).into_owned(),
        }
    }
}

/// One page of a `LOGFS_LIST` walk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListPage {
    /// Cursor to pass to the next `LOGFS_LIST`. `0` = the walk is done.
    pub next_cursor: u16,
    pub entries: Vec<LogEntry>,
}

impl ListPage {
    /// True when this was the last page.
    #[must_use]
    pub fn is_last(&self) -> bool {
        self.next_cursor == CURSOR_END
    }
}

/// Parse a `LOGFS_LIST` response body (opcode byte already stripped).
pub fn parse_list(bytes: &[u8]) -> Result<ListPage, ParseError> {
    if bytes.len() < LIST_HEADER_LEN {
        return Err(ParseError::RecordTooShort {
            got: bytes.len(),
            need: LIST_HEADER_LEN,
        });
    }
    let next_cursor = u16::from_le_bytes([bytes[0], bytes[1]]);
    let count = bytes[2] as usize;
    let need = LIST_HEADER_LEN + count * ENTRY_LEN;
    if bytes.len() < need {
        return Err(ParseError::RecordTooShort {
            got: bytes.len(),
            need,
        });
    }
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let off = LIST_HEADER_LEN + i * ENTRY_LEN;
        let chunk: &[u8; ENTRY_LEN] = bytes[off..off + ENTRY_LEN]
            .try_into()
            .expect("slice length checked above");
        entries.push(LogEntry::parse(chunk));
    }
    Ok(ListPage {
        next_cursor,
        entries,
    })
}

/// A file opened by `LOGFS_OPEN`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenedFile {
    /// Handle for the subsequent READ/CRC/CLOSE calls. Never `0` — the
    /// firmware reserves that as its "nothing open" marker, so a zeroed
    /// handle comes back as `BAD_HANDLE` rather than reading file 0.
    pub handle: u16,
    pub size: u32,
    /// Whole-file CRC32, or `0` when the firmware declined to compute it
    /// up front (the agreed default — verify with `LOGFS_CRC` at EOF
    /// instead of paying a full card read before the transfer starts).
    pub crc32: u32,
}

impl OpenedFile {
    /// True when the node deferred the CRC and the host must verify via
    /// `LOGFS_CRC`.
    #[must_use]
    pub fn crc_deferred(&self) -> bool {
        self.crc32 == 0
    }
}

/// Length of a `LOGFS_OPEN` reply body: `handle:u16, size:u32, crc32:u32`.
pub const OPEN_REPLY_LEN: usize = 10;

/// Parse a `LOGFS_OPEN` response body: `handle:u16, size:u32, crc32:u32`.
pub fn parse_open(bytes: &[u8]) -> Result<OpenedFile, ParseError> {
    if bytes.len() < OPEN_REPLY_LEN {
        return Err(ParseError::RecordTooShort {
            got: bytes.len(),
            need: OPEN_REPLY_LEN,
        });
    }
    Ok(OpenedFile {
        handle: u16::from_le_bytes([bytes[0], bytes[1]]),
        size: u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]),
        crc32: u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]),
    })
}

/// Parse a `LOGFS_FINALIZE` response body: `index:u16` — the index the
/// just-sealed log now occupies, ready to LIST or OPEN.
pub fn parse_finalize(bytes: &[u8]) -> Result<u16, ParseError> {
    if bytes.len() < 2 {
        return Err(ParseError::RecordTooShort {
            got: bytes.len(),
            need: 2,
        });
    }
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

/// Parse a `LOGFS_CRC` response body: `crc32:u32`.
pub fn parse_crc(bytes: &[u8]) -> Result<u32, ParseError> {
    if bytes.len() < 4 {
        return Err(ParseError::RecordTooShort {
            got: bytes.len(),
            need: 4,
        });
    }
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Result of one `LOGFS_READ`. A response shorter than the requested
/// length is the EOF signal — there is no separate end marker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadOutcome {
    pub data: Vec<u8>,
    /// The node returned fewer bytes than requested ⇒ end of file.
    pub eof: bool,
}

/// Interpret a `LOGFS_READ` response body against what was requested.
#[must_use]
pub fn parse_read(requested: u16, bytes: &[u8]) -> ReadOutcome {
    ReadOutcome {
        data: bytes.to_vec(),
        eof: bytes.len() < requested as usize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_bytes(index: u16, size: u32, mtime: u32, name: &str) -> [u8; ENTRY_LEN] {
        let mut b = [0u8; ENTRY_LEN];
        b[0..2].copy_from_slice(&index.to_le_bytes());
        b[2..6].copy_from_slice(&size.to_le_bytes());
        b[6..10].copy_from_slice(&mtime.to_le_bytes());
        b[10..10 + name.len()].copy_from_slice(name.as_bytes());
        b
    }

    #[test]
    fn entry_decodes_le_and_strips_null_padding() {
        let e = LogEntry::parse(&entry_bytes(7, 123_456, 98_765, "LOG0001.CSV"));
        assert_eq!(e.index, 7);
        assert_eq!(e.size, 123_456);
        assert_eq!(e.mtime, 98_765);
        assert_eq!(e.name, "LOG0001.CSV");
    }

    #[test]
    fn entry_handles_a_full_width_name() {
        // 12 chars = no null terminator at all.
        let e = LogEntry::parse(&entry_bytes(1, 0, 0, "ABCDEFGH.IJK"));
        assert_eq!(e.name, "ABCDEFGH.IJK");
        assert_eq!(e.name.len(), NAME_LEN);
    }

    #[test]
    fn list_page_parses_and_paginates() {
        let mut body = Vec::new();
        body.extend_from_slice(&9u16.to_le_bytes()); // next_cursor
        body.push(2); // count
        body.extend_from_slice(&entry_bytes(0, 10, 100, "LOG0001.CSV"));
        body.extend_from_slice(&entry_bytes(1, 20, 200, "LOG0002.CSV"));

        let page = parse_list(&body).unwrap();
        assert_eq!(page.next_cursor, 9);
        assert!(!page.is_last());
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[1].name, "LOG0002.CSV");
        assert_eq!(page.entries[1].size, 20);
    }

    #[test]
    fn list_page_sentinel_cursor_is_last() {
        let mut body = CURSOR_END.to_le_bytes().to_vec();
        body.push(0); // count
        let page = parse_list(&body).unwrap();
        assert!(page.is_last());
        assert!(page.entries.is_empty());
    }

    #[test]
    fn list_page_zero_cursor_is_not_last() {
        // 0 is the FIRST cursor, not a terminator. Treating it as the end
        // would silently truncate a listing to one page; treating the end
        // as a cursor would loop forever. Both were real bugs on #506.
        let body = [0u8, 0, 0];
        let page = parse_list(&body).unwrap();
        assert!(!page.is_last());
    }

    #[test]
    fn list_rejects_truncated_entry() {
        let mut body = vec![0, 0, 1]; // claims 1 entry
        body.extend_from_slice(&[0u8; ENTRY_LEN - 1]); // one byte short
        assert!(matches!(
            parse_list(&body),
            Err(ParseError::RecordTooShort { .. })
        ));
    }

    #[test]
    fn open_parses_and_flags_deferred_crc() {
        let mut body = 0x0103u16.to_le_bytes().to_vec();
        body.extend_from_slice(&4096u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(body.len(), OPEN_REPLY_LEN);
        let f = parse_open(&body).unwrap();
        assert_eq!(f.handle, 0x0103, "handle is u16 — a u8 read truncates it");
        assert_eq!(f.size, 4096);
        assert!(f.crc_deferred());

        let mut body = 0x0103u16.to_le_bytes().to_vec();
        body.extend_from_slice(&4096u32.to_le_bytes());
        body.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        let f = parse_open(&body).unwrap();
        assert_eq!(f.crc32, 0xDEAD_BEEF);
        assert!(!f.crc_deferred());
    }

    #[test]
    fn open_rejects_the_pre_452_nine_byte_reply() {
        // The old u8-handle shape is 9 B. It must be refused outright: it
        // is long enough that a lenient parser would happily read a
        // handle, a size and a CRC that are all off by one byte.
        let body = vec![0u8; 9];
        assert!(matches!(
            parse_open(&body),
            Err(ParseError::RecordTooShort { need: 10, .. })
        ));
    }

    #[test]
    fn finalize_parses_the_sealed_index() {
        assert_eq!(parse_finalize(&7u16.to_le_bytes()).unwrap(), 7);
        assert!(parse_finalize(&[1]).is_err());
    }

    #[test]
    fn crc_parses_le() {
        assert_eq!(
            parse_crc(&0x1234_5678u32.to_le_bytes()).unwrap(),
            0x1234_5678
        );
        assert!(matches!(
            parse_crc(&[1, 2, 3]),
            Err(ParseError::RecordTooShort { .. })
        ));
    }

    #[test]
    fn short_read_signals_eof() {
        let full = parse_read(512, &vec![0xAA; 512]);
        assert!(!full.eof);
        assert_eq!(full.data.len(), 512);

        let short = parse_read(512, &[0xAA; 100]);
        assert!(short.eof);
        assert_eq!(short.data.len(), 100);

        // A zero-length reply at an exact multiple of the read size is EOF.
        assert!(parse_read(512, &[]).eof);
    }

    #[test]
    fn page_ceiling_matches_the_isotp_budget() {
        assert_eq!(MAX_ENTRIES_PER_PAGE, 46);
    }
}
