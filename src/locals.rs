//! Capacità locali esposte agli applicativi web tramite il bridge:
//! esecuzione processi, filesystem, appunti, porte seriali, apertura file/URL.
//! Ogni comando viene eseguito su un thread separato per non bloccare l'interfaccia.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tao::event_loop::EventLoopProxy;

use crate::config::AppDef;
use crate::UserEvent;

/// Permesso richiesto da ciascun comando locale (None = comando non locale).
pub fn permission_for(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        "system.info" => "",
        "shell.exec" | "env.get" => "exec",
        "shell.open" => "open",
        "fs.read" | "fs.write" | "fs.list" | "fs.exists" | "fs.mkdir" | "fs.remove" => "fs",
        "clipboard.read" | "clipboard.write" => "clipboard",
        "serial.list" | "serial.open" | "serial.write" | "serial.close" => "serial",
        _ => return None,
    })
}

struct SerialEntry {
    pane: u32,
    port: Box<dyn serialport::SerialPort>,
    stop: Arc<AtomicBool>,
}

pub struct Locals {
    serial: Mutex<HashMap<u32, SerialEntry>>,
    next: AtomicU32,
    proxy: EventLoopProxy<UserEvent>,
}

type Res = Result<Value, String>;

fn s<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| format!("parametro '{key}' mancante"))
}

fn opt_s<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn decode(args: &Value) -> Result<Vec<u8>, String> {
    let data = s(args, "data")?;
    match opt_s(args, "encoding").unwrap_or("utf8") {
        "base64" => B64.decode(data).map_err(|e| e.to_string()),
        _ => Ok(data.as_bytes().to_vec()),
    }
}

impl Locals {
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> Arc<Self> {
        Arc::new(Locals { serial: Mutex::new(HashMap::new()), next: AtomicU32::new(1), proxy })
    }

