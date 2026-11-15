//! Flash log (#651): an append-only record of every action that writes
//! to a board — who flashed what onto which board, when, and whether it
//! worked.
//!
//! One JSON object per line (JSONL) in a per-user file, by default
//! `<data-local-dir>/mingocan/flash-log.jsonl`:
//! - macOS `~/Library/Application Support/mingocan/`
//! - Linux `$XDG_DATA_HOME` or `~/.local/share/mingocan/`
//! - Windows `%LOCALAPPDATA%\mingocan\`
//!
//! The CLI (`--log` / `--operator`) and the desktop app both append to
//! the same file, so one laptop has one history whichever tool it used.
//! Writing the log never fails the action it records: an I/O error is a
//! warning, not an error.
//!
//! Lines are appended with a single `write` on a file opened in append
//! mode, so concurrent writers (CLI + app) don't interleave a line.
//! Readers skip lines they can't parse, so a torn or hand-edited line
//! costs that line, not the history.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Bumped when a field changes meaning. Adding optional fields doesn't.
pub const SCHEMA_VERSION: u32 = 1;

/// File name inside the per-user `mingocan` data directory.
pub const FILE_NAME: &str = "flash-log.jsonl";

/// One logged action. Every field but the identity/result ones is
/// optional: a failure can happen before the artifact is parsed or the
/// board answers, and the line still records what was known.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// [`SCHEMA_VERSION`] at write time.
    pub v: u32,
    /// UTC, RFC 3339 with milliseconds (`2026-11-15T19:42:07.123Z`).
    pub ts: String,
    /// `--operator`, the app's operator setting, or the OS user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
    /// Machine name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// `"cli"` or `"app"`.
    pub tool: String,
    /// MingoCAN version that wrote the line.
    pub tool_version: String,
    /// What was done: `flash`, `swd-flash`, `swd-erase`, `dfu-flash`,
    /// `provision`, `ob-apply-wrp`, `nvm-write`, `nvm-erase`,
    /// `nvm-format`, `clear-dtc`.
    pub action: String,

    /// The CAN node the action addressed, or the node-id a bootloader
    /// burn provisioned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<u8>,
    /// `ecu` / `ams` / `udv` for the standard node-ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// CAN adapter backend (`slcan`, `pcan`, …) and channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// USB serial of the DFU board or the SWD probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_serial: Option<String>,

    /// Firmware file as given (absolute when known).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    /// CRC32 of the bytes written, `0x%08X`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crc32: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// From the image's `__firmware_info` record, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fw_product: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fw_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fw_git_hash: Option<String>,
    /// Whether the written image was read back and matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    /// A dry run touched nothing; logged so the history shows it ran.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
    /// Action-specific extras (NVM key, chip name, sector counts, …).
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub details: serde_json::Map<String, serde_json::Value>,

    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The CLI's exit code on failure (see `docs/CLI.md` § Exit codes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<u8>,
}

impl Entry {
    /// A fresh entry stamped with the time, host, user and version.
    /// `tool` is `"cli"` or `"app"`.
    pub fn new(tool: &str, action: &str) -> Self {
        Self {
            v: SCHEMA_VERSION,
            ts: rfc3339_utc(SystemTime::now()),
            operator: os_user(),
            host: host_name(),
            tool: tool.to_string(),
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            action: action.to_string(),
            ..Self::default()
        }
    }

    /// Set the node-id and, for the standard boards, its role.
    pub fn set_node(&mut self, node_id: u8) {
        self.node_id = Some(node_id);
        self.role = role_name(node_id).map(str::to_string);
    }

    /// Record the parsed firmware image: CRC32, size and, when the image
    /// carries a `__firmware_info` record, product, version and git hash.
    pub fn set_image(&mut self, image: &crate::firmware::Image) {
        self.crc32 = Some(format!("0x{:08X}", image.crc32()));
        self.size_bytes = Some(image.data.len() as u64);
        if let Some(fw) = &image.fw_info {
            let (major, minor, patch) = fw.version();
            self.fw_version = Some(format!("{major}.{minor}.{patch}"));
            let product = fw.product_name();
            if !product.is_empty() {
                self.fw_product = Some(product.to_string());
            }
            self.fw_git_hash = Some(fw.git_hash.iter().map(|b| format!("{b:02x}")).collect());
        }
    }

    /// Set the artifact path, made absolute when it resolves.
    pub fn set_artifact(&mut self, path: &Path) {
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.artifact = Some(abs.display().to_string());
    }

    /// Add one action-specific extra.
    pub fn detail(&mut self, key: &str, value: impl Into<serde_json::Value>) {
        self.details.insert(key.to_string(), value.into());
    }

    /// Mark the outcome. `exit_code` is the CLI's, when there is one.
    pub fn finish(&mut self, error: Option<String>, exit_code: Option<u8>) {
        self.ok = error.is_none();
        self.error = error;
        self.exit_code = if self.ok { None } else { exit_code };
    }
}

/// `ecu` / `ams` / `udv` for node-ids 0x1 / 0x2 / 0x3.
pub fn role_name(node_id: u8) -> Option<&'static str> {
    match node_id {
        0x1 => Some("ecu"),
        0x2 => Some("ams"),
        0x3 => Some("udv"),
        _ => None,
    }
}

/// The per-user log file, or `None` when the platform has no data dir.
pub fn default_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("mingocan").join(FILE_NAME))
}

/// Append one entry as a single line, creating the file and its
/// directory if needed.
pub fn append(path: &Path, entry: &Entry) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_vec(entry).map_err(std::io::Error::other)?;
    line.push(b'\n');
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    // One write per line: O_APPEND keeps concurrent writers' lines whole.
    file.write_all(&line)
}

