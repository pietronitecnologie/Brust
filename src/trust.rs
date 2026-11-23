//! Signed envelopes for central remote configuration and update manifests.
//!
//! An envelope carries a base64-encoded payload plus an ed25519 signature over
//! the raw payload bytes. Keys are exchanged as base64 text (32-byte seed for
//! the private key, 32-byte compressed point for the public key).

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Maximum size of a fetched envelope/config document.
const MAX_ENVELOPE_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Envelope {
    /// Base64 of the raw payload bytes.
    pub payload: String,
    /// Base64 ed25519 signature over the raw payload bytes.
    pub signature: String,
}

fn random_bytes<const N: usize>() -> [u8; N] {
    use rand_core::RngCore;
    let mut buf = [0u8; N];
    rand_core::OsRng.fill_bytes(&mut buf);
    buf
}

/// Generate a new keypair. Returns (private key b64 (32-byte seed), public key b64).
pub fn generate_keypair() -> (String, String) {
    let seed: [u8; 32] = random_bytes();
    let sk = SigningKey::from_bytes(&seed);
    let pk = sk.verifying_key();
    (B64.encode(seed), B64.encode(pk.to_bytes()))
}

fn decode_key32(b64: &str, what: &str) -> Result<[u8; 32], String> {
    let raw = B64
        .decode(b64.trim())
        .map_err(|e| format!("invalid {what} (base64): {e}"))?;
    raw.as_slice()
        .try_into()
        .map_err(|_| format!("invalid {what}: expected 32 bytes, got {}", raw.len()))
}

pub fn sign(payload: &[u8], private_b64: &str) -> Result<Envelope, String> {
    let seed = decode_key32(private_b64, "private key")?;
    let sk = SigningKey::from_bytes(&seed);
    let sig: Signature = sk.sign(payload);
    Ok(Envelope {
        payload: B64.encode(payload),
        signature: B64.encode(sig.to_bytes()),
    })
}

/// Verify an envelope and return the raw payload bytes.
pub fn verify(env: &Envelope, public_b64: &str) -> Result<Vec<u8>, String> {
    let pk_bytes = decode_key32(public_b64, "public key")?;
    let pk = VerifyingKey::from_bytes(&pk_bytes).map_err(|e| format!("invalid public key: {e}"))?;
    let payload = B64
        .decode(env.payload.trim())
        .map_err(|e| format!("invalid payload (base64): {e}"))?;
    let sig_raw = B64
        .decode(env.signature.trim())
        .map_err(|e| format!("invalid signature (base64): {e}"))?;
    let sig = Signature::from_slice(&sig_raw).map_err(|e| format!("invalid signature: {e}"))?;
    pk.verify_strict(&payload, &sig)
        .map_err(|_| "signature verification failed".to_string())?;
    Ok(payload)
}

fn read_local(path: &Path, max_bytes: u64) -> Result<Vec<u8>, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = Vec::new();
    f.take(max_bytes + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if buf.len() as u64 > max_bytes {
        return Err(format!("{}: larger than {max_bytes} bytes", path.display()));
    }
    Ok(buf)
}

/// Convert a file:// URL to a local path. Handles `file:///C:/x`, `file:///etc/x`
/// and UNC hosts (`file://server/share/x` -> `\\server\share\x`).
fn file_url_to_path(rest: &str) -> std::path::PathBuf {
    let rest = percent_decode(rest);
    if let Some(p) = rest.strip_prefix('/') {
        // file:///...
        let bytes = p.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            return std::path::PathBuf::from(p); // Windows drive path
        }
        return std::path::PathBuf::from(format!("/{p}"));
    }
    if rest.starts_with("localhost/") {
        return file_url_to_path(&rest["localhost".len()..]);
    }
    // file://host/share/... -> UNC
    if cfg!(windows) {
        std::path::PathBuf::from(format!(r"\\{}", rest.replace('/', "\\")))
    } else {
        std::path::PathBuf::from(format!("//{rest}"))
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(v) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Fetch bytes from an http(s) URL (30s timeout), a file:// URL or a plain local
/// path (including UNC paths such as `\\server\share\brust.json`).
pub fn fetch_bytes(url: &str, max_bytes: u64) -> Result<Vec<u8>, String> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        let mut resp = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
        return resp
            .body_mut()
            .with_config()
            .limit(max_bytes)
            .read_to_vec()
            .map_err(|e| format!("{url}: {e}"));
    }
    if lower.starts_with("file://") {
        return read_local(&file_url_to_path(&url[7..]), max_bytes);
    }
    read_local(Path::new(url), max_bytes)
}

