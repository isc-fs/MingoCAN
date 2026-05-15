//! Download the prebuilt CAN bootloader from the
//! [isc-fs/stm32-can-bootloader] release page and cache it locally,
//! so operators don't have to clone the BL repo just to get the
//! `.elf` they need to first-boot an STM32.
//!
//! Hits the GitHub Releases API to resolve `latest` (or a pinned
//! tag) to its asset list, picks the right artefact by extension
//! (`CAN_BL.elf` / `.hex` / `.bin`), and downloads to a
//! platform-appropriate cache dir:
//!
//! - **Linux**:   `$XDG_CACHE_HOME/can-flasher/bootloaders/<tag>/`
//! - **macOS**:   `~/Library/Caches/can-flasher/bootloaders/<tag>/`
//! - **Windows**: `%LOCALAPPDATA%\can-flasher\bootloaders\<tag>\`
//!
//! Tagged releases are immutable, so once an asset is on disk under
//! a specific tag we never re-hit the network. The `latest` alias
//! still calls the API to resolve which tag is current, but skips
//! the download when the resolved tag is already cached.
//!
//! Behind the `swd` Cargo feature — same gating as the rest of the
//! SWD path. The CLI `flash` subcommand (over-CAN) deliberately
//! doesn't use this yet; that's a follow-up if operators want it.
//!
//! [isc-fs/stm32-can-bootloader]: https://github.com/isc-fs/stm32-can-bootloader

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;
use tracing::{debug, info};

/// `owner/repo` slug for the BL we fetch from. Hard-coded —
/// switching to a different BL would be a fork, not a flag.
pub const BOOTLOADER_REPO: &str = "isc-fs/stm32-can-bootloader";

/// Format of the bootloader artefact to fetch. The BL repo
/// publishes all three (`CAN_BL.elf`, `CAN_BL.hex`, `CAN_BL.bin`)
/// on every tagged release from v1.2.0 onward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootloaderFormat {
    Elf,
    Hex,
    Bin,
}

impl BootloaderFormat {
    /// Filename the BL repo uses for this format on its releases.
    pub const fn asset_name(self) -> &'static str {
        match self {
            Self::Elf => "CAN_BL.elf",
            Self::Hex => "CAN_BL.hex",
            Self::Bin => "CAN_BL.bin",
        }
    }
}

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("could not determine a cache directory; set XDG_CACHE_HOME / HOME / LOCALAPPDATA")]
    NoCacheDir,
    #[error("filesystem: {0}")]
    Io(#[from] std::io::Error),
    #[error("github api {status}: {body}")]
    HttpStatus { status: u16, body: String },
    #[error("http transport: {0}")]
    HttpTransport(String),
    #[error(
        "release `{tag}` of {repo} does not include `{asset}` — older BL releases (pre-v1.2.0) did not attach build artefacts; pin a newer `--release-tag` or update the BL release"
    )]
    AssetMissing {
        repo: &'static str,
        tag: String,
        asset: &'static str,
    },
    #[error("github releases response could not be parsed: {0}")]
    Json(#[from] serde_json::Error),
}

/// A resolved bootloader artefact on local disk. The path is
/// always under our cache directory; ok to hand directly to the
/// SWD flash pipeline.
#[derive(Debug, Clone)]
pub struct CachedBootloader {
    pub tag: String,
    pub path: PathBuf,
    /// `true` when this call had to hit GitHub for download;
    /// `false` when the cache served the request.
    pub downloaded: bool,
}