    /// Esegue un comando locale; da chiamare su un thread di lavoro.
    pub fn execute(&self, app: &AppDef, pane: u32, cmd: &str, args: &Value) -> Res {
        match cmd {
            "system.info" => Ok(system_info()),
            "env.get" => Ok(json!(std::env::var(s(args, "name")?).ok())),
            "shell.exec" => exec(app, args),
            "shell.open" => {
                let target = s(args, "target")?;
                open::that_detached(target).map_err(|e| e.to_string())?;
                Ok(Value::Null)
            }
            "fs.read" => {
                let p = check_path(app, s(args, "path")?, false)?;
                let bytes = std::fs::read(&p).map_err(|e| e.to_string())?;
                Ok(match opt_s(args, "encoding").unwrap_or("utf8") {
                    "base64" => json!(B64.encode(bytes)),
                    _ => json!(String::from_utf8_lossy(&bytes)),
                })
            }
            "fs.write" => {
                let p = check_path(app, s(args, "path")?, true)?;
                let data = decode(args)?;
                let append = args.get("append").and_then(Value::as_bool).unwrap_or(false);
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .append(append)
                    .truncate(!append)
                    .open(&p)
                    .map_err(|e| e.to_string())?;
                f.write_all(&data).map_err(|e| e.to_string())?;
                Ok(json!(data.len()))
            }
            "fs.list" => {
                let p = check_path(app, s(args, "path")?, false)?;
                let mut out = Vec::new();
                for e in std::fs::read_dir(&p).map_err(|e| e.to_string())?.flatten() {
                    let md = e.metadata().ok();
                    let modified = md
                        .as_ref()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as u64);
                    out.push(json!({
                        "name": e.file_name().to_string_lossy(),
                        "dir": md.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                        "size": md.as_ref().map(|m| m.len()).unwrap_or(0),
                        "modified": modified,
                    }));
                }
                Ok(Value::Array(out))
            }
            "fs.exists" => {
                let p = check_path(app, s(args, "path")?, true)?;
                Ok(json!(p.exists()))
            }
            "fs.mkdir" => {
                let p = check_path(app, s(args, "path")?, true)?;
                std::fs::create_dir_all(&p).map_err(|e| e.to_string())?;
                Ok(Value::Null)
            }
            "fs.remove" => {
                let p = check_path(app, s(args, "path")?, false)?;
                if p.is_dir() {
                    std::fs::remove_dir(&p).map_err(|e| e.to_string())?;
                } else {
                    std::fs::remove_file(&p).map_err(|e| e.to_string())?;
                }
                Ok(Value::Null)
            }
            "clipboard.read" => {
                let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
                Ok(json!(cb.get_text().unwrap_or_default()))
            }
            "clipboard.write" => {
                let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
                cb.set_text(s(args, "text")?.to_string()).map_err(|e| e.to_string())?;
                Ok(Value::Null)
            }
            "serial.list" => {
                let ports = serialport::available_ports().map_err(|e| e.to_string())?;
                Ok(Value::Array(
                    ports
                        .into_iter()
                        .map(|p| {
                            let kind = match p.port_type {
                                serialport::SerialPortType::UsbPort(u) => json!({
                                    "type": "usb", "vid": u.vid, "pid": u.pid,
                                    "manufacturer": u.manufacturer, "product": u.product, "serial": u.serial_number
                                }),
                                serialport::SerialPortType::BluetoothPort => json!({"type": "bluetooth"}),
                                serialport::SerialPortType::PciPort => json!({"type": "pci"}),
                                serialport::SerialPortType::Unknown => json!({"type": "unknown"}),
                            };
                            json!({"port": p.port_name, "info": kind})
                        })
                        .collect(),
                ))
            }
            "serial.open" => self.serial_open(pane, args),
            "serial.write" => {
                let handle = args.get("handle").and_then(Value::as_u64).ok_or("parametro 'handle' mancante")? as u32;
                let data = decode(args)?;
                let mut map = self.serial.lock().unwrap();
                let e = map.get_mut(&handle).filter(|e| e.pane == pane).ok_or("porta non aperta")?;
                e.port.write_all(&data).map_err(|e| e.to_string())?;
                Ok(json!(data.len()))
            }
            "serial.close" => {
                let handle = args.get("handle").and_then(Value::as_u64).ok_or("parametro 'handle' mancante")? as u32;
                let mut map = self.serial.lock().unwrap();
                if map.get(&handle).is_some_and(|e| e.pane == pane) {
                    if let Some(e) = map.remove(&handle) {
                        e.stop.store(true, Ordering::Relaxed);
                    }
                }
                Ok(Value::Null)
            }
            _ => Err(format!("comando sconosciuto: {cmd}")),
        }
    }

    fn serial_open(&self, pane: u32, args: &Value) -> Res {
        use serialport::{DataBits, Parity, StopBits};
        let name = s(args, "port")?;
        let baud = args.get("baud").and_then(Value::as_u64).unwrap_or(9600) as u32;
        let data_bits = match args.get("data_bits").and_then(Value::as_u64).unwrap_or(8) {
            5 => DataBits::Five,
            6 => DataBits::Six,
            7 => DataBits::Seven,
            _ => DataBits::Eight,
        };
        let parity = match opt_s(args, "parity").unwrap_or("none") {
            "even" => Parity::Even,
            "odd" => Parity::Odd,
            _ => Parity::None,
        };
        let stop_bits = match args.get("stop_bits").and_then(Value::as_u64).unwrap_or(1) {
            2 => StopBits::Two,
            _ => StopBits::One,
        };
        let port = serialport::new(name, baud)
            .data_bits(data_bits)
            .parity(parity)
            .stop_bits(stop_bits)
            .timeout(Duration::from_millis(100))
            .open()
            .map_err(|e| e.to_string())?;
        let mut reader = port.try_clone().map_err(|e| e.to_string())?;
        let handle = self.next.fetch_add(1, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while !stop2.load(Ordering::Relaxed) {
                match reader.read(&mut buf) {
                    Ok(0) => {}
                    Ok(n) => {
                        let _ = proxy.send_event(UserEvent::SerialData { pane, handle, data: buf[..n].to_vec() });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::SerialClosed { pane, handle, error: Some(e.to_string()) });
                        return;
                    }
                }
            }
        });
        self.serial.lock().unwrap().insert(handle, SerialEntry { pane, port, stop });
        Ok(json!({ "handle": handle }))
    }

    /// Chiude le risorse (porte seriali) aperte da un pannello.
    pub fn release_pane(&self, pane: u32) {
        self.serial.lock().unwrap().retain(|_, e| {
            if e.pane == pane {
                e.stop.store(true, Ordering::Relaxed);
                false
            } else {
                true
            }
        });
    }
}

