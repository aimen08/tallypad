//! Self-update from GitHub Releases.
//!
//! The release workflow attaches `Tallypad.exe` to each `vX.Y.Z` release. To
//! update, the running exe is renamed (Windows allows renaming a running
//! executable), the new one is written in its place, and the app relaunches.

use std::path::PathBuf;
use std::time::Duration;

pub const REPO: &str = "aimen08/tallypad";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const ASSET_NAME: &str = "Tallypad.exe";

#[derive(Debug, Clone)]
pub struct Release {
    pub version: String,
    pub page_url: String,
    pub exe_url: Option<String>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .build()
        .into()
}

/// Fetches the latest published release (blocking).
pub fn latest() -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = agent()
        .get(&url)
        .header("User-Agent", "Tallypad-updater")
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    parse_release(&body).ok_or_else(|| "unexpected response from GitHub".into())
}

fn parse_release(json: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let tag = v.get("tag_name")?.as_str()?;
    let exe_url = v
        .get("assets")
        .and_then(|a| a.as_array())
        .and_then(|assets| {
            assets.iter().find_map(|a| {
                (a.get("name")?.as_str()? == ASSET_NAME)
                    .then(|| a.get("browser_download_url")?.as_str().map(str::to_string))
                    .flatten()
            })
        });
    Some(Release {
        version: tag.trim_start_matches('v').to_string(),
        page_url: v.get("html_url")?.as_str()?.to_string(),
        exe_url,
    })
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.trim_start_matches('v').split(['.', '-']);
    Some((
        parts.next()?.parse().ok()?,
        parts.next().unwrap_or("0").parse().ok()?,
        parts.next().unwrap_or("0").parse().ok()?,
    ))
}

pub fn is_newer(remote: &str, current: &str) -> bool {
    match (parse_version(remote), parse_version(current)) {
        (Some(r), Some(c)) => r > c,
        _ => false,
    }
}

fn old_exe_path(exe: &std::path::Path) -> PathBuf {
    exe.with_extension("old")
}

/// Removes the previous executable left behind by an update.
pub fn cleanup_previous() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_exe_path(&exe));
    }
}

/// Downloads `url` and replaces the running executable. Returns the path to
/// relaunch.
pub fn install(url: &str) -> Result<PathBuf, String> {
    let bytes = agent()
        .get(url)
        .header("User-Agent", "Tallypad-updater")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(256 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    // A Windows PE file starts with "MZ".
    if bytes.len() < 1024 || &bytes[..2] != b"MZ" {
        return Err("downloaded file is not a Windows executable".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = old_exe_path(&exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| format!("couldn't move current exe: {e}"))?;
    if let Err(e) = std::fs::write(&exe, &bytes) {
        // Roll back so the app still starts next time.
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("couldn't write new exe: {e}"));
    }
    Ok(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("garbage", "0.1.0"));
    }

    #[test]
    fn release_json() {
        let json = r#"{"tag_name":"v0.2.0","html_url":"https://github.com/x/y/releases/tag/v0.2.0",
            "assets":[{"name":"Tallypad-windows-x64.zip","browser_download_url":"https://a/zip"},
                      {"name":"Tallypad.exe","browser_download_url":"https://a/exe"}]}"#;
        let r = parse_release(json).unwrap();
        assert_eq!(r.version, "0.2.0");
        assert_eq!(r.exe_url.as_deref(), Some("https://a/exe"));
    }
}
