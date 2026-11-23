//! Persistent configuration: apps, shells, bridge, session and size memory.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::layout::Layout;

/// A web app registered in the client.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppDef {
    pub id: String,
    pub name: String,
    pub url: String,
    /// Short text or emoji shown as the icon.
    #[serde(default)]
    pub icon: Option<String>,
    /// Extra origins (besides the one of `url`) treated as "belonging to the app":
    /// navigation is allowed inside the pane and they get bridge access.
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// If true (default), links to non-allowed origins open in the system browser.
    /// If false, all navigation stays in the pane (useful for SSO), but the bridge is
    /// still limited to the allowed origins.
    #[serde(default = "yes")]
    pub external_links_in_browser: bool,
    /// Granted capabilities: "exec", "open", "fs", "clipboard", "serial", "messaging".
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Programs runnable with `shell.exec` (file name, without path). "*" = any.
    #[serde(default)]
    pub exec_allow: Vec<String>,
    /// Filesystem roots accessible with `fs.*`. "*" = any path.
    #[serde(default)]
    pub fs_roots: Vec<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub zoom: Option<f64>,
    /// JS file injected into every page of the app (adapter for apps unaware of the bridge).
    #[serde(default)]
    pub inject_script: Option<String>,

    /* ---- privacy ---- */
    /// Own cookie/storage profile, separate from other apps (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolated: Option<bool>,
    /// Delete the app's cookies/storage when Brust exits.
    #[serde(default)]
    pub clear_on_exit: bool,
    /// Never store cookies/storage on disk (private mode).
    #[serde(default)]
    pub incognito: bool,
    /// Block trackers/ads (overrides `privacy.block_trackers`).
    #[serde(default)]
    pub block_trackers: Option<bool>,
    /// Ask the user before sensitive bridge commands (overrides `privacy.confirm_sensitive`).
    #[serde(default)]
    pub confirm: Option<bool>,
    /// Browser permissions: name -> "allow" | "deny" | "default".
    /// Names: camera, microphone, geolocation, notifications, clipboard_read, display_capture,
    /// midi, sensors, local_fonts, window_management, pointer_lock, automatic_downloads,
    /// file_system_access, autoplay. Camera, microphone, geolocation and display capture are
    /// denied unless allowed here.
    #[serde(default)]
    pub web_permissions: HashMap<String, String>,
}


fn yes() -> bool {
    true
}

impl AppDef {
    pub fn is_isolated(&self) -> bool {
        self.isolated.unwrap_or(true)
    }

    pub fn has(&self, perm: &str) -> bool {
        self.permissions.iter().any(|p| p == perm || p == "*")
    }

    /// The app's origins (normalized).
    pub fn origins(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        if let Some(o) = origin_of(&self.url) {
            v.push(o);
        }
        for o in &self.allowed_origins {
            if let Some(o) = origin_of(o) {
                v.push(o);
            }
        }
        v
    }

    pub fn owns_url(&self, url: &str) -> bool {
        match origin_of(url) {
            Some(o) => self.origins().contains(&o),
            None => false,
        }
    }
}

/// A shell/terminal that can be started in a pane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellDef {
    pub id: String,
    pub name: String,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeConfig {
    /// Local WebSocket server for external software (127.0.0.1 only).
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Token required from external clients (`ws://127.0.0.1:PORT/?token=...`). Generated if empty.
    #[serde(default)]
    pub token: String,
}

