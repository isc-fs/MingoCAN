//! Decode the AMS binary log files (`IMUnnnn.BIN`, `CELnnnn.BIN`, …) to CSV,
//! and save pulled log files (#613).
//!
//! A binary log is a 512-byte header that carries its own plain-text schema,
//! followed by fixed-size little-endian records. The decoder is driven
//! entirely by that schema, so a new stream (e.g. `ELEnnnn.BIN`) decodes with
//! no change here. Source of truth: AMS `Core/Inc/app/bin_log.hpp`; the
//! output matches the AMS reference decoder `tools/log_decode.py` byte for
//! byte (column names, number formatting, CRLF line ends), so CSVs from
//! either tool are interchangeable.
//!
//! [`save_pulled`] is what both `can-flasher logs pull` and the Studio Data
//! logs view call once a pull's bytes are CRC-verified: it writes the file
//! and, for a binary log, a decoded `<stem>.csv` beside it. A decode failure
//! never fails the save — the `.BIN` is the verified original and is always
//! kept.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// First 8 bytes of every binary log.
pub const MAGIC: &[u8; 8] = b"AMSBIN1\0";

/// Header length; records start right after it.
pub const HEADER_LEN: usize = 512;

/// Where the NUL-terminated schema text starts inside the header.
const SCHEMA_OFFSET: usize = 64;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("not an AMS binary log (bad magic)")]
    BadMagic,
    #[error("file is {0} B, shorter than the {HEADER_LEN}-byte header")]
    ShortHeader(usize),
    #[error("schema text is not ASCII")]
    SchemaNotAscii,
    #[error("schema line {line} malformed: {text:?}")]
    BadSchemaLine { line: usize, text: String },
    #[error("schema is empty")]
    EmptySchema,
    #[error("schema describes {schema} B per record but the header says {header} B")]
    RecordSizeMismatch { schema: usize, header: usize },
}

/// Field types a schema may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
}

impl FieldType {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "u8" => Self::U8,
            "i8" => Self::I8,
            "u16" => Self::U16,
            "i16" => Self::I16,
            "u32" => Self::U32,
            "i32" => Self::I32,
            _ => return None,
        })
    }

    pub fn size(self) -> usize {
        match self {
            Self::U8 | Self::I8 => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 => 4,
        }
    }

    /// Read one little-endian value from the start of `b`.
    fn read(self, b: &[u8]) -> i64 {
        match self {
            Self::U8 => i64::from(b[0]),
            Self::I8 => i64::from(b[0] as i8),
            Self::U16 => i64::from(u16::from_le_bytes([b[0], b[1]])),
            Self::I16 => i64::from(i16::from_le_bytes([b[0], b[1]])),
            Self::U32 => i64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            Self::I32 => i64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        }
    }
}

/// One schema line: `<name> <type> <count> <scale> <unit> [<flags>]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: FieldType,
    /// Empty for a scalar, `[n]` for `N`, `[a, b]` for `AxB` (row-major).
    pub dims: Vec<usize>,
    /// Multiply the raw value by this to get `unit`.
    pub scale: f64,
    pub unit: String,
    /// Flag `z`: a raw 0 means "not measured" and decodes to an empty cell.
    pub zero_empty: bool,
}

impl Field {
    pub fn count(&self) -> usize {
        self.dims.iter().product()
    }

    /// Whether values are scaled (and so carry the unit in the column name).
    fn scaled(&self) -> bool {
        self.scale != 1.0
    }

    /// CSV column names: `name`, `name0..nameN-1`, or `name<a>_<b>`; plus
    /// `_<unit>` (with `/` → `_`) when a scale is applied.
    pub fn columns(&self) -> Vec<String> {
        let mut names = match self.dims.as_slice() {
            [] => vec![self.name.clone()],
            [n] => (0..*n).map(|i| format!("{}{i}", self.name)).collect(),
            [a, b] => (0..*a)
                .flat_map(|i| (0..*b).map(move |j| (i, j)))
                .map(|(i, j)| format!("{}{i}_{j}", self.name))
                .collect(),
            _ => unreachable!("parse_schema only builds 0-2 dimensions"),
        };
        if self.scaled() && self.unit != "-" {
            let suffix = self.unit.replace('/', "_");
            for n in &mut names {
                n.push('_');
                n.push_str(&suffix);
            }
        }
        names
    }
}

