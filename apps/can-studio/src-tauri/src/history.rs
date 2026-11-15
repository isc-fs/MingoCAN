// Flash history (#651): the app's side of the flash log.
//
// Every command that writes to a board (flash, burn bootloader over SWD
// or USB, chip erase, provision, clear DTCs) appends one entry to the
// same per-user JSONL file the CLI writes — `can_flasher::audit` owns
// the format and the path. The History view reads it back.
//
// Writing the log never fails the action: errors are warnings.

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

use can_flasher::audit::{self, Entry};

/// Start an entry for `action`, stamped with the operator from Settings
/// (falling back to the OS user).
pub fn begin(app: &AppHandle, action: &str) -> Entry {
    let mut entry = Entry::new("app", action);
    if let Some(op) = operator(app) {
        entry.operator = Some(op);
    }
    entry
}

/// Record the CAN target of `entry`.
pub fn set_can(entry: &mut Entry, interface: &str, channel: Option<&str>, node_id: Option<u8>) {
    entry.interface = Some(interface.to_string());
    entry.channel = channel.map(str::to_string).filter(|c| !c.is_empty());
    if let Some(id) = node_id {
        entry.set_node(id);
    }
}

/// Close `entry` with the command's result and append it. The virtual
/// bus is a demo/test target and stays out of the history.
pub fn finish<T>(mut entry: Entry, result: &Result<T, String>) {
    if entry.interface.as_deref() == Some("virtual") {
        return;
    }
    let Some(path) = audit::default_path() else {
        return;
    };
    entry.finish(result.as_ref().err().cloned(), None);
    audit::append_or_warn(&path, &entry);
}

/// `history.operator` from the settings the frontend saved, if set.
fn operator(app: &AppHandle) -> Option<String> {
    let store = app.get_store("settings.json")?;
    let all = store.get("all")?;
    let op = all.get("history")?.get("operator")?.as_str()?.trim();
    (!op.is_empty()).then(|| op.to_string())
}

// ---- Commands ----

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryDto {
    /// Where the log lives (may not exist yet).
    pub path: Option<String>,
    /// Newest first.
    pub entries: Vec<Entry>,
}

#[tauri::command]
pub fn history_read() -> Result<HistoryDto, String> {
    let Some(path) = audit::default_path() else {
        return Ok(HistoryDto {
            path: None,
            entries: Vec::new(),
        });
    };
    let mut entries = audit::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    entries.reverse();
    Ok(HistoryDto {
        path: Some(path.display().to_string()),
        entries,
    })
}

/// Write the whole history (or the rows the view is showing, when the
/// frontend passes them) as CSV to `path`. Returns the row count.
#[tauri::command]
pub fn history_export_csv(path: String, entries: Option<Vec<Entry>>) -> Result<usize, String> {
    let entries = match entries {
        Some(e) => e,
        None => history_read()?.entries,
    };
    std::fs::write(&path, audit::to_csv(&entries)).map_err(|e| format!("write {path}: {e}"))?;
    Ok(entries.len())
}