/// Append, turning a failure into a warning — the log must never fail
/// the action it records.
pub fn append_or_warn(path: &Path, entry: &Entry) {
    if let Err(e) = append(path, entry) {
        tracing::warn!("couldn't write the flash log {}: {e}", path.display());
    }
}

/// Every parseable entry in the file, oldest first. A missing file is an
/// empty history; lines that don't parse are skipped.
pub fn read(path: &Path) -> std::io::Result<Vec<Entry>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

/// The entries as CSV (header + one row each), for spreadsheets.
pub fn to_csv(entries: &[Entry]) -> String {
    const HEADER: [&str; 21] = [
        "ts",
        "operator",
        "host",
        "tool",
        "toolVersion",
        "action",
        "nodeId",
        "role",
        "interface",
        "channel",
        "deviceSerial",
        "artifact",
        "crc32",
        "sizeBytes",
        "fwProduct",
        "fwVersion",
        "fwGitHash",
        "verified",
        "dryRun",
        "ok",
        "error",
    ];
    let mut out = HEADER.join(",");
    out.push('\n');
    for e in entries {
        let opt = |v: &Option<String>| v.clone().unwrap_or_default();
        let fields = [
            e.ts.clone(),
            opt(&e.operator),
            opt(&e.host),
            e.tool.clone(),
            e.tool_version.clone(),
            e.action.clone(),
            e.node_id.map(|n| format!("0x{n:X}")).unwrap_or_default(),
            opt(&e.role),
            opt(&e.interface),
            opt(&e.channel),
            opt(&e.device_serial),
            opt(&e.artifact),
            opt(&e.crc32),
            e.size_bytes.map(|n| n.to_string()).unwrap_or_default(),
            opt(&e.fw_product),
            opt(&e.fw_version),
            opt(&e.fw_git_hash),
            e.verified.map(|b| b.to_string()).unwrap_or_default(),
            e.dry_run.to_string(),
            e.ok.to_string(),
            opt(&e.error),
        ];
        let row: Vec<String> = fields.iter().map(|f| csv_field(f)).collect();
        out.push_str(&row.join(","));
        out.push('\n');
    }
    out
}

/// Quote a CSV field when it needs it (RFC 4180). A leading `=+-@` is
/// prefixed with `'` so a spreadsheet doesn't run it as a formula.
fn csv_field(raw: &str) -> String {
    let s = if raw.starts_with(['=', '+', '-', '@']) {
        format!("'{raw}")
    } else {
        raw.to_string()
    };
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

fn os_user() -> Option<String> {
    ["USER", "USERNAME", "LOGNAME"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn host_name() -> Option<String> {
    let h = gethostname::gethostname()
        .to_string_lossy()
        .trim()
        .to_string();
    (!h.is_empty()).then_some(h)
}

/// RFC 3339 UTC with milliseconds, without a date-time dependency.
pub fn rfc3339_utc(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let (days, sod) = (secs / 86_400, secs % 86_400);
    let (y, m, day) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60,
        d.subsec_millis()
    )
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's
/// `civil_from_days`, proleptic Gregorian.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn temp_file(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mingocan-audit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("nested").join(FILE_NAME)
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(rfc3339_utc(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        // 2000-02-29 (leap day) 12:34:56.789
        let t = UNIX_EPOCH + Duration::from_millis(951_827_696_789);
        assert_eq!(rfc3339_utc(t), "2000-02-29T12:34:56.789Z");
        let t = UNIX_EPOCH + Duration::from_secs(1_794_758_400);
        assert_eq!(rfc3339_utc(t), "2026-11-15T16:00:00.000Z");
    }

    #[test]
    fn append_then_read_round_trips_and_skips_junk() {
        let path = temp_file("roundtrip");
        let mut a = Entry::new("cli", "flash");
        a.set_node(0x2);
        a.crc32 = Some("0xDEADBEEF".into());
        a.detail("sectorsWritten", 3);
        a.finish(None, None);
        append(&path, &a).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{not json\n\n")
            .unwrap();
        let mut b = Entry::new("app", "provision");
        b.finish(Some("board didn't answer".into()), Some(4));
        append(&path, &b).unwrap();

        let got = read(&path).unwrap();
        assert_eq!(got, vec![a.clone(), b.clone()]);
        assert_eq!(got[0].role.as_deref(), Some("ams"));
        assert!(got[0].ok && !got[1].ok);
        assert_eq!(got[1].exit_code, Some(4));
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn a_missing_file_is_an_empty_history() {
        assert!(read(&temp_file("missing")).unwrap().is_empty());
    }

    #[test]
    fn success_drops_the_exit_code_and_lines_stay_compact() {
        let mut e = Entry::new("cli", "clear-dtc");
        e.finish(None, Some(1));
        assert_eq!(e.exit_code, None);
        let line = serde_json::to_string(&e).unwrap();
        assert!(
            !line.contains("details") && !line.contains("dryRun"),
            "{line}"
        );
        assert!(line.contains(r#""action":"clear-dtc""#));
    }

    #[test]
    fn csv_quotes_and_defuses_formulas() {
        let mut e = Entry::new("cli", "flash");
        e.artifact = Some(r#"/tmp/a,"b".elf"#.into());
        e.error = Some("=HYPERLINK(1)".into());
        let csv = to_csv(&[e]);
        let row = csv.lines().nth(1).unwrap();
        assert!(row.contains(r#""/tmp/a,""b"".elf""#), "{row}");
        assert!(row.contains("'=HYPERLINK(1)"), "{row}");
        assert_eq!(csv.lines().next().unwrap().split(',').count(), 21);
    }
}