/// The parsed 512-byte header.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub version: u16,
    pub record_size: usize,
    /// Rotation index, shared with `LOGnnnn.CSV`.
    pub rotation: u32,
    /// `tick_ms` when the file was opened (same clock as the LOG file).
    pub open_tick_ms: u32,
    pub stream: String,
    pub fw_version: [u8; 3],
    pub git_hash: [u8; 4],
    pub schema: Vec<Field>,
}

/// Whether `bytes` is an AMS binary log (starts with [`MAGIC`]).
pub fn is_bin_log(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

fn parse_scale(s: &str) -> Option<f64> {
    match s.split_once('/') {
        Some((num, den)) => Some(num.parse::<f64>().ok()? / den.parse::<f64>().ok()?),
        None => s.parse().ok(),
    }
}

/// Parse the schema text, one field per non-blank line.
pub fn parse_schema(text: &str) -> Result<Vec<Field>, DecodeError> {
    let mut fields = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let bad = || DecodeError::BadSchemaLine {
            line: i + 1,
            text: line.to_string(),
        };
        let tok: Vec<&str> = line.split_whitespace().collect();
        if tok.len() < 5 {
            return Err(bad());
        }
        let ty = FieldType::parse(tok[1]).ok_or_else(bad)?;
        let dims: Vec<usize> = if tok[2].contains('x') {
            let d: Vec<usize> = tok[2]
                .split('x')
                .map(|n| n.parse().map_err(|_| bad()))
                .collect::<Result<_, _>>()?;
            if d.len() != 2 {
                return Err(bad());
            }
            d
        } else {
            match tok[2].parse::<usize>().map_err(|_| bad())? {
                1 => Vec::new(),
                n => vec![n],
            }
        };
        if dims.contains(&0) {
            return Err(bad());
        }
        let scale = parse_scale(tok[3])
            .filter(|s| s.is_finite())
            .ok_or_else(bad)?;
        fields.push(Field {
            name: tok[0].to_string(),
            ty,
            dims,
            scale,
            unit: tok[4].to_string(),
            zero_empty: tok.get(5).is_some_and(|f| f.contains('z')),
        });
    }
    if fields.is_empty() {
        return Err(DecodeError::EmptySchema);
    }
    Ok(fields)
}

/// Parse and validate the header, including that the schema adds up to the
/// declared record size.
pub fn read_header(bytes: &[u8]) -> Result<Header, DecodeError> {
    if !is_bin_log(bytes) {
        return Err(DecodeError::BadMagic);
    }
    if bytes.len() < HEADER_LEN {
        return Err(DecodeError::ShortHeader(bytes.len()));
    }
    let u16_at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    let u32_at =
        |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let until_nul = |b: &[u8]| -> Vec<u8> { b.iter().copied().take_while(|&c| c != 0).collect() };

    let schema_bytes = until_nul(&bytes[SCHEMA_OFFSET..HEADER_LEN]);
    if !schema_bytes.is_ascii() {
        return Err(DecodeError::SchemaNotAscii);
    }
    let schema = parse_schema(&String::from_utf8_lossy(&schema_bytes))?;
    let record_size = usize::from(u16_at(10));
    let declared: usize = schema.iter().map(|f| f.ty.size() * f.count()).sum();
    if declared != record_size {
        return Err(DecodeError::RecordSizeMismatch {
            schema: declared,
            header: record_size,
        });
    }
    Ok(Header {
        version: u16_at(8),
        record_size,
        rotation: u32_at(12),
        open_tick_ms: u32_at(16),
        stream: String::from_utf8_lossy(&until_nul(&bytes[20..28])).into_owned(),
        fw_version: [bytes[28], bytes[29], bytes[30]],
        git_hash: [bytes[32], bytes[33], bytes[34], bytes[35]],
        schema,
    })
}

/// A decoded file.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub header: Header,
    pub csv: String,
    pub records: usize,
    /// Bytes of a partial last record that were dropped (a power cut
    /// mid-write); 0 normally.
    pub torn_bytes: usize,
}

