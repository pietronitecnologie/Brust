//! Local secret protection (DPAPI on Windows) and PIN hashing for the lock screen.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use sha2::{Digest, Sha256};

const PIN_ROUNDS: u32 = 100_000;

#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    fn input_blob(data: &[u8]) -> Result<CRYPT_INTEGER_BLOB, String> {
        Ok(CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(data.len()).map_err(|_| "data too large".to_string())?,
            pbData: data.as_ptr() as *mut u8,
        })
    }

    /// Copy the DPAPI-allocated output into a Vec and free it with LocalFree.
    unsafe fn take_output(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        if out.pbData.is_null() {
            return Vec::new();
        }
        let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
        unsafe { LocalFree(out.pbData as _) };
        v
    }

    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        let input = input_blob(data)?;
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(format!("CryptProtectData failed: {}", std::io::Error::last_os_error()));
        }
        Ok(unsafe { take_output(out) })
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        let input = input_blob(data)?;
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(format!("CryptUnprotectData failed: {}", std::io::Error::last_os_error()));
        }
        Ok(unsafe { take_output(out) })
    }
}

/// Encrypt data for the current user (Windows DPAPI). Identity on other OSes.
pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    {
        dpapi::protect(data)
    }
    #[cfg(not(windows))]
    {
        Ok(data.to_vec())
    }
}

/// Decrypt data produced by [`protect`]. Identity on other OSes.
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    {
        dpapi::unprotect(data)
    }
    #[cfg(not(windows))]
    {
        Ok(data.to_vec())
    }
}

/// Store a small secret string in a file: protect() then base64.
/// On non-Windows the file mode is set to 0600.
pub fn save_secret(path: &Path, value: &str) -> Result<(), String> {
    let enc = B64.encode(protect(value.as_bytes())?);
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        // Enforce the mode for files that already existed.
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        f.write_all(enc.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, enc).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(())
}

/// Load a secret saved with [`save_secret`]. None if missing or undecodable.
pub fn load_secret(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let raw = B64.decode(text.trim()).ok()?;
    String::from_utf8(unprotect(&raw).ok()?).ok()
}

fn pin_digest(salt: &[u8], pin: &str, rounds: u32) -> [u8; 32] {
    let mut prev: [u8; 32] = {
        let mut h = Sha256::new();
        h.update(salt);
        h.update(pin.as_bytes());
        h.finalize().into()
    };
    for _ in 1..rounds {
        let mut h = Sha256::new();
        h.update(salt);
        h.update(prev);
        prev = h.finalize().into();
    }
    prev
}

/// Hash a PIN: random 16-byte salt, iterated SHA-256 (first round over salt||pin,
/// then sha256(salt||prev) for a total of 100_000 rounds). Format "v1$<salt b64>$<hash b64>".
pub fn hash_pin(pin: &str) -> String {
    use rand_core::RngCore;
    let mut salt = [0u8; 16];
    rand_core::OsRng.fill_bytes(&mut salt);
    let hash = pin_digest(&salt, pin, PIN_ROUNDS);
    format!("v1${}${}", B64.encode(salt), B64.encode(hash))
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

/// Verify a PIN against a stored "v1$salt$hash" string (constant-time compare).
pub fn verify_pin(pin: &str, stored: &str) -> bool {
    let mut parts = stored.trim().split('$');
    let (Some("v1"), Some(salt), Some(hash), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let (Ok(salt), Ok(hash)) = (B64.decode(salt), B64.decode(hash)) else {
        return false;
    };
    ct_eq(&pin_digest(&salt, pin, PIN_ROUNDS), &hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protect_roundtrip() {
        let data = b"top secret \x00\xff";
        let p = protect(data).unwrap();
        #[cfg(not(windows))]
        assert_eq!(p, data);
        assert_eq!(unprotect(&p).unwrap(), data);
    }

    #[test]
    fn save_load_secret() {
        let d = std::env::temp_dir().join(format!("brust-secrets-{}", std::process::id()));
        let p = d.join("sub").join("token.bin");
        save_secret(&p, "s3cr3t-tøken").unwrap();
        assert_eq!(load_secret(&p).as_deref(), Some("s3cr3t-tøken"));
        save_secret(&p, "second").unwrap();
        assert_eq!(load_secret(&p).as_deref(), Some("second"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(load_secret(&d.join("missing")).is_none());
        std::fs::write(&p, "%%% not base64").unwrap();
        assert!(load_secret(&p).is_none());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn pin_hash_verify() {
        let h = hash_pin("1234");
        assert!(h.starts_with("v1$"));
        assert_eq!(h.split('$').count(), 3);
        assert!(verify_pin("1234", &h));
        assert!(!verify_pin("1235", &h));
        assert!(!verify_pin("", &h));
        assert_ne!(hash_pin("1234"), h, "salt must be random");
        assert!(!verify_pin("1234", "garbage"));
        assert!(!verify_pin("1234", "v2$AAAA$AAAA"));
        assert!(!verify_pin("1234", &format!("{h}$extra")));
    }
}
