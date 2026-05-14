// Tauri command that wraps the full can-flasher flash pipeline.
//
// The orchestration mirrors `can_flasher::cli::flash`'s `run`
// function in miniature: load firmware → open backend → attach
// session → connect → optionally run a build command → drive
// FlashManager → optional JUMP. FlashManager's progress events
// stream to the frontend over Tauri's `Emitter::emit` API as
// `flash:event` payloads; the matching FlashView.svelte listener
// turns them into a phase-aware progress indicator.
//
// Build-step output (stdout/stderr of the user-configured shell
// command) is also forwarded as `flash:event` payloads with
// `kind: "build_line"` so the UI shows compilation output in real
// time.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::process::Command;
use tokio::sync::mpsc;

use can_flasher::cli::InterfaceType;
use can_flasher::firmware::{self, loader};
use can_flasher::flash::{FlashConfig, FlashEvent, FlashManager, FlashReport, SectorRole};
use can_flasher::protocol::commands::cmd_jump;
use can_flasher::session::{Session, SessionConfig};
use can_flasher::transport::open_backend;

// ---- Request from frontend ----

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlashRequest {
    /// Path to .elf / .hex / .bin firmware artifact.
    pub artifact_path: String,
    /// Optional pre-flash shell command (`cmake --build build`,
    /// `west build -t flash-elf`, etc.). Skipped when `null` or
    /// empty.
    pub build_command: Option<String>,
    /// Working directory for the build command. Falls back to the
    /// parent of `artifact_path` when omitted.
    pub build_cwd: Option<String>,

    /// Adapter selection — mirrors the CLI's `--interface` and
    /// `--channel`.
    pub interface: String,
    pub channel: Option<String>,
    pub bitrate: u32,
    /// Target node ID — `null` means broadcast to default 0x3.
    pub node_id: Option<u8>,

    /// Per-frame timeout in milliseconds.
    pub timeout_ms: u32,
    /// Session keepalive interval.
    pub keepalive_ms: u32,

    /// Honour the on-device CRC-diff and skip matching sectors.
    pub diff: bool,
    /// Walk the pipeline but send no erases / writes.
    pub dry_run: bool,
    /// Post-write CRC verification per sector.
    pub verify_after: bool,
    /// Fire the final `CMD_FLASH_VERIFY` commit.
    pub final_commit: bool,
    /// Jump to the application after a successful flash.
    pub jump: bool,
}

// ---- Events emitted to frontend ----

