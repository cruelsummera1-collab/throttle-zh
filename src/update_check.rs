//! Background check for a newer GitHub release, and in-place self-update.
//!
//! The check runs once at startup on its own thread so a slow or missing
//! network never blocks the UI. Any failure is logged and treated as "no
//! update".
//!
//! Installing relies on Windows allowing a running executable to be renamed
//! (but not overwritten): the current exe is moved aside to `throttle.exe.old`,
//! the new one is written in its place, and the caller relaunches it. The new
//! process waits for the old one to exit (`--wait-pid`) and removes the `.old`
//! file on startup. Only `throttle.exe` is replaced; the WinDivert files are
//! held open by the running process and are left as they are.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, bounded};

const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/FrancescoCoding/Throttle/releases/latest";
const USER_AGENT: &str = concat!("Throttle/", env!("CARGO_PKG_VERSION"));

/// A release newer than the running build.
#[derive(Clone, Debug)]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
    /// Download URL of the Windows zip, if the release has one.
    pub asset_url: Option<String>,
}

/// Spawn the check; the receiver yields at most one value.
pub fn spawn(current: &'static str) -> Receiver<UpdateInfo> {
    let (tx, rx) = bounded(1);
    let _ = thread::Builder::new()
        .name("update-check".into())
        .spawn(move || match fetch_latest() {
            Ok(info) if is_newer(&info.version, current) => {
                tracing::info!("update available: {} (running {current})", info.version);
                let _ = tx.send(info);
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("update check failed: {e:#}"),
        });
    rx
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .into()
}

/// Release API endpoint. Debug builds honor `THROTTLE_UPDATE_API`, or a
/// `update-api.txt` next to the exe (UAC elevation drops the caller's
/// environment), so the update flow can be exercised against a fake release.
fn release_api() -> String {
    if cfg!(debug_assertions) {
        let from_file = std::env::current_exe()
            .ok()
            .and_then(|exe| std::fs::read_to_string(exe.with_file_name("update-api.txt")).ok());
        if let Some(url) = std::env::var("THROTTLE_UPDATE_API").ok().or(from_file) {
            return url.trim().to_string();
        }
    }
    LATEST_RELEASE_API.to_string()
}

fn fetch_latest() -> anyhow::Result<UpdateInfo> {
    let body = agent(Duration::from_secs(10))
        .get(&release_api())
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .call()?
        .body_mut()
        .read_to_string()?;
    let json: serde_json::Value = serde_json::from_str(&body)?;
    let tag = json["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing tag_name"))?;
    // Offer the in-app install only when the zip ships with its checksum file;
    // otherwise the banner falls back to opening the release page.
    let asset_url = json["assets"].as_array().and_then(|assets| {
        let url_of = |suffix: &str| {
            assets.iter().find_map(|a| {
                let name = a["name"].as_str()?;
                if name.ends_with(suffix) {
                    a["browser_download_url"].as_str().map(str::to_string)
                } else {
                    None
                }
            })
        };
        let zip = url_of("-windows-x64.zip")?;
        url_of("-windows-x64.zip.sha256")?;
        Some(zip)
    });
    Ok(UpdateInfo {
        version: tag.trim_start_matches('v').to_string(),
        url: json["html_url"].as_str().unwrap_or_default().to_string(),
        asset_url,
    })
}

/// Path the running exe is moved to while its replacement is installed.
fn old_exe_path(exe: &Path) -> PathBuf {
    exe.with_extension("exe.old")
}

/// Download the release zip, verify it against the `.sha256` file published
/// next to it, and swap it in for the running executable. Returns the path to
/// launch. On failure the original exe is left in place.
pub fn install(asset_url: &str) -> anyhow::Result<PathBuf> {
    let zip_bytes = agent(Duration::from_secs(300))
        .get(asset_url)
        .header("User-Agent", USER_AGENT)
        .call()?
        .body_mut()
        .with_config()
        .limit(100 * 1024 * 1024)
        .read_to_vec()?;

    // `sha256sum` format: "<hex>  <file name>"; only the hash is used.
    let checksum_file = agent(Duration::from_secs(30))
        .get(&format!("{asset_url}.sha256"))
        .header("User-Agent", USER_AGENT)
        .call()?
        .body_mut()
        .read_to_string()?;
    let expected = checksum_file
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty checksum file"))?;
    let actual = sha256_hex(&zip_bytes);
    if !actual.eq_ignore_ascii_case(expected) {
        anyhow::bail!("checksum mismatch: expected {expected}, got {actual}");
    }

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))?;
    let name = archive
        .file_names()
        .find(|n| n.rsplit('/').next() == Some("throttle.exe"))
        .ok_or_else(|| anyhow::anyhow!("throttle.exe not found in release zip"))?
        .to_string();
    let mut new_exe = Vec::new();
    archive.by_name(&name)?.read_to_end(&mut new_exe)?;

    let exe = std::env::current_exe()?;
    let staged = exe.with_extension("exe.new");
    let old = old_exe_path(&exe);
    std::fs::write(&staged, &new_exe)?;
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old)?;
    if let Err(e) = std::fs::rename(&staged, &exe) {
        // Put the original back so the next launch still works.
        let _ = std::fs::rename(&old, &exe);
        let _ = std::fs::remove_file(&staged);
        return Err(e.into());
    }
    tracing::info!("installed update to {}", exe.display());
    Ok(exe)
}

/// Remove the exe left behind by a previous update. Best effort.
pub fn cleanup_old_exe() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_exe_path(&exe));
    }
}

/// Block until process `pid` exits, up to `timeout`. Used by a freshly
/// installed build so it only takes the single-instance lock once the build it
/// replaced has saved its rules and quit.
pub fn wait_for_pid(pid: u32, timeout: Duration) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    // SAFETY: the handle is checked before use and closed afterwards.
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(h, timeout.as_millis() as u32);
            let _ = CloseHandle(h);
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Compare dotted numeric versions; pre-release suffixes are ignored.
fn is_newer(latest: &str, current: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.split(['-', '+'])
            .next()
            .unwrap_or_default()
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    }
    let (a, b) = (parts(latest), parts(current));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{is_newer, sha256_hex};

    #[test]
    fn hashes_known_input() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.1.2", "0.1.1"));
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.1.1", "0.1.1"));
        assert!(!is_newer("0.1.0", "0.1.1"));
        assert!(!is_newer("0.1.1-beta", "0.1.1"));
    }
}