/// Resolve a tag to an on-disk artefact, downloading if needed.
///
/// `tag = None` ⇒ `/releases/latest`. The resolved tag (e.g.
/// `v1.2.0`) anchors the cache key, so two calls with `None` only
/// download once even if the API gets called twice.
pub fn fetch(tag: Option<&str>, format: BootloaderFormat) -> Result<CachedBootloader, FetchError> {
    let release = resolve_release(tag)?;
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == format.asset_name())
        .ok_or_else(|| FetchError::AssetMissing {
            repo: BOOTLOADER_REPO,
            tag: release.tag_name.clone(),
            asset: format.asset_name(),
        })?;

    let cache_dir = cache_dir_for_tag(&release.tag_name)?;
    fs::create_dir_all(&cache_dir)?;
    let target = cache_dir.join(format.asset_name());

    if target.is_file() {
        debug!(?target, tag = %release.tag_name, "bootloader cache hit");
        return Ok(CachedBootloader {
            tag: release.tag_name,
            path: target,
            downloaded: false,
        });
    }

    info!(
        url = %asset.browser_download_url,
        target = ?target,
        tag = %release.tag_name,
        "downloading bootloader artefact",
    );
    download_to(&asset.browser_download_url, &target)?;

    Ok(CachedBootloader {
        tag: release.tag_name,
        path: target,
        downloaded: true,
    })
}

// ---- GitHub API ---------------------------------------------------

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

fn resolve_release(tag: Option<&str>) -> Result<GhRelease, FetchError> {
    let url = match tag {
        Some(t) => format!(
            "https://api.github.com/repos/{}/releases/tags/{}",
            BOOTLOADER_REPO, t
        ),
        None => format!(
            "https://api.github.com/repos/{}/releases/latest",
            BOOTLOADER_REPO
        ),
    };
    let agent = http_agent();
    let resp = agent
        .get(&url)
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(into_fetch_err)?;
    let release: GhRelease = resp.into_json()?;
    Ok(release)
}

fn download_to(url: &str, target: &std::path::Path) -> Result<(), FetchError> {
    let agent = http_agent();
    let resp = agent.get(url).call().map_err(into_fetch_err)?;
    // Stream to a temp file in the same dir, then rename — keeps
    // a partial download from masquerading as a complete one if
    // the connection drops mid-fetch.
    let tmp = target.with_extension("part");
    {
        let mut reader = resp.into_reader();
        let mut writer = fs::File::create(&tmp)?;
        // 8 KB is the kernel-friendly read size; the largest BL
        // asset today is ~70 KB so allocation patterns don't matter.
        let mut buf = [0u8; 8 * 1024];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut writer, &buf[..n])?;
        }
    }
    fs::rename(&tmp, target)?;
    Ok(())
}

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        // GitHub rejects anonymous requests with no UA; pin one
        // so abuse-detection treats us as a known client.
        .user_agent(concat!("can-flasher/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .build()
}

fn into_fetch_err(e: ureq::Error) -> FetchError {
    match e {
        ureq::Error::Status(status, response) => {
            let body = response.into_string().unwrap_or_default();
            FetchError::HttpStatus { status, body }
        }
        ureq::Error::Transport(t) => FetchError::HttpTransport(t.to_string()),
    }
}

// ---- Cache dir ----------------------------------------------------

fn cache_dir_for_tag(tag: &str) -> Result<PathBuf, FetchError> {
    let base = dirs::cache_dir().ok_or(FetchError::NoCacheDir)?;
    Ok(base.join("can-flasher").join("bootloaders").join(tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_names_match_bl_release_layout() {
        // Pin the names the BL repo uses on its release page so a
        // rename on either side breaks loudly here rather than
        // silently fetching the wrong file at runtime.
        assert_eq!(BootloaderFormat::Elf.asset_name(), "CAN_BL.elf");
        assert_eq!(BootloaderFormat::Hex.asset_name(), "CAN_BL.hex");
        assert_eq!(BootloaderFormat::Bin.asset_name(), "CAN_BL.bin");
    }

    #[test]
    fn cache_dir_segments_under_tag() {
        let p = cache_dir_for_tag("v1.2.0").unwrap();
        let tail: Vec<_> = p.components().rev().take(3).collect();
        // Reversed: ["v1.2.0", "bootloaders", "can-flasher"]
        assert_eq!(tail[0].as_os_str(), "v1.2.0");
        assert_eq!(tail[1].as_os_str(), "bootloaders");
        assert_eq!(tail[2].as_os_str(), "can-flasher");
    }
}