/// Mirror of `can_flasher::flash::FlashEvent` plus build-step
/// stdout/stderr lines. The Rust enum doesn't derive Serialize, so
/// we keep this parallel discriminated-union shape and From-impl
/// the conversion at emit time.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FlashStreamEvent {
    BuildLine { stream: &'static str, text: String },
    BuildExited { code: Option<i32> },
    Planning { sector: u8, role: &'static str },
    Erased { sector: u8 },
    Written { sector: u8, bytes: u32, total: u32 },
    Verified { sector: u8, crc: String },
    Committing,
    Done { report: JsonReport },
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonReport {
    pub sectors_erased: Vec<u8>,
    pub sectors_written: Vec<u8>,
    pub sectors_skipped: Vec<u8>,
    pub crc32: String,
    pub size: u32,
    pub version: u32,
    pub duration_ms: u128,
}

impl From<&FlashReport> for JsonReport {
    fn from(r: &FlashReport) -> Self {
        Self {
            sectors_erased: r.sectors_erased.clone(),
            sectors_written: r.sectors_written.clone(),
            sectors_skipped: r.sectors_skipped.clone(),
            crc32: format!("0x{:08X}", r.crc32),
            size: r.size,
            version: r.version,
            duration_ms: r.duration.as_millis(),
        }
    }
}

fn convert_event(e: &FlashEvent) -> FlashStreamEvent {
    match e {
        FlashEvent::PlanningSector { sector, role } => FlashStreamEvent::Planning {
            sector: *sector,
            role: match role {
                SectorRole::Skip => "skip",
                SectorRole::Write => "write",
            },
        },
        FlashEvent::Erased { sector } => FlashStreamEvent::Erased { sector: *sector },
        FlashEvent::ChunkWritten {
            sector,
            bytes,
            total,
        } => FlashStreamEvent::Written {
            sector: *sector,
            bytes: *bytes,
            total: *total,
        },
        FlashEvent::SectorVerified { sector, crc } => FlashStreamEvent::Verified {
            sector: *sector,
            crc: format!("0x{:08X}", crc),
        },
        FlashEvent::Committing => FlashStreamEvent::Committing,
        FlashEvent::Done { report } => FlashStreamEvent::Done {
            report: report.into(),
        },
    }
}

// ---- Command entry point ----

const EVENT_NAME: &str = "flash:event";

#[tauri::command]
pub async fn flash(app: AppHandle, request: FlashRequest) -> Result<JsonReport, String> {
    // ---- 1. Build step ----

    if let Some(cmd) = request.build_command.as_deref() {
        let cmd = cmd.trim();
        if !cmd.is_empty() {
            let cwd = resolve_build_cwd(&request);
            run_build(&app, cmd, cwd).await?;
        }
    }

    // ---- 2. Load firmware ----

    let path = PathBuf::from(&request.artifact_path);
    let image = loader::load(&path, None).map_err(|e| format!("load firmware: {e}"))?;
    image
        .validate_fits_app_region()
        .map_err(|e| format!("firmware doesn't fit app region: {e}"))?;
    if image.base_addr != firmware::BL_APP_BASE {
        return Err(format!(
            "firmware base 0x{:08X} ≠ BL_APP_BASE (0x{:08X})",
            image.base_addr,
            firmware::BL_APP_BASE,
        ));
    }

    // ---- 3. Open backend + session ----

    let interface = parse_interface(&request.interface).map_err(|e| format!("interface: {e}"))?;
    let channel = request.channel.as_deref();
    let backend = open_backend(interface, channel, request.bitrate)
        .map_err(|e| format!("opening backend: {e}"))?;
    let target_node = request.node_id.unwrap_or(0x3);
    let session = Session::attach(
        backend,
        SessionConfig {
            target_node,
            keepalive_interval: Duration::from_millis(u64::from(request.keepalive_ms)),
            command_timeout: Duration::from_millis(u64::from(request.timeout_ms)),
            ..SessionConfig::default()
        },
    );

    // ---- 4. Connect ----

    let _proto = session
        .connect()
        .await
        .map_err(|e| format!("CONNECT failed: {e}"))?;

    // ---- 5. FlashManager ----

    let config = FlashConfig {
        diff: request.diff,
        dry_run: request.dry_run,
        verify_after: request.verify_after,
        write_chunk_size: can_flasher::flash::DEFAULT_WRITE_CHUNK,
        final_commit: request.final_commit && !request.dry_run,
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<FlashEvent>();
    let app_for_events = app.clone();
    let forward_task = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let payload = convert_event(&ev);
            let _ = app_for_events.emit(EVENT_NAME, &payload);
        }
    });

    let manager = FlashManager::new(&session, &image, config);
    let report = manager
        .run(Some(tx))
        .await
        .map_err(|e| format!("flash failed: {e}"))?;
    drop(forward_task); // event channel closed; forward task exits.

    // ---- 6. Optional JUMP ----

    if request.jump && !request.dry_run {
        let _ = session.send_command(&cmd_jump(firmware::BL_APP_BASE)).await;
        // Fire-and-forget: the device resets and ACK delivery is
        // best-effort post-reset. Surfacing a stray timeout here
        // would be confusing UX after a successful flash.
    }

    let _ = session.disconnect().await;

    Ok((&report).into())
}