fn default_port() -> u16 {
    47800
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self { enabled: true, port: default_port(), token: String::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyConfig {
    /// Block trackers/ads for apps and web panes (through a local filtering proxy).
    #[serde(default)]
    pub block_trackers: bool,
    /// Extra block lists (hosts files or one domain per line).
    #[serde(default)]
    pub blocklists: Vec<String>,
    #[serde(default)]
    pub block_domains: Vec<String>,
    #[serde(default)]
    pub allow_domains: Vec<String>,
    /// Ask before exec, file writes/deletes, serial ports, opening files and printing files.
    #[serde(default = "yes")]
    pub confirm_sensitive: bool,
    /// Record bridge activity in audit.log.
    #[serde(default = "yes")]
    pub audit_log: bool,
    /// Lock Brust (PIN) after this many idle minutes; 0 = never. Requires a PIN (Settings).
    #[serde(default)]
    pub lock_after_minutes: u32,
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        serde_json::from_str("{}").unwrap()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NotesConfig {
    /// Folder for notebooks (Markdown files). Default: Documents/Brust Notes.
    #[serde(default)]
    pub dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KioskConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Template opened at start (replaces the saved session).
    #[serde(default)]
    pub template: Option<String>,
}

/// Central configuration: a signed file (see `brust --sign`) whose keys override config.json.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RemoteConfig {
    #[serde(default)]
    pub url: String,
    /// Base64 ed25519 public key that must have signed the file.
    #[serde(default)]
    pub public_key: String,
    /// Re-check every N minutes (0 = only at start).
    #[serde(default)]
    pub refresh_minutes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateConfig {
    /// Signed update manifest (see GUIDE.md).
    #[serde(default)]
    pub manifest_url: String,
    #[serde(default)]
    pub public_key: String,
    #[serde(default)]
    pub check_on_start: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub apps: Vec<AppDef>,
    #[serde(default)]
    pub shells: Vec<ShellDef>,
    #[serde(default)]
    pub bridge: BridgeConfig,
    /// Enables developer tools (button in the pane).
    #[serde(default)]
    pub devtools: bool,
    #[serde(default)]
    pub privacy: PrivacyConfig,
    #[serde(default)]
    pub notes: NotesConfig,
    #[serde(default)]
    pub kiosk: KioskConfig,
    #[serde(default)]
    pub remote_config: Option<RemoteConfig>,
    #[serde(default)]
    pub update: Option<UpdateConfig>,
    /// Register the brust:// link scheme for the current user (Windows).
    #[serde(default = "yes")]
    pub register_protocol: bool,
    /// Default printer for print commands (system default if empty).
    #[serde(default)]
    pub default_printer: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            apps: vec![
                AppDef {
                    id: "crm".into(),
                    name: "CRM".into(),
                    url: "http://localhost:3000".into(),
                    icon: Some("📦".into()),
                    external_links_in_browser: true,
                    permissions: vec!["messaging".into(), "clipboard".into(), "open".into(), "notes".into()],
                    ..Default::default()
                },
                AppDef {
                    id: "erp".into(),
                    name: "ERP".into(),
                    url: "http://localhost:8080".into(),
                    icon: Some("📐".into()),
                    external_links_in_browser: true,
                    permissions: vec!["messaging".into(), "clipboard".into(), "open".into(), "notes".into()],
                    ..Default::default()
                },
            ],
            shells: default_shells(),
            bridge: BridgeConfig::default(),
            devtools: cfg!(debug_assertions),
            privacy: PrivacyConfig::default(),
            notes: NotesConfig::default(),
            kiosk: KioskConfig::default(),
            remote_config: None,
            update: None,
            register_protocol: true,
            default_printer: None,
        }
    }
}

fn shell(id: &str, name: &str, program: &str, args: &[&str]) -> ShellDef {
    ShellDef {
        id: id.into(),
        name: name.into(),
        program: program.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        cwd: None,
        env: HashMap::new(),
    }
}

fn default_shells() -> Vec<ShellDef> {
    if cfg!(windows) {
        vec![
            shell("powershell", "PowerShell", "powershell.exe", &["-NoLogo"]),
            shell("cmd", "Command Prompt", "cmd.exe", &[]),
        ]
    } else {
        let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        vec![shell("shell", "Terminal", &sh, &["-l"])]
    }
}

/// Content of a pane (tile).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PaneContent {
    Empty,
    App {
        app: String,
        /// Specific URL within the app (e.g. a window opened with window.open).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    Terminal { shell: String },
    Url {
        url: String,
        /// Private pane: nothing stored on disk.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        private: bool,
    },
    Notes { notebook: String },
}

impl PaneContent {
    /// Stable key used by the size memory.
    pub fn key(&self) -> String {
        match self {
            PaneContent::Empty => "-".into(),
            PaneContent::App { app, .. } => format!("app:{app}"),
            PaneContent::Terminal { shell } => format!("term:{shell}"),
            PaneContent::Url { url, .. } => format!("url:{}", origin_of(url).unwrap_or_default()),
            PaneContent::Notes { .. } => "notes".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedTab {
    #[serde(default)]
    pub title: Option<String>,
    pub layout: Layout,
    #[serde(default)]
    pub ratios: Vec<f64>,
    pub panes: Vec<PaneContent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Session {
    pub tabs: Vec<SavedTab>,
    #[serde(default)]
    pub active: usize,
}

/// Proportion memory: key = layout + contents, value = ratios.
pub type SizeMemory = HashMap<String, Vec<f64>>;

pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn new() -> Self {
        let dirs = directories::ProjectDirs::from("it", "Pietroni", "Brust");
        let (config_dir, data_dir) = match dirs {
            Some(d) => (d.config_dir().to_path_buf(), d.data_local_dir().to_path_buf()),
            None => (PathBuf::from("brust-config"), PathBuf::from("brust-data")),
        };
        let _ = fs::create_dir_all(&config_dir);
        let _ = fs::create_dir_all(&data_dir);
        Paths { config_dir, data_dir }
    }
    pub fn config(&self) -> PathBuf {
        self.config_dir.join("config.json")
    }
    pub fn session(&self) -> PathBuf {
        self.config_dir.join("session.json")
    }
    pub fn sizes(&self) -> PathBuf {
        self.config_dir.join("sizes.json")
    }
    /// Browser profile folder (cookies, storage) for a profile key.
    pub fn profile(&self, key: &str) -> PathBuf {
        self.data_dir.join("profiles").join(key)
    }
    pub fn templates(&self) -> PathBuf {
        self.config_dir.join("templates.json")
    }
    pub fn grants(&self) -> PathBuf {
        self.config_dir.join("grants.json")
    }
    pub fn remote_cache(&self) -> PathBuf {
        self.config_dir.join("remote-config.cache.json")
    }
    pub fn token_secret(&self) -> PathBuf {
        self.config_dir.join("bridge-token.dat")
    }
    pub fn pin_secret(&self) -> PathBuf {
        self.config_dir.join("pin.dat")
    }
    pub fn audit_log(&self) -> PathBuf {
        self.data_dir.join("audit.log")
    }
}

pub fn load_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let text = fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            log::error!("{}: {e}", path.display());
            None
        }
    }
}

pub fn save_json<T: Serialize>(path: &Path, value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(text) => {
            let tmp = path.with_extension("tmp");
            if fs::write(&tmp, text).is_ok() {
                let _ = fs::rename(&tmp, path);
            }
        }
        Err(e) => log::error!("serializing {}: {e}", path.display()),
    }
}

