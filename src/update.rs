//! Self-update: signed manifest check, verified download and in-place replacement
//! of the running executable.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::trust;

/// Maximum size of a downloaded update binary.
const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;

#[derive(Deserialize, Clone, Serialize, Debug)]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub notes: String,
}

fn parse_version(v: &str) -> Vec<u64> {
    let v = v.trim();
    let v = v.strip_prefix(['v', 'V']).unwrap_or(v);
    let v = v.split(['-', '+']).next().unwrap_or("");
    v.split('.')
        .map(|p| {
            let digits: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().unwrap_or(0)
        })
        .collect()
}

/// Compare numeric dotted versions (optional leading 'v', suffix after '-' ignored).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let a = parse_version(candidate);
    let b = parse_version(current);
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// `manifest_url` points to an Envelope whose payload is UpdateInfo JSON.
/// Returns Some(info) if the manifest version is newer than `current`.
pub fn check(manifest_url: &str, public_b64: &str, current: &str) -> Result<Option<UpdateInfo>, String> {
    let value = trust::fetch_verified_json(manifest_url, public_b64)?;
    let info: UpdateInfo =
        serde_json::from_value(value).map_err(|e| format!("invalid update manifest: {e}"))?;
    Ok(is_newer(&info.version, current).then_some(info))
}

fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("cannot locate current executable: {e}"))
}

fn old_path(exe: &Path) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    exe.with_file_name(name)
}

fn download_to(info: &UpdateInfo, dir: &Path, base_name: &str) -> Result<PathBuf, String> {
    let data = trust::fetch_bytes(&info.url, MAX_DOWNLOAD_BYTES)?;
    let actual = trust::sha256_hex(&data);
    if !actual.eq_ignore_ascii_case(info.sha256.trim()) {
        return Err(format!(
            "sha256 mismatch for update {}: expected {}, got {actual}",
            info.version, info.sha256
        ));
    }
    let staged = dir.join(format!("{base_name}.update-{}.tmp", std::process::id()));
    std::fs::write(&staged, &data).map_err(|e| format!("{}: {e}", staged.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755));
    }
    Ok(staged)
}

/// Download (max 200 MB) to a temp file next to the current exe and verify its
/// sha256 (hex, case-insensitive). Returns the staged path.
pub fn download(info: &UpdateInfo) -> Result<PathBuf, String> {
    let exe = current_exe()?;
    let dir = exe.parent().ok_or("executable has no parent directory")?;
    let base = exe.file_name().unwrap_or_default().to_string_lossy().into_owned();
    download_to(info, dir, &base)
}

/// Swap `staged` into `exe`'s place, keeping the previous binary as `<name>.old`.
fn replace_exe(exe: &Path, staged: &Path) -> Result<(), String> {
    let old = old_path(exe);
    if old.exists() {
        std::fs::remove_file(&old).map_err(|e| format!("cannot remove {}: {e}", old.display()))?;
    }
    // A running exe on Windows can be renamed but not overwritten.
    std::fs::rename(exe, &old).map_err(|e| format!("cannot rename {}: {e}", exe.display()))?;
    if let Err(e) = std::fs::rename(staged, exe) {
        // Roll back so the installation stays usable.
        let _ = std::fs::rename(&old, exe);
        return Err(format!("cannot move update into place: {e}"));
    }
    Ok(())
}

/// Replace the running executable with `staged` and spawn the new exe with the
/// same arguments. Returns Ok(()) so the caller can exit.
pub fn apply_and_restart(staged: &Path) -> Result<(), String> {
    let exe = current_exe()?;
    replace_exe(&exe, staged)?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    std::process::Command::new(&exe)
        .args(&args)
        .spawn()
        .map_err(|e| format!("update installed but restart failed: {e}"))?;
    Ok(())
}

/// Delete a leftover `<exe>.old` from a previous update (call on startup).
pub fn cleanup_old() {
    if let Ok(exe) = std::env::current_exe() {
        let old = old_path(&exe);
        if old.exists()
            && let Err(e) = std::fs::remove_file(&old)
        {
            log::debug!("could not remove {}: {e}", old.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "brust-update-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn version_comparison() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("v1.10.0", "1.9.9"));
        assert!(is_newer("1.0.1", "1.0"));
        assert!(is_newer("2", "1.99.99"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("1.0", "1.0.0"));
        assert!(!is_newer("1.0.0-beta", "1.0.0"));
        assert!(!is_newer("0.9.0", "v1.0.0"));
        assert!(is_newer("1.2.3-rc1", "1.2.2"));
    }

    #[test]
    fn download_verifies_sha256() {
        let d = tmpdir("dl");
        let src = d.join("new-brust.bin");
        std::fs::write(&src, b"new binary contents").unwrap();
        let hash = trust::sha256_hex(b"new binary contents").to_uppercase();
        let mut info = UpdateInfo {
            version: "9.9.9".into(),
            url: format!("file://{}", src.to_str().unwrap()),
            sha256: hash,
            notes: String::new(),
        };
        let staged = download_to(&info, &d, "brust").unwrap();
        assert_eq!(std::fs::read(&staged).unwrap(), b"new binary contents");

        info.sha256 = "00".repeat(32);
        assert!(download_to(&info, &d, "brust").is_err());

        // Replacement swaps files and keeps the old one.
        let exe = d.join("brust");
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(old_path(&exe), b"older").unwrap();
        replace_exe(&exe, &staged).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new binary contents");
        assert_eq!(std::fs::read(old_path(&exe)).unwrap(), b"old");
        assert!(!staged.exists());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn check_signed_manifest() {
        let d = tmpdir("check");
        let (sk, pk) = trust::generate_keypair();
        let info = serde_json::json!({"version": "v99.0.0", "url": "https://x/y.exe", "sha256": "ab"});
        let env = trust::sign(info.to_string().as_bytes(), &sk).unwrap();
        let m = d.join("manifest.json");
        std::fs::write(&m, serde_json::to_vec(&env).unwrap()).unwrap();
        let ms = m.to_str().unwrap();
        let got = check(ms, &pk, "0.1.0").unwrap().unwrap();
        assert_eq!(got.version, "v99.0.0");
        assert!(got.notes.is_empty());
        assert!(check(ms, &pk, "99.0.0").unwrap().is_none());
        let (_, other) = trust::generate_keypair();
        assert!(check(ms, &other, "0.1.0").is_err());
        let _ = std::fs::remove_dir_all(d);
    }
}