/// A scaled value: 6 decimals, trailing zeros stripped, `-0` → `0`
/// (identical to the reference decoder's `fmt`).
fn fmt_scaled(v: f64) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// Quote a CSV cell only when it needs it (Python `csv` QUOTE_MINIMAL).
fn csv_cell(out: &mut String, s: &str) {
    if s.contains([',', '"', '\r', '\n']) {
        out.push('"');
        out.push_str(&s.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(s);
    }
}

/// Decode a whole binary log to CSV text.
pub fn decode(bytes: &[u8]) -> Result<Decoded, DecodeError> {
    let header = read_header(bytes)?;
    let body = &bytes[HEADER_LEN..];
    let records = body.len() / header.record_size;
    let torn_bytes = body.len() % header.record_size;

    let mut csv = String::new();
    let columns: Vec<String> = header.schema.iter().flat_map(Field::columns).collect();
    for (i, c) in columns.iter().enumerate() {
        if i > 0 {
            csv.push(',');
        }
        csv_cell(&mut csv, c);
    }
    csv.push_str("\r\n");

    for rec in body.chunks_exact(header.record_size) {
        let mut off = 0;
        let mut first = true;
        for f in &header.schema {
            for _ in 0..f.count() {
                let raw = f.ty.read(&rec[off..]);
                off += f.ty.size();
                if !first {
                    csv.push(',');
                }
                first = false;
                if f.zero_empty && raw == 0 {
                    continue;
                }
                if f.scaled() {
                    csv.push_str(&fmt_scaled(raw as f64 * f.scale));
                } else {
                    let _ = write!(csv, "{raw}");
                }
            }
        }
        csv.push_str("\r\n");
    }
    Ok(Decoded {
        header,
        csv,
        records,
        torn_bytes,
    })
}

// ---- Saving a pulled file ----

/// Never clobber an existing file: `LOG0001.CSV` → `LOG0001_2.CSV`. The
/// counter goes before the extension so the copy still opens in a
/// spreadsheet.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let base = dir.join(name);
    if !base.exists() {
        return base;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) => (stem, format!(".{ext}")),
        None => (name, String::new()),
    };
    for n in 2..1000 {
        let candidate = dir.join(format!("{stem}_{n}{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    base
}

/// Write to `<path>.part`, flush it to the disk, then rename: a crash or a
/// yanked USB disk leaves a `.part`, never a truncated file under the real
/// name.
pub fn write_atomically(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&part)?;
        f.write_all(data)?;
        f.sync_all()?;
        std::fs::rename(&part, path)
    };
    write().inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })
}

/// The CSV written next to a binary log.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedFile {
    pub path: PathBuf,
    pub records: usize,
    pub torn_bytes: usize,
}

/// What [`save_pulled`] did.
#[derive(Debug)]
pub struct SavedLog {
    /// The file exactly as pulled (CRC-verified).
    pub path: PathBuf,
    /// `None` for a file that isn't a binary log (e.g. `LOGnnnn.CSV`);
    /// otherwise the decoded CSV, or why decoding failed.
    pub decoded: Option<Result<DecodedFile, String>>,
}

/// Decode a binary log whose bytes are `data` into `<stem>.csv` next to
/// `bin_path` (`CEL0003.BIN` → `CEL0003.csv`, never clobbering). `None` if
/// `data` isn't a binary log.
pub fn decode_beside(bin_path: &Path, data: &[u8]) -> Option<Result<DecodedFile, String>> {
    if !is_bin_log(data) {
        return None;
    }
    Some((|| {
        let decoded = decode(data).map_err(|e| e.to_string())?;
        let dir = bin_path.parent().unwrap_or(Path::new("."));
        let stem = bin_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "decoded".to_string());
        let path = unique_path(dir, &format!("{stem}.csv"));
        write_atomically(&path, decoded.csv.as_bytes())
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(DecodedFile {
            path,
            records: decoded.records,
            torn_bytes: decoded.torn_bytes,
        })
    })())
}