/// Fetch an envelope JSON document, verify it and parse the payload as JSON.
pub fn fetch_verified_json(url: &str, public_b64: &str) -> Result<serde_json::Value, String> {
    let raw = fetch_bytes(url, MAX_ENVELOPE_BYTES)?;
    let env: Envelope =
        serde_json::from_slice(&raw).map_err(|e| format!("{url}: invalid envelope: {e}"))?;
    let payload = verify(&env, public_b64)?;
    serde_json::from_slice(&payload).map_err(|e| format!("{url}: payload is not JSON: {e}"))
}

/// Lowercase hex SHA-256 of a byte slice.
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn arg_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
}

/// Read a key that is given either as base64 text or as a path to a key file.
fn key_arg(v: &str) -> Result<String, String> {
    let p = Path::new(v);
    if p.is_file() {
        std::fs::read_to_string(p)
            .map(|s| s.trim().to_string())
            .map_err(|e| format!("{v}: {e}"))
    } else {
        Ok(v.trim().to_string())
    }
}

fn cli_keygen(dir: &str) -> Result<(), String> {
    let dir = Path::new(dir);
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let priv_path = dir.join("brust-private.key");
    let pub_path = dir.join("brust-public.key");
    if priv_path.exists() {
        return Err(format!("{} already exists, refusing to overwrite", priv_path.display()));
    }
    let (sk, pk) = generate_keypair();
    std::fs::write(&priv_path, format!("{sk}\n"))
        .map_err(|e| format!("{}: {e}", priv_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&priv_path, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::write(&pub_path, format!("{pk}\n"))
        .map_err(|e| format!("{}: {e}", pub_path.display()))?;
    eprintln!("Wrote {} and {}", priv_path.display(), pub_path.display());
    eprintln!("Keep the private key secret. Public key:");
    println!("{pk}");
    Ok(())
}

fn cli_sign(args: &[String], input: &str) -> Result<(), String> {
    let key = arg_value(args, "--key").ok_or("--sign requires --key <private.key>")?;
    let data = std::fs::read(input).map_err(|e| format!("{input}: {e}"))?;
    serde_json::from_slice::<serde_json::Value>(&data)
        .map_err(|e| format!("{input}: not valid JSON: {e}"))?;
    let env = sign(&data, &key_arg(key)?)?;
    let out = serde_json::to_string_pretty(&env).map_err(|e| e.to_string())?;
    match arg_value(args, "--out") {
        Some(path) => {
            std::fs::write(path, format!("{out}\n")).map_err(|e| format!("{path}: {e}"))?;
            eprintln!("Wrote {path}");
        }
        None => println!("{out}"),
    }
    Ok(())
}

fn cli_verify(args: &[String], input: &str) -> Result<(), String> {
    let pk = arg_value(args, "--pubkey").ok_or("--verify requires --pubkey <b64 or file>")?;
    let data = fetch_bytes(input, MAX_ENVELOPE_BYTES)?;
    let env: Envelope =
        serde_json::from_slice(&data).map_err(|e| format!("{input}: invalid envelope: {e}"))?;
    let payload = verify(&env, &key_arg(pk)?)?;
    println!("{}", String::from_utf8_lossy(&payload));
    eprintln!("Signature OK");
    Ok(())
}

fn cli_hash(path: &str) -> Result<(), String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("{path}: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let hex: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    println!("{hex}");
    Ok(())
}

/// CLI helpers, called from main() before the GUI starts.
/// Returns Some(exit_code) if args were a CLI command, None otherwise.
///
/// - `brust --keygen <dir>`: writes brust-private.key and brust-public.key, prints public key
/// - `brust --sign <input.json> --key <private.key> [--out <file>]`: writes Envelope JSON
/// - `brust --verify <envelope.json> --pubkey <b64 or file>`: prints payload, exit 0/1
/// - `brust --hash-file <file>`: prints sha256 hex
pub fn run_cli(args: &[String]) -> Option<i32> {
    let result = if let Some(dir) = arg_value(args, "--keygen") {
        cli_keygen(dir)
    } else if let Some(input) = arg_value(args, "--sign") {
        cli_sign(args, input)
    } else if let Some(input) = arg_value(args, "--verify") {
        cli_verify(args, input)
    } else if let Some(path) = arg_value(args, "--hash-file") {
        cli_hash(path)
    } else if args
        .iter()
        .any(|a| matches!(a.as_str(), "--keygen" | "--sign" | "--verify" | "--hash-file"))
    {
        Err("missing argument; usage:\n  brust --keygen <dir>\n  brust --sign <input.json> --key <private.key> [--out <file>]\n  brust --verify <envelope.json> --pubkey <b64 or file>\n  brust --hash-file <file>".to_string())
    } else {
        return None;
    };
    match result {
        Ok(()) => Some(0),
        Err(e) => {
            eprintln!("error: {e}");
            Some(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "brust-trust-{name}-{}-{}",
            std::process::id(),
            B64.encode(random_bytes::<6>()).replace(['/', '+'], "_")
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn keygen_produces_valid_keys() {
        let (sk, pk) = generate_keypair();
        assert_eq!(B64.decode(&sk).unwrap().len(), 32);
        assert_eq!(B64.decode(&pk).unwrap().len(), 32);
        let seed = decode_key32(&sk, "k").unwrap();
        assert_eq!(B64.encode(SigningKey::from_bytes(&seed).verifying_key().to_bytes()), pk);
        let (sk2, _) = generate_keypair();
        assert_ne!(sk, sk2);
    }

    #[test]
    fn sign_verify_roundtrip_and_tamper() {
        let (sk, pk) = generate_keypair();
        let env = sign(br#"{"a":1}"#, &sk).unwrap();
        assert_eq!(verify(&env, &pk).unwrap(), br#"{"a":1}"#);

        let mut bad = env.clone();
        bad.payload = B64.encode(br#"{"a":2}"#);
        assert!(verify(&bad, &pk).is_err());

        let mut bad_sig = env.clone();
        let mut s = B64.decode(&bad_sig.signature).unwrap();
        s[0] ^= 1;
        bad_sig.signature = B64.encode(s);
        assert!(verify(&bad_sig, &pk).is_err());

        let (_, other_pk) = generate_keypair();
        assert!(verify(&env, &other_pk).is_err());
        assert!(verify(&env, "not base64!").is_err());
    }

    #[test]
    fn fetch_local_and_file_url() {
        let d = tmpdir("fetch");
        let p = d.join("env.json");
        let (sk, pk) = generate_keypair();
        let env = sign(br#"{"hello":"world"}"#, &sk).unwrap();
        std::fs::write(&p, serde_json::to_vec(&env).unwrap()).unwrap();

        let v = fetch_verified_json(p.to_str().unwrap(), &pk).unwrap();
        assert_eq!(v["hello"], "world");
        let url = format!("file://{}", p.to_str().unwrap());
        assert_eq!(fetch_verified_json(&url, &pk).unwrap()["hello"], "world");
        assert!(fetch_bytes(p.to_str().unwrap(), 5).is_err());
        assert!(fetch_bytes(d.join("missing").to_str().unwrap(), 100).is_err());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn file_url_parsing() {
        assert_eq!(file_url_to_path("/etc/x%20y"), std::path::PathBuf::from("/etc/x y"));
        assert_eq!(file_url_to_path("/C:/a/b"), std::path::PathBuf::from("C:/a/b"));
    }

    #[test]
    fn sha256_known_value() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn cli_keygen_sign_verify() {
        let d = tmpdir("cli");
        let ds = d.to_str().unwrap().to_string();
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        assert_eq!(run_cli(&a(&["brust", "--keygen", &ds])), Some(0));
        // Refuses to overwrite.
        assert_eq!(run_cli(&a(&["brust", "--keygen", &ds])), Some(1));

        let input = d.join("cfg.json");
        std::fs::write(&input, br#"{"url":"https://example.com"}"#).unwrap();
        let out = d.join("cfg.signed.json");
        let key = d.join("brust-private.key");
        let pubkey = d.join("brust-public.key");
        assert_eq!(
            run_cli(&a(&[
                "brust", "--sign", input.to_str().unwrap(), "--key", key.to_str().unwrap(),
                "--out", out.to_str().unwrap()
            ])),
            Some(0)
        );
        assert_eq!(
            run_cli(&a(&["brust", "--verify", out.to_str().unwrap(), "--pubkey", pubkey.to_str().unwrap()])),
            Some(0)
        );
        // Public key given inline as b64.
        let pk_text = std::fs::read_to_string(&pubkey).unwrap();
        assert_eq!(
            run_cli(&a(&["brust", "--verify", out.to_str().unwrap(), "--pubkey", pk_text.trim()])),
            Some(0)
        );
        let (_, other) = generate_keypair();
        assert_eq!(
            run_cli(&a(&["brust", "--verify", out.to_str().unwrap(), "--pubkey", &other])),
            Some(1)
        );
        // Non-JSON input is rejected.
        let bad = d.join("bad.json");
        std::fs::write(&bad, b"not json").unwrap();
        assert_eq!(
            run_cli(&a(&["brust", "--sign", bad.to_str().unwrap(), "--key", key.to_str().unwrap()])),
            Some(1)
        );
        assert_eq!(run_cli(&a(&["brust", "--hash-file", input.to_str().unwrap()])), Some(0));
        assert_eq!(run_cli(&a(&["brust", "--sign"])), Some(1));
        assert_eq!(run_cli(&a(&["brust", "--config", "x"])), None);
        let _ = std::fs::remove_dir_all(d);
    }
}
