//! Persistent configuration: apps, shells, bridge, session and size memory.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::layout::Layout;

/// A web app registered in the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

fn yes() -> bool {
    true
}

impl AppDef {
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
}

impl Default for Config {
    fn default() -> Self {
        Config {
            apps: vec![
                AppDef {
                    id: "ordo".into(),
                    name: "Ordo".into(),
                    url: "http://localhost:3000".into(),
                    icon: Some("📦".into()),
                    allowed_origins: vec![],
                    external_links_in_browser: true,
                    permissions: vec!["messaging".into(), "clipboard".into(), "open".into()],
                    exec_allow: vec![],
                    fs_roots: vec![],
                    user_agent: None,
                    zoom: None,
                    inject_script: None,
                },
                AppDef {
                    id: "metodo".into(),
                    name: "Metodo".into(),
                    url: "http://localhost:8080".into(),
                    icon: Some("📐".into()),
                    allowed_origins: vec![],
                    external_links_in_browser: true,
                    permissions: vec!["messaging".into(), "clipboard".into(), "open".into()],
                    exec_allow: vec![],
                    fs_roots: vec![],
                    user_agent: None,
                    zoom: None,
                    inject_script: None,
                },
            ],
            shells: default_shells(),
            bridge: BridgeConfig::default(),
            devtools: cfg!(debug_assertions),
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
    Url { url: String },
}

impl PaneContent {
    /// Stable key used by the size memory.
    pub fn key(&self) -> String {
        match self {
            PaneContent::Empty => "-".into(),
            PaneContent::App { app, .. } => format!("app:{app}"),
            PaneContent::Terminal { shell } => format!("term:{shell}"),
            PaneContent::Url { url } => format!("url:{}", origin_of(url).unwrap_or_default()),
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
    pub fn webdata(&self) -> PathBuf {
        self.data_dir.join("webdata")
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
/// Also returns any parse error to show to the user.
pub fn load_config(paths: &Paths) -> (Config, Option<String>) {
    let path = paths.config();
    let (mut cfg, err) = match fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Config>(&text) {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(format!("Error in {}: {e}", path.display()))),
        },
        Err(_) => {
            let c = Config::default();
            save_json(&path, &c);
            (c, None)
        }
    };
    if cfg.shells.is_empty() {
        cfg.shells = default_shells();
    }
    if cfg.bridge.token.is_empty() && err.is_none() {
        cfg.bridge.token = random_token();
        save_json(&path, &cfg);
    }
    (cfg, err)
}

pub fn random_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for i in 0..2u64 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
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