/// Save a pulled file into `dir` under `name` (never clobbering), then, if
/// it is a binary log, write the decoded CSV beside it. Only the save of the
/// pulled bytes can fail; a decode problem is reported in
/// [`SavedLog::decoded`] and the original is kept.
pub fn save_pulled(dir: &Path, name: &str, data: &[u8]) -> std::io::Result<SavedLog> {
    std::fs::create_dir_all(dir)?;
    let path = unique_path(dir, name);
    write_atomically(&path, data)?;
    let decoded = decode_beside(&path, data);
    Ok(SavedLog { path, decoded })
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMU_BIN: &[u8] = include_bytes!("../tests/fixtures/log_decode/IMU0003.BIN");
    const IMU_CSV: &str = include_str!("../tests/fixtures/log_decode/IMU0003.csv");
    const CEL_BIN: &[u8] = include_bytes!("../tests/fixtures/log_decode/CEL0003.BIN");
    const CEL_CSV: &str = include_str!("../tests/fixtures/log_decode/CEL0003.csv");

    const IMU_SCHEMA: &str =
        "tick_ms u32 1 1 ms\na i16 3 6/32768 g\ng i16 3 8.726646259971648/32768 rad/s\n";

    /// A header for `schema` with `record_size`, the way the AMS writes it.
    fn header(stream: &str, record_size: u16, schema: &str) -> Vec<u8> {
        let mut h = vec![0u8; HEADER_LEN];
        h[..8].copy_from_slice(MAGIC);
        h[8..10].copy_from_slice(&1u16.to_le_bytes());
        h[10..12].copy_from_slice(&record_size.to_le_bytes());
        h[12..16].copy_from_slice(&3u32.to_le_bytes());
        h[16..20].copy_from_slice(&1000u32.to_le_bytes());
        h[20..20 + stream.len()].copy_from_slice(stream.as_bytes());
        h[64..64 + schema.len()].copy_from_slice(schema.as_bytes());
        h
    }

    fn imu_record(tick: u32, a: [i16; 3], g: [i16; 3]) -> Vec<u8> {
        let mut r = tick.to_le_bytes().to_vec();
        for v in a.iter().chain(g.iter()) {
            r.extend_from_slice(&v.to_le_bytes());
        }
        r
    }

    #[test]
    fn header_fields_parse() {
        let h = read_header(IMU_BIN).unwrap();
        assert_eq!(h.version, 1);
        assert_eq!(h.record_size, 16);
        assert_eq!(h.rotation, 3);
        assert_eq!(h.open_tick_ms, 1000);
        assert_eq!(h.stream, "IMU");
        assert_eq!(h.fw_version, [3, 4, 5]);
        assert_eq!(h.git_hash, [0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(h.schema.len(), 3);
    }

    #[test]
    fn schema_parses_ratios_grids_and_flags() {
        let f = parse_schema("a i16 3 6/32768 g\nc u16 5x19 1 mV z\n\n i i32 1 0.001 A\n").unwrap();
        assert_eq!(f[0].dims, vec![3]);
        assert_eq!(f[0].scale, 6.0 / 32768.0);
        assert_eq!(f[1].dims, vec![5, 19]);
        assert_eq!(f[1].count(), 95);
        assert!(f[1].zero_empty);
        assert_eq!(f[1].columns()[0], "c0_0");
        assert_eq!(f[1].columns()[94], "c4_18");
        assert_eq!(f[2].dims, Vec::<usize>::new());
        assert_eq!(f[2].columns(), vec!["i_A"]);
        // A scale of 1 adds no unit; a "-" unit never does.
        assert_eq!(
            parse_schema("t u32 1 1 ms").unwrap()[0].columns(),
            vec!["t"]
        );
        assert_eq!(
            parse_schema("x i8 2 0.5 -").unwrap()[0].columns(),
            vec!["x0", "x1"]
        );
        assert_eq!(
            parse_schema("g i16 1 2 rad/s").unwrap()[0].columns(),
            vec!["g_rad_s"]
        );
    }

    #[test]
    fn malformed_schema_lines_are_rejected() {
        for bad in [
            "a f32 1 1 g",
            "a i16 1 1",
            "a i16 x 1 g",
            "a i16 2x 1 g",
            "a i16 1x2x3 1 g",
            "a i16 0 1 g",
            "a i16 1 1/0 g",
            "a i16 1 abc g",
        ] {
            assert!(
                matches!(parse_schema(bad), Err(DecodeError::BadSchemaLine { .. })),
                "{bad:?} should be rejected"
            );
        }
        assert_eq!(parse_schema("\n  \n"), Err(DecodeError::EmptySchema));
    }

    #[test]
    fn record_size_mismatch_is_rejected() {
        let mut bytes = header("IMU", 18, IMU_SCHEMA);
        bytes.extend(imu_record(1, [0; 3], [0; 3]));
        assert_eq!(
            decode(&bytes).unwrap_err(),
            DecodeError::RecordSizeMismatch {
                schema: 16,
                header: 18
            }
        );
    }

    #[test]
    fn bad_magic_and_short_header_are_rejected() {
        assert_eq!(read_header(b"tick_ms,a0\r\n"), Err(DecodeError::BadMagic));
        assert_eq!(read_header(MAGIC), Err(DecodeError::ShortHeader(8)));
        assert!(!is_bin_log(b"LOG"));
    }

    /// The vector from #613.
    #[test]
    fn imu_vector_from_the_issue() {
        let mut bytes = header("IMU", 16, IMU_SCHEMA);
        bytes.extend(imu_record(1234, [16384, 0, -32768], [1, -1, -32768]));
        let d = decode(&bytes).unwrap();
        assert_eq!(
            d.csv,
            "tick_ms,a0_g,a1_g,a2_g,g0_rad_s,g1_rad_s,g2_rad_s\r\n\
             1234,3,0,-6,0.000266,-0.000266,-8.726646\r\n"
        );
        assert_eq!((d.records, d.torn_bytes), (1, 0));
    }

    /// Byte-identical to the AMS reference decoder (`tools/log_decode.py`)
    /// on the same files — the fixtures are its output.
    #[test]
    fn matches_the_reference_decoder() {
        assert_eq!(decode(IMU_BIN).unwrap().csv, IMU_CSV);
        let cel = decode(CEL_BIN).unwrap();
        assert_eq!(cel.csv, CEL_CSV);
        // Two whole records; the 3-byte torn tail is dropped.
        assert_eq!((cel.records, cel.torn_bytes), (2, 3));
    }

    /// The CEL fixture's second record has the second IC's 19 cells at 0:
    /// flag `z` turns them into empty cells, and nothing else.
    #[test]
    fn zero_flag_empties_unmeasured_cells() {
        let cel = decode(CEL_BIN).unwrap();
        let rows: Vec<&str> = cel.csv.split("\r\n").collect();
        let row2: Vec<&str> = rows[2].split(',').collect();
        let cols: Vec<&str> = rows[0].split(',').collect();
        let at = |name: &str| row2[cols.iter().position(|c| *c == name).unwrap()];
        assert_eq!(at("c0_18"), "3718");
        assert!((0..19).all(|j| at(&format!("c1_{j}")).is_empty()));
        assert_eq!(at("c2_0"), "3738");
        assert_eq!(at("i_A"), "0.25");
    }

    #[test]
    fn torn_tail_keeps_every_whole_record() {
        let mut bytes = header("IMU", 16, IMU_SCHEMA);
        bytes.extend(imu_record(1, [0; 3], [0; 3]));
        bytes.extend(imu_record(2, [0; 3], [0; 3]));
        bytes.extend(&imu_record(3, [0; 3], [0; 3])[..7]);
        let d = decode(&bytes).unwrap();
        assert_eq!((d.records, d.torn_bytes), (2, 7));
        assert_eq!(d.csv.lines().count(), 3);
    }

    #[test]
    fn scaled_values_format_like_the_reference() {
        assert_eq!(fmt_scaled(3.0), "3");
        assert_eq!(fmt_scaled(-12.345), "-12.345");
        assert_eq!(fmt_scaled(0.25), "0.25");
        assert_eq!(fmt_scaled(-0.0000004), "0");
        assert_eq!(fmt_scaled(100.0), "100");
        assert_eq!(fmt_scaled(-8.726646259971648), "-8.726646");
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("mingocan-log-decode-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn save_pulled_writes_the_bin_and_its_csv() {
        let dir = temp_dir("save");
        let saved = save_pulled(&dir, "CEL0003.BIN", CEL_BIN).unwrap();
        assert_eq!(saved.path, dir.join("CEL0003.BIN"));
        assert_eq!(std::fs::read(&saved.path).unwrap(), CEL_BIN);
        let csv = saved.decoded.unwrap().unwrap();
        assert_eq!(csv.path, dir.join("CEL0003.csv"));
        assert_eq!(std::fs::read_to_string(&csv.path).unwrap(), CEL_CSV);
        assert_eq!(csv.records, 2);

        // A second pull of the same file clobbers neither.
        let again = save_pulled(&dir, "CEL0003.BIN", CEL_BIN).unwrap();
        assert_eq!(again.path, dir.join("CEL0003_2.BIN"));
        assert_eq!(
            again.decoded.unwrap().unwrap().path,
            dir.join("CEL0003_2.csv")
        );
        assert!(!dir.join("CEL0003.BIN.part").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_bad_schema_keeps_the_bin_and_reports_why() {
        let dir = temp_dir("bad");
        let mut bytes = header("CEL", 9, "x u8 1 1 -\n");
        bytes.extend([1, 2, 3]);
        let saved = save_pulled(&dir, "CEL0001.BIN", &bytes).unwrap();
        assert_eq!(std::fs::read(&saved.path).unwrap(), bytes);
        let err = saved.decoded.unwrap().unwrap_err();
        assert!(err.contains("schema describes 1 B"), "{err}");
        assert!(!dir.join("CEL0001.csv").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn non_bin_files_are_saved_untouched() {
        let dir = temp_dir("csv");
        let data = b"tick_ms,soc\n1,99\n";
        let saved = save_pulled(&dir, "LOG0001.CSV", data).unwrap();
        assert!(saved.decoded.is_none());
        assert_eq!(std::fs::read(&saved.path).unwrap(), data);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
