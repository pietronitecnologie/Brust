//! Configurazione persistente: applicazioni, shell, bridge, sessione e memoria dimensioni.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::layout::Layout;

/// Un applicativo web registrato nel client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppDef {
    pub id: String,
    pub name: String,
    pub url: String,
    /// Testo breve o emoji mostrato come icona.
    #[serde(default)]
    pub icon: Option<String>,
    /// Origini aggiuntive (oltre a quella di `url`) considerate "dell'applicazione":
    /// navigazione consentita all'interno del pannello e accesso al bridge.
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// Se true (default) i link verso origini non consentite si aprono nel browser di sistema.
    /// Se false ogni navigazione resta nel pannello (utile per SSO), ma il bridge resta
    /// comunque limitato alle origini consentite.
    #[serde(default = "yes")]
    pub external_links_in_browser: bool,
    /// Capacità concesse: "exec", "open", "fs", "clipboard", "serial", "messaging".
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Programmi eseguibili con `shell.exec` (nome file, senza percorso). "*" = qualsiasi.
    #[serde(default)]
    pub exec_allow: Vec<String>,
    /// Radici del filesystem accessibili con `fs.*`. "*" = qualsiasi percorso.
    #[serde(default)]
    pub fs_roots: Vec<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub zoom: Option<f64>,
    /// File JS iniettato in ogni pagina dell'app (adattatore per app che non conoscono il bridge).
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

    /// Origini dell'applicazione (normalizzate).
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

/// Una shell/terminale avviabile in un pannello.
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
    /// Server WebSocket locale per software esterni (solo 127.0.0.1).
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Token richiesto ai client esterni (`ws://127.0.0.1:PORT/?token=...`). Generato se vuoto.
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
    /// Abilita gli strumenti sviluppatore (tasto nel pannello).
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
            shell("cmd", "Prompt dei comandi", "cmd.exe", &[]),
        ]
    } else {
        let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        vec![shell("shell", "Terminale", &sh, &["-l"])]
    }
}

/// Contenuto di un pannello (tile).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PaneContent {
    Empty,
    App {
        app: String,
        /// URL specifico all'interno dell'app (es. finestra aperta con window.open).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    Terminal { shell: String },
    Url { url: String },
}

impl PaneContent {
    /// Chiave stabile usata dalla memoria delle dimensioni.
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

/// Memoria delle proporzioni: chiave = layout + contenuti, valore = ratios.
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
        Err(e) => log::error!("serializzazione {}: {e}", path.display()),
    }
}

/// Carica la configurazione; se manca la crea con valori di esempio.
/// Restituisce anche un eventuale errore di parsing da mostrare all'utente.
pub fn load_config(paths: &Paths) -> (Config, Option<String>) {
    let path = paths.config();
    let (mut cfg, err) = match fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Config>(&text) {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(format!("Errore in {}: {e}", path.display()))),
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

/// Origine normalizzata `scheme://host[:port]` (porta di default omessa).
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