/// Loads the configuration; if missing, creates it with sample values.
/// `remote` (verified central configuration) overrides top-level keys of the local file.
/// Also returns any parse error to show to the user.
pub fn load_config(paths: &Paths, remote: Option<&serde_json::Value>) -> (Config, Option<String>) {
    let path = paths.config();
    let local: Result<serde_json::Value, String> = match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("Error in {}: {e}", path.display())),
        Err(_) => {
            let c = Config::default();
            save_json(&path, &c);
            Ok(serde_json::to_value(&c).unwrap_or_default())
        }
    };
    let (mut cfg, err) = match local {
        Ok(mut v) => {
            if let (Some(obj), Some(serde_json::Value::Object(r))) = (v.as_object_mut(), remote) {
                for (k, val) in r {
                    if k != "remote_config" {
                        obj.insert(k.clone(), val.clone());
                    }
                }
            }
            match serde_json::from_value::<Config>(v) {
                Ok(c) => (c, None),
                Err(e) => (Config::default(), Some(format!("Error in configuration: {e}"))),
            }
        }
        Err(e) => (Config::default(), Some(e)),
    };
    if cfg.shells.is_empty() {
        cfg.shells = default_shells();
    }
    (cfg, err)
}

/// Random hex token from the OS random generator.
pub fn random_token() -> String {
    use rand_core::RngCore;
    let mut b = [0u8; 24];
    rand_core::OsRng.fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Default notes folder.
pub fn default_notes_dir(paths: &Paths) -> PathBuf {
    directories::UserDirs::new()
        .and_then(|u| u.document_dir().map(|d| d.join("Brust Notes")))
        .unwrap_or_else(|| paths.data_dir.join("notes"))
}

/// A saved tileset that can be reopened from the palette, the command line or kiosk mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Template {
    pub name: String,
    pub tab: SavedTab,
}

/// Normalized origin `scheme://host[:port]` (default port omitted).
pub fn origin_of(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or(authority).to_ascii_lowercase();
    if authority.is_empty() {
        return None;
    }
    let authority = match (scheme.as_str(), authority.rsplit_once(':')) {
        ("http", Some((h, "80"))) | ("https", Some((h, "443"))) => h.to_string(),
        _ => authority,
    };
    Some(format!("{scheme}://{authority}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins() {
        assert_eq!(origin_of("https://Ex.com:443/a?b").as_deref(), Some("https://ex.com"));
        assert_eq!(origin_of("http://localhost:3000/x").as_deref(), Some("http://localhost:3000"));
        assert_eq!(origin_of("http://u:p@host/x").as_deref(), Some("http://host"));
        assert_eq!(origin_of("about:blank"), None);
    }
}