/// Run *only* the build step. Same shell semantics as the build
/// phase of `flash` (login-shell on Unix so Homebrew PATH is
/// visible, `cmd /c` on Windows), same `flash:event` build-line
/// stream — but the IPC call returns the moment the build process
/// exits, with no firmware load / no adapter open / no flashing.
///
/// Why it exists: configure-from-scratch CMake projects need to
/// produce the `build/` directory + the artifact before flashing
/// is even possible. Without a "build only" path, operators were
/// forced to drop to a terminal for the very first run, which
/// defeats the purpose of a desktop app.
#[tauri::command]
pub async fn build_only(
    app: AppHandle,
    command: String,
    cwd: Option<String>,
) -> Result<(), String> {
    let cmd = command.trim();
    if cmd.is_empty() {
        return Err("build command is empty".into());
    }
    let cwd = match cwd.as_deref().map(str::trim) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from("."),
    };
    run_build(&app, cmd, cwd).await
}

// ---- Build step ----

fn resolve_build_cwd(request: &FlashRequest) -> PathBuf {
    if let Some(cwd) = request.build_cwd.as_deref() {
        if !cwd.trim().is_empty() {
            return PathBuf::from(cwd);
        }
    }
    PathBuf::from(&request.artifact_path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

async fn run_build(app: &AppHandle, command: &str, cwd: PathBuf) -> Result<(), String> {
    let _ = app.emit(
        EVENT_NAME,
        &FlashStreamEvent::BuildLine {
            stream: "info",
            text: format!("$ {command}"),
        },
    );

    // Apps launched from a macOS GUI (Finder / Dock / Spotlight)
    // inherit a *minimal* environment — PATH typically only contains
    // /usr/bin:/bin:/usr/sbin:/sbin. Operator-installed tools like
    // cmake / west / arm-none-eabi-gcc live under Homebrew's
    // /opt/homebrew/bin or /usr/local/bin, which aren't on that
    // PATH. Running the build command through `/bin/sh -c` would
    // then fail with "command not found".
    //
    // Spawning the operator's login shell (`$SHELL -lc`) sources
    // their shell rc / profile (~/.zprofile / ~/.bash_profile),
    // which is where Homebrew's shellenv normally lives — so PATH
    // matches what the operator sees in Terminal.
    //
    // Linux GUI sessions usually inherit a fuller environment, but
    // the same approach is robust there too. Windows keeps the
    // `cmd /c` shape it had before.
    let (program, args): (String, Vec<String>) = if cfg!(target_os = "windows") {
        ("cmd.exe".into(), vec!["/c".into(), command.to_string()])
    } else {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if cfg!(target_os = "macos") {
                    "/bin/zsh".into()
                } else {
                    "/bin/bash".into()
                }
            });
        (shell, vec!["-lc".into(), command.to_string()])
    };

    let mut child = Command::new(&program)
        .args(&args)
        .current_dir(&cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn build: {e}"))?;

    use tokio::io::{AsyncBufReadExt, BufReader};
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let app_out = app.clone();
    let app_err = app.clone();

    let out_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = app_out.emit(
                EVENT_NAME,
                &FlashStreamEvent::BuildLine {
                    stream: "stdout",
                    text: line,
                },
            );
        }
    });
    let err_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = app_err.emit(
                EVENT_NAME,
                &FlashStreamEvent::BuildLine {
                    stream: "stderr",
                    text: line,
                },
            );
        }
    });

    let status = child.wait().await.map_err(|e| format!("wait build: {e}"))?;
    let _ = tokio::join!(out_task, err_task);

    let _ = app.emit(
        EVENT_NAME,
        &FlashStreamEvent::BuildExited {
            code: status.code(),
        },
    );

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "build command exited with code {:?}",
            status.code()
        ))
    }
}

// ---- Interface parsing ----

fn parse_interface(s: &str) -> Result<InterfaceType, String> {
    match s {
        "slcan" => Ok(InterfaceType::Slcan),
        "socketcan" => Ok(InterfaceType::Socketcan),
        "pcan" => Ok(InterfaceType::Pcan),
        "vector" => Ok(InterfaceType::Vector),
        "virtual" => Ok(InterfaceType::Virtual),
        other => Err(format!("unknown interface: {other}")),
    }
}
