//! Audit log of bridge activity (JSON lines, rotated at 5 MB) and remembered user grants.

use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 5 * 1024 * 1024;

pub struct Audit {
    path: PathBuf,
    enabled: bool,
}

/// Short human-readable description of a command's arguments (no file contents or clipboard text).
pub fn summarize(cmd: &str, args: &Value) -> String {
    let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match cmd {
        "shell.exec" => {
            let argv: Vec<String> = args
                .get("args")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).collect())
                .unwrap_or_default();
            format!("{} {}", s("program"), argv.join(" ")).trim().to_string()
        }
        "shell.open" => s("target"),
        c if c.starts_with("fs.") => s("path"),
        "print.file" => format!("{} → {}", s("path"), s("printer")),
        "serial.open" => format!("{} @ {}", s("port"), args.get("baud").and_then(Value::as_u64).unwrap_or(9600)),
        "serial.write" | "serial.close" => format!("handle {}", args.get("handle").and_then(Value::as_u64).unwrap_or(0)),
        "env.get" => s("name"),
        "bridge.call" => format!("{} → {}", s("target"), s("method")),
        "notes.append" | "notes.read" => s("notebook"),
        _ => String::new(),
    }
}

impl Audit {
    pub fn new(path: PathBuf, enabled: bool) -> Self {
        Audit { path, enabled }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Appends an entry. `decision`: "allowed", "confirmed", "always", "denied", "refused".
    pub fn record(&self, app: &str, cmd: &str, detail: &str, decision: &str, outcome: Option<&Result<Value, String>>) {
        if !self.enabled {
            return;
        }
        let outcome = match outcome {
            Some(Ok(_)) => json!("ok"),
            Some(Err(e)) => json!(format!("error: {e}")),
            None => Value::Null,
        };
        let entry = json!({
            "time": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            "app": app, "cmd": cmd, "detail": detail, "decision": decision, "outcome": outcome,
        });
        if fs::metadata(&self.path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
            let _ = fs::rename(&self.path, self.path.with_extension("log.1"));
        }
        if let Some(dir) = self.path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = writeln!(f, "{entry}");
        }
    }

    /// Last `n` entries, newest first.
    pub fn tail(&self, n: usize) -> Vec<Value> {
        let text = fs::read_to_string(&self.path).unwrap_or_default();
        text.lines().rev().take(n).filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// "Always allow" decisions, stored as keys `app|cmd|detail-key`.
pub struct Grants {
    path: PathBuf,
    set: BTreeSet<String>,
}

impl Grants {
    pub fn load(path: PathBuf) -> Self {
        let set = crate::config::load_json::<BTreeSet<String>>(&path).unwrap_or_default();
        Grants { path, set }
    }

    /// Key that identifies what was allowed: for exec the program name, for other commands the command.
    pub fn key(app: &str, cmd: &str, args: &Value) -> String {
        let detail = match cmd {
            "shell.exec" => args
                .get("program")
                .and_then(Value::as_str)
                .map(|p| p.rsplit(['/', '\\']).next().unwrap_or(p).to_ascii_lowercase())
                .unwrap_or_default(),
            "serial.open" => args.get("port").and_then(Value::as_str).unwrap_or("").to_string(),
            _ => String::new(),
        };
        format!("{app}|{cmd}|{detail}")
    }

    pub fn contains(&self, key: &str) -> bool {
        self.set.contains(key)
    }

    pub fn insert(&mut self, key: String) {
        self.set.insert(key);
        crate::config::save_json(&self.path, &self.set);
    }

    pub fn list(&self) -> Vec<String> {
        self.set.iter().cloned().collect()
    }

    pub fn remove(&mut self, key: &str) {
        self.set.remove(key);
        crate::config::save_json(&self.path, &self.set);
    }

    pub fn clear(&mut self) {
        self.set.clear();
        crate::config::save_json(&self.path, &self.set);
    }
}

/// Commands that need user confirmation when `confirm_sensitive` is on.
pub fn is_sensitive(cmd: &str) -> bool {
    matches!(cmd, "shell.exec" | "shell.open" | "fs.write" | "fs.remove" | "fs.mkdir" | "serial.open" | "print.file")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_roundtrip_and_summary() {
        let dir = std::env::temp_dir().join(format!("brust-audit-{}", std::process::id()));
        let a = Audit::new(dir.join("audit.log"), true);
        let args = json!({"program": "git", "args": ["status", "-s"]});
        assert_eq!(summarize("shell.exec", &args), "git status -s");
        a.record("ordo", "shell.exec", &summarize("shell.exec", &args), "allowed", Some(&Ok(Value::Null)));
        a.record("ordo", "fs.read", "C:/x", "denied", None);
        let t = a.tail(10);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0]["cmd"], "fs.read");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn grant_keys() {
        assert_eq!(Grants::key("ordo", "shell.exec", &json!({"program": "C:\\x\\Git.EXE"})), "ordo|shell.exec|git.exe");
        assert!(is_sensitive("fs.write") && !is_sensitive("fs.read"));
    }
}