pub fn system_info() -> Value {
    let hostname = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_string()));
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).ok();
    json!({
        "client": "brust",
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "hostname": hostname,
        "username": user,
        "home": directories::UserDirs::new().map(|u| u.home_dir().to_string_lossy().into_owned()),
    })
}

fn program_allowed(app: &AppDef, program: &str) -> bool {
    // Nome file senza estensione, riconoscendo sia `/` sia `\` come separatori.
    let stem = |p: &str| {
        let name = p.rsplit(['/', '\\']).next().unwrap_or(p).to_ascii_lowercase();
        match name.rsplit_once('.') {
            Some((s, "exe" | "cmd" | "bat" | "com")) => s.to_string(),
            _ => name,
        }
    };
    let wanted = stem(program);
    app.exec_allow.iter().any(|a| a == "*" || stem(a) == wanted)
}

fn exec(app: &AppDef, args: &Value) -> Res {
    let program = s(args, "program")?;
    if !program_allowed(app, program) {
        return Err(format!("programma non consentito: {program} (vedi exec_allow)"));
    }
    let argv: Vec<String> = args
        .get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).collect())
        .unwrap_or_default();
    let timeout = Duration::from_millis(args.get("timeout_ms").and_then(Value::as_u64).unwrap_or(60_000));
    let mut cmd = Command::new(program);
    cmd.args(&argv).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(cwd) = opt_s(args, "cwd") {
        cmd.current_dir(cwd);
    }
    if let Some(env) = args.get("env").and_then(Value::as_object) {
        for (k, v) in env {
            if let Some(v) = v.as_str() {
                cmd.env(k, v);
            }
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| format!("{program}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        if let Some(input) = opt_s(args, "stdin") {
            let input = input.as_bytes().to_vec();
            std::thread::spawn(move || {
                let _ = stdin.write_all(&input);
            });
        }
    }
    let read_all = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut v = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut v);
            }
            v
        })
    };
    let out = read_all(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let err = read_all(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                timed_out = true;
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    Ok(json!({
        "code": status.and_then(|s| s.code()),
        "stdout": String::from_utf8_lossy(&stdout),
        "stderr": String::from_utf8_lossy(&stderr),
        "timed_out": timed_out,
    }))
}

/// Verifica che il percorso ricada in una delle radici consentite all'app.
/// `may_not_exist`: per scritture/creazioni si canonicalizza la cartella padre.
fn check_path(app: &AppDef, path: &str, may_not_exist: bool) -> Result<PathBuf, String> {
    let p = PathBuf::from(path);
    if !p.is_absolute() {
        return Err("il percorso deve essere assoluto".into());
    }
    let canon = match p.canonicalize() {
        Ok(c) => c,
        Err(e) if may_not_exist => {
            let parent = p.parent().ok_or_else(|| e.to_string())?;
            let name = p.file_name().ok_or_else(|| e.to_string())?;
            let parent = parent.canonicalize().map_err(|e| format!("{}: {e}", parent.display()))?;
            parent.join(name)
        }
        Err(e) => return Err(format!("{path}: {e}")),
    };
    if app.fs_roots.iter().any(|r| r == "*") {
        return Ok(canon);
    }
    for root in &app.fs_roots {
        if let Ok(r) = Path::new(root).canonicalize() {
            if canon.starts_with(&r) {
                return Ok(canon);
            }
        }
    }
    Err(format!("percorso fuori dalle cartelle consentite: {path} (vedi fs_roots)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> AppDef {
        serde_json::from_value(json!({"id":"a","name":"A","url":"http://x","exec_allow":["git.exe"],"fs_roots":[std::env::temp_dir()]})).unwrap()
    }

    #[test]
    fn exec_allowlist() {
        let a = app();
        assert!(program_allowed(&a, "git"));
        assert!(program_allowed(&a, "C:\\Program Files\\Git\\GIT.EXE"));
        assert!(!program_allowed(&a, "rm"));
    }

    #[test]
    fn path_roots() {
        let a = app();
        let inside = std::env::temp_dir().join("brust-test-file.txt");
        assert!(check_path(&a, inside.to_str().unwrap(), true).is_ok());
        let tricky = std::env::temp_dir().join("..").join("brust-outside.txt");
        assert!(check_path(&a, tricky.to_str().unwrap(), true).is_err());
        assert!(check_path(&a, "relative.txt", true).is_err());
    }
}
