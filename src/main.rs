#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Brust – dedicated desktop client for web apps.
//!
//! * tabs, each with a tileset of 1–4 resizable panes;
//! * each pane shows an app, a URL or a terminal (PowerShell, cmd, …);
//! * JS bridge (`window.brust`) to local capabilities and to external software (WebSocket).

#[allow(dead_code)]
mod adblock;
mod assets;
mod audit;
mod cli;
mod config;
mod layout;
mod locals;
#[allow(dead_code)]
mod notes;
#[allow(dead_code)]
mod print;
mod pty;
#[allow(dead_code)]
mod secrets;
#[allow(dead_code)]
mod trust;
#[allow(dead_code)]
mod update;
mod ws;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tao::dpi::{LogicalPosition, LogicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Fullscreen, Window, WindowBuilder};
use wry::{
    NewWindowResponse, PageLoadEvent, PermissionKind, PermissionResponse, ProxyConfig, ProxyEndpoint, Rect, WebContext,
    WebView, WebViewBuilder,
};

use audit::{Audit, Grants};
use cli::Launch;
use config::{AppDef, Config, PaneContent, Paths, SavedTab, Session, SizeMemory, Template};
use layout::{Layout, R};
use locals::Locals;
use pty::PtySession;
use ws::WsEvent;

const BAR_H: f64 = 38.0;
const HEADER_H: f64 = 26.0;
const GAP: f64 = 6.0;
const PAD: f64 = 5.0;

pub enum UserEvent {
    ShellIpc(String),
    PaneIpc { pane: u32, url: String, body: String },
    PtyOutput { pane: u32, data: Vec<u8> },
    PtyExit { pane: u32 },
    TaskDone { pane: u32, id: u64, result: Result<Value, String> },
    SerialData { pane: u32, handle: u32, data: Vec<u8> },
    SerialClosed { pane: u32, handle: u32, error: Option<String> },
    Title { pane: u32, title: String },
    Loaded { pane: u32, url: String },
    NewWindow { pane: u32, url: String },
    Ws(WsEvent),
    /// Periodic timer (idle lock, remote configuration refresh).
    Tick,
    RemoteConfig(Result<Value, String>),
    UpdateChecked(Result<Option<update::UpdateInfo>, String>),
    UpdateDownloaded(Result<PathBuf, String>),
    /// Result of a local command, also recorded in the audit log.
    Audited { pane: u32, id: u64, app: String, cmd: String, detail: String, decision: String, result: Result<Value, String> },
}

/* ------------------------------------------------------------------ host */

/// Window hosting the child webviews (on Linux via a gtk::Fixed).
struct Host {
    window: Window,
    #[cfg(target_os = "linux")]
    fixed: gtk::Fixed,
}

impl Host {
    fn new(window: Window) -> Self {
        #[cfg(target_os = "linux")]
        {
            use gtk::prelude::*;
            use tao::platform::unix::WindowExtUnix;
            let vbox = window.default_vbox().expect("default_vbox");
            let fixed = gtk::Fixed::new();
            vbox.pack_start(&fixed, true, true, 0);
            fixed.show_all();
            Host { window, fixed }
        }
        #[cfg(not(target_os = "linux"))]
        Host { window }
    }

    fn build<'a>(&'a self, b: WebViewBuilder<'a>) -> wry::Result<WebView> {
        #[cfg(target_os = "linux")]
        {
            use wry::WebViewBuilderExtUnix;
            b.build_gtk(&self.fixed)
        }
        #[cfg(not(target_os = "linux"))]
        b.build_as_child(&self.window)
    }

    fn place(&self, wv: &WebView, r: R) {
        let _ = wv.set_bounds(rect(r));
        #[cfg(target_os = "linux")]
        {
            use gtk::prelude::*;
            use wry::WebViewExtUnix;
            let w = wv.webview();
            self.fixed.move_(&w, r.x as i32, r.y as i32);
            w.set_size_request(r.w.max(1.0) as i32, r.h.max(1.0) as i32);
        }
    }

    /// Logical size of the client area.
    fn size(&self) -> (f64, f64) {
        let s = self.window.inner_size().to_logical::<f64>(self.window.scale_factor());
        (s.width, s.height)
    }
}

fn rect(r: R) -> Rect {
    Rect {
        position: LogicalPosition::new(r.x, r.y).into(),
        size: LogicalSize::new(r.w.max(1.0), r.h.max(1.0)).into(),
    }
}

/* ----------------------------------------------------------------- model */

struct Pane {
    id: u32,
    content: PaneContent,
    view: Option<WebView>,
    doc_title: String,
    custom_title: Option<String>,
    picking: bool,
    pty: Option<PtySession>,
    pty_exited: bool,
    /// Unread counter set by the app with `ui.badge`.
    badge: u32,
}

impl Pane {
    fn new(id: u32, content: PaneContent) -> Self {
        Pane {
            id,
            content,
            view: None,
            doc_title: String::new(),
            custom_title: None,
            picking: false,
            pty: None,
            pty_exited: false,
            badge: 0,
        }
    }
}

struct Tab {
    id: u32,
    // `panes` may hold more panes than the layout shows:
    // the extra ones stay "parked" (hidden but alive) and
    // reappear when switching back to a layout with more tiles.
    title: Option<String>,
    layout: Layout,
    ratios: Vec<f64>,
    panes: Vec<Pane>,
    focused: usize,
    /// Set for a pane detached into its own window (not shown in the tab bar).
    window: Option<Window>,
}

impl Tab {
    fn new(id: u32, title: Option<String>, layout: Layout, ratios: Vec<f64>, panes: Vec<Pane>) -> Self {
        Tab { id, title, layout, ratios, panes, focused: 0, window: None }
    }

    fn is_floating(&self) -> bool {
        self.window.is_some()
    }

    /// Panes shown by the current layout.
    fn visible(&self) -> &[Pane] {
        &self.panes[..self.layout.pane_count().min(self.panes.len())]
    }
}

enum Caller {
    Pane { pane: u32, id: u64 },
    Ws { client: u32, id: Value },
}

struct WsClient {
    name: String,
    tx: Sender<String>,
}

/// A sensitive bridge command waiting for the user's decision.
struct Confirm {
    seq: u64,
    pane: u32,
    id: u64,
    app: AppDef,
    cmd: String,
    args: Value,
    detail: String,
}

struct App {
    host: Host,
    /// Browser profiles (cookies/storage), one per profile key.
    contexts: HashMap<String, WebContext>,
    shell: Option<WebView>,
    paths: Paths,
    config: Config,
    config_error: Option<String>,
    tabs: Vec<Tab>,
    active: usize,
    sizes: SizeMemory,
    next_id: u32,
    proxy: EventLoopProxy<UserEvent>,
    locals: Arc<Locals>,
    overlay: bool,
    clients: HashMap<u32, WsClient>,
    calls: HashMap<u64, Caller>,
    /// Pending calls to external software: call id → WebSocket client expected to reply.
    calls_to_ws: HashMap<u64, u32>,
    next_call: u64,
    bridge_status: String,
    audit: Audit,
    grants: Grants,
    confirms: VecDeque<Confirm>,
    next_confirm: u64,
    notes: notes::Notes,
    templates: Vec<Template>,
    adblock: Option<adblock::ProxyHandle>,
    remote: Option<Value>,
    remote_status: String,
    update_info: Option<update::UpdateInfo>,
    update_status: String,
    locked: bool,
    kiosk_unlocked: bool,
    last_activity: Instant,
    ticks: u64,
    /// Panes to detach into their own window (handled where the event loop target is available).
    pending_detach: Vec<u32>,
    exit_requested: bool,
}

const INTERNAL_PROFILE: &str = "internal";

/// Window icon (64×64 RGBA, generated from assets/icon/brust.svg).
fn window_icon() -> Option<tao::window::Icon> {
    tao::window::Icon::from_rgba(include_bytes!("../assets/icon/brust-64.rgba").to_vec(), 64, 64).ok()
}

fn permission_name(kind: &PermissionKind) -> &'static str {
    match kind {
        PermissionKind::Microphone => "microphone",
        PermissionKind::Camera => "camera",
        PermissionKind::Geolocation => "geolocation",
        PermissionKind::Notifications => "notifications",
        PermissionKind::ClipboardRead => "clipboard_read",
        PermissionKind::DisplayCapture => "display_capture",
        PermissionKind::Midi => "midi",
        PermissionKind::Sensors => "sensors",
        PermissionKind::MediaKeySystemAccess => "media_key_system_access",
        PermissionKind::LocalFonts => "local_fonts",
        PermissionKind::WindowManagement => "window_management",
        PermissionKind::PointerLock => "pointer_lock",
        PermissionKind::AutomaticDownloads => "automatic_downloads",
        PermissionKind::FileSystemAccess => "file_system_access",
        PermissionKind::Autoplay => "autoplay",
        #[allow(unreachable_patterns)]
        _ => "other",
    }
}

/// Browser permission policy: the configured value, otherwise deny camera, microphone,
/// location and screen capture and leave the rest to the browser default.
fn permission_policy(perms: &HashMap<String, String>, kind: &PermissionKind) -> PermissionResponse {
    let name = permission_name(kind);
    match perms.get(name).map(String::as_str) {
        Some("allow") => PermissionResponse::Allow,
        Some("deny") => PermissionResponse::Deny,
        Some(_) => PermissionResponse::Default,
        None => match name {
            "camera" | "microphone" | "geolocation" | "display_capture" => PermissionResponse::Deny,
            _ => PermissionResponse::Default,
        },
    }
}

/// Safe folder name for a profile key.
fn profile_dir_name(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// Builds a webview filling a detached window.
fn build_floating<'a>(b: WebViewBuilder<'a>, w: &'a Window) -> wry::Result<WebView> {
    #[cfg(target_os = "linux")]
    {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        match w.default_vbox() {
            Some(vbox) => b.build_gtk(vbox),
            None => b.build(w),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let size = w.inner_size().to_logical::<f64>(w.scale_factor());
        b.with_bounds(rect(R::new(0.0, 0.0, size.width, size.height))).build_as_child(w)
    }
}

fn ok_reply(id: u64, result: Result<Value, String>) -> Value {
    match result {
        Ok(v) => json!({"t": "reply", "id": id, "ok": true, "result": v}),
        Err(e) => json!({"t": "reply", "id": id, "ok": false, "error": e}),
    }
}

impl App {
    fn new(host: Host, proxy: EventLoopProxy<UserEvent>, launch: Vec<Launch>) -> Self {
        let paths = Paths::new();
        let remote = config::load_json::<Value>(&paths.remote_cache());
        let (mut config, config_error) = config::load_config(&paths, remote.as_ref());
        Self::resolve_token(&paths, &mut config);
        Self::clear_profiles(&paths, &config);
        let sizes: SizeMemory = config::load_json(&paths.sizes()).unwrap_or_default();
        let locals = Locals::new(proxy.clone());
        let audit = Audit::new(paths.audit_log(), config.privacy.audit_log);
        let grants = Grants::load(paths.grants());
        let notes_dir = config.notes.dir.clone().map(PathBuf::from).unwrap_or_else(|| config::default_notes_dir(&paths));
        let notes = notes::Notes::new(notes_dir);
        let templates: Vec<Template> = config::load_json(&paths.templates()).unwrap_or_default();
        let adblock = Self::start_adblock(&config);
        let mut app = App {
            host,
            contexts: HashMap::new(),
            shell: None,
            paths,
            config,
            config_error,
            tabs: Vec::new(),
            active: 0,
            sizes,
            next_id: 1,
            proxy,
            locals,
            overlay: false,
            clients: HashMap::new(),
            calls: HashMap::new(),
            calls_to_ws: HashMap::new(),
            next_call: 1,
            bridge_status: "not started".into(),
            audit,
            grants,
            confirms: VecDeque::new(),
            next_confirm: 1,
            notes,
            templates,
            adblock,
            remote,
            remote_status: String::new(),
            update_info: None,
            update_status: String::new(),
            locked: false,
            kiosk_unlocked: false,
            last_activity: Instant::now(),
            ticks: 0,
            pending_detach: Vec::new(),
            exit_requested: false,
        };
        app.remote_status = match (&app.config.remote_config, &app.remote) {
            (None, _) => "not configured".into(),
            (Some(_), Some(_)) => "using cached copy".into(),
            (Some(_), None) => "not loaded yet".into(),
        };
        app.start_bridge();
        app.build_shell();
        let kiosk_template = app.config.kiosk.template.clone().filter(|_| app.config.kiosk.enabled);
        if app.config.kiosk.enabled {
            app.host.window.set_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        match kiosk_template.and_then(|n| app.templates.iter().find(|t| t.name == n).cloned()) {
            Some(t) => {
                app.push_saved_tab(t.tab);
                app.active = usize::MAX;
                app.activate(0);
            }
            None => app.restore_session(),
        }
        for l in launch {
            app.launch(l);
        }
        app.start_background_jobs();
        app.apply_theme();
        app
    }

    /// Bridge token: explicit value in config.json, otherwise a generated one stored
    /// encrypted for the current user (Windows DPAPI).
    fn resolve_token(paths: &Paths, config: &mut Config) {
        if config.bridge.token.is_empty() {
            config.bridge.token = secrets::load_secret(&paths.token_secret()).unwrap_or_else(|| {
                let t = config::random_token();
                if let Err(e) = secrets::save_secret(&paths.token_secret(), &t) {
                    log::warn!("cannot store the bridge token: {e}");
                }
                t
            });
        }
    }

    /// Deletes the stored browser data of apps marked `clear_on_exit`
    /// (also at start, in case the previous run did not exit cleanly).
    fn clear_profiles(paths: &Paths, config: &Config) {
        for a in config.apps.iter().filter(|a| a.clear_on_exit && a.is_isolated()) {
            let base = format!("app-{}", profile_dir_name(&a.id));
            for key in [base.clone(), format!("{base}-b")] {
                let dir = paths.profile(&key);
                if dir.exists()
                    && let Err(e) = std::fs::remove_dir_all(&dir) {
                        log::warn!("cannot clear {}: {e}", dir.display());
                    }
            }
        }
    }

    fn start_adblock(config: &Config) -> Option<adblock::ProxyHandle> {
        let wanted = config.privacy.block_trackers || config.apps.iter().any(|a| a.block_trackers == Some(true));
        if !wanted {
            return None;
        }
        let mut blocker = adblock::Blocker::new(&config.privacy.block_domains, &config.privacy.allow_domains);
        for list in &config.privacy.blocklists {
            match std::fs::read_to_string(list) {
                Ok(text) => blocker.load_list(&text),
                Err(e) => log::warn!("block list {list}: {e}"),
            }
        }
        match adblock::start(blocker) {
            Ok(h) => {
                log::info!("tracker blocking proxy on 127.0.0.1:{}", h.port);
                Some(h)
            }
            Err(e) => {
                log::error!("tracker blocking: {e}");
                None
            }
        }
    }

    /// Timer, central configuration, update check and link-scheme registration.
    fn start_background_jobs(&mut self) {
        let p = self.proxy.clone();
        std::thread::spawn(move || {
            while p.send_event(UserEvent::Tick).is_ok() {
                std::thread::sleep(Duration::from_secs(20));
            }
        });
        self.fetch_remote_config();
        if self.config.update.as_ref().is_some_and(|u| u.check_on_start && !u.manifest_url.is_empty()) {
            self.check_update();
        }
        if self.config.register_protocol {
            std::thread::spawn(cli::register_protocol);
        }
    }

    fn fetch_remote_config(&self) {
        let Some(rc) = self.config.remote_config.clone().filter(|r| !r.url.is_empty()) else { return };
        let p = self.proxy.clone();
        std::thread::spawn(move || {
            let r = if rc.public_key.is_empty() {
                Err("remote_config.public_key is missing".to_string())
            } else {
                trust::fetch_verified_json(&rc.url, &rc.public_key)
            };
            let _ = p.send_event(UserEvent::RemoteConfig(r));
        });
    }

    fn check_update(&mut self) {
        let Some(u) = self.config.update.clone().filter(|u| !u.manifest_url.is_empty()) else {
            self.update_status = "not configured".into();
            return;
        };
        self.update_status = "checking…".into();
        let p = self.proxy.clone();
        std::thread::spawn(move || {
            let r = update::check(&u.manifest_url, &u.public_key, env!("CARGO_PKG_VERSION"));
            let _ = p.send_event(UserEvent::UpdateChecked(r));
        });
    }

    fn start_bridge(&mut self) {
        let b = &self.config.bridge;
        if !b.enabled {
            self.bridge_status = "disabled".into();
            return;
        }
        self.bridge_status = match ws::start(b.port, b.token.clone(), self.proxy.clone()) {
            Ok(()) => format!("listening on 127.0.0.1:{}", b.port),
            Err(e) => format!("error: {e}"),
        };
        log::info!("bridge: {}", self.bridge_status);
    }

    fn id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn build_shell(&mut self) {
        let proxy = self.proxy.clone();
        let (w, h) = self.host.size();
        let paths = &self.paths;
        let ctx = self
            .contexts
            .entry(INTERNAL_PROFILE.into())
            .or_insert_with(|| WebContext::new(Some(paths.profile(INTERNAL_PROFILE))));
        let builder = WebViewBuilder::new_with_web_context(ctx)
            .with_custom_protocol(assets::SCHEME.into(), assets::handle)
            .with_url(assets::url("shell.html"))
            .with_bounds(rect(R::new(0.0, 0.0, w, h)))
            .with_background_color((21, 23, 28, 255))
            .with_devtools(self.config.devtools && !self.config.kiosk.enabled)
            .with_hotkeys_zoom(false)
            .with_ipc_handler(move |req| {
                let _ = proxy.send_event(UserEvent::ShellIpc(req.into_body()));
            });
        match self.host.build(builder) {
            Ok(v) => self.shell = Some(v),
            Err(e) => panic!("cannot create the UI: {e}"),
        }
    }

    /* ------------------------------------------------------------- session */

    fn restore_session(&mut self) {
        let session: Session = config::load_json(&self.paths.session()).unwrap_or_default();
        for t in session.tabs {
            let mut panes: Vec<Pane> = Vec::new();
            for c in t.panes.into_iter().take(4) {
                let id = self.id();
                panes.push(Pane::new(id, c));
            }
            while panes.len() < t.layout.pane_count() {
                let id = self.id();
                panes.push(Pane::new(id, PaneContent::Empty));
            }
            let id = self.id();
            self.tabs.push(Tab::new(id, t.title, t.layout, t.layout.sanitize(&t.ratios), panes));
        }
        if self.tabs.is_empty() {
            self.push_tab(PaneContent::Empty);
        }
        self.active = session.active.min(self.tabs.len() - 1);
        self.activate(self.active);
    }

    /// Appends a tab built from a saved description (session or template).
    fn push_saved_tab(&mut self, t: SavedTab) {
        let mut panes: Vec<Pane> = Vec::new();
        for c in t.panes.into_iter().take(4) {
            let id = self.id();
            panes.push(Pane::new(id, c));
        }
        while panes.len() < t.layout.pane_count() {
            let id = self.id();
            panes.push(Pane::new(id, PaneContent::Empty));
        }
        let id = self.id();
        self.tabs.push(Tab::new(id, t.title, t.layout, t.layout.sanitize(&t.ratios), panes));
    }

    fn saved_tab(t: &Tab) -> SavedTab {
        SavedTab { title: t.title.clone(), layout: t.layout, ratios: t.ratios.clone(), panes: t.panes.iter().map(|p| p.content.clone()).collect() }
    }

    fn save_session(&self) {
        let s = Session {
            active: self.active,
            tabs: self.tabs.iter().map(Self::saved_tab).collect(),
        };
        config::save_json(&self.paths.session(), &s);
    }

    /* ---------------------------------------------------- tile memory */

    fn size_keys(t: &Tab) -> (String, String) {
        let all: Vec<String> = t.visible().iter().map(|p| p.content.key()).collect();
        (
            format!("{}|{}", t.layout.id(), all.join(",")),
            format!("{}|first:{}", t.layout.id(), all.first().cloned().unwrap_or_default()),
        )
    }

    /// Applies the remembered proportions for this combination of layout and apps.
    fn recall_sizes(&mut self, ti: usize, reset_if_unknown: bool) {
        let (k1, k2) = Self::size_keys(&self.tabs[ti]);
        let t = &mut self.tabs[ti];
        if let Some(r) = self.sizes.get(&k1).or_else(|| self.sizes.get(&k2)) {
            t.ratios = t.layout.sanitize(r);
        } else if reset_if_unknown {
            t.ratios = t.layout.default_ratios();
        }
    }

    fn remember_sizes(&mut self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        if t.layout == Layout::Single {
            return;
        }
        let (k1, k2) = Self::size_keys(t);
        self.sizes.insert(k1, t.ratios.clone());
        self.sizes.insert(k2, t.ratios.clone());
        config::save_json(&self.paths.sizes(), &self.sizes);
    }

    /* ------------------------------------------------------------ webview */

    fn app_def(&self, id: &str) -> Option<&AppDef> {
        self.config.apps.iter().find(|a| a.id == id)
    }

    /// "Effective" definition for a pane: the configured app, or an anonymous app for free URLs.
    fn effective_app(&self, content: &PaneContent) -> Option<AppDef> {
        match content {
            PaneContent::App { app, .. } => self.app_def(app).cloned(),
            PaneContent::Url { url, private } => Some(AppDef {
                id: String::new(),
                name: config::origin_of(url).unwrap_or_else(|| url.clone()),
                url: url.clone(),
                external_links_in_browser: false,
                incognito: *private,
                ..Default::default()
            }),
            _ => None,
        }
    }

    /// Browser profile for a pane: (profile key, private mode, tracker blocking).
    fn profile_for(&self, content: &PaneContent) -> (String, bool, bool) {
        let can_block = self.adblock.is_some();
        let default_block = self.config.privacy.block_trackers;
        let with_block = |base: String, block: bool| if block { format!("{base}-b") } else { base };
        match content {
            PaneContent::App { app, .. } => {
                let a = self.app_def(app);
                let block = can_block && a.and_then(|a| a.block_trackers).unwrap_or(default_block);
                let base = match a {
                    Some(a) if a.is_isolated() => format!("app-{}", profile_dir_name(&a.id)),
                    _ => "shared".into(),
                };
                (with_block(base, block), a.is_some_and(|a| a.incognito), block)
            }
            PaneContent::Url { private, .. } => {
                let block = can_block && default_block;
                (with_block("web".into(), block), *private, block)
            }
            _ => (INTERNAL_PROFILE.into(), false, false),
        }
    }

    fn create_view(&mut self, ti: usize, pi: usize) {
        let pane_id = self.tabs[ti].panes[pi].id;
        let content = self.tabs[ti].panes[pi].content.clone();
        let devtools = self.config.devtools && !(self.config.kiosk.enabled && !self.kiosk_unlocked);
        let floating = self.tabs[ti].is_floating();
        let proxy = self.proxy.clone();
        let ipc_proxy = self.proxy.clone();
        let ipc = move |req: wry::http::Request<String>| {
            let url = req.uri().to_string();
            let _ = ipc_proxy.send_event(UserEvent::PaneIpc { pane: pane_id, url, body: req.into_body() });
        };
        // Everything that needs `&self` is computed before borrowing the browser profile.
        let (profile, incognito, block) = self.profile_for(&content);
        let block_port = if block { self.adblock.as_ref().map(|h| h.port) } else { None };
        let page: Option<&str> = match &content {
            PaneContent::Empty => return,
            PaneContent::Terminal { .. } => Some("terminal.html"),
            PaneContent::Notes { .. } => Some("notes.html"),
            _ => None,
        };
        let web = if page.is_none() {
            let Some(app) = self.effective_app(&content) else {
                self.tabs[ti].panes[pi].doc_title = "app not found in the configuration".into();
                return;
            };
            let start_url = match &content {
                PaneContent::App { url: Some(u), .. } => u.clone(),
                _ => app.url.clone(),
            };
            let cfg = json!({"paneId": pane_id, "appId": app.id, "version": env!("CARGO_PKG_VERSION")});
            let mut script = assets::BRIDGE_JS.replace("__BRUST_CFG__", &cfg.to_string());
            if let Some(path) = &app.inject_script {
                match std::fs::read_to_string(path) {
                    Ok(extra) => {
                        script.push_str("\n;");
                        script.push_str(&extra);
                    }
                    Err(e) => log::warn!("inject_script {path}: {e}"),
                }
            }
            Some((app, start_url, script))
        } else {
            None
        };

        let paths = &self.paths;
        let ctx = self.contexts.entry(profile.clone()).or_insert_with(|| WebContext::new(Some(paths.profile(&profile))));
        let mut b = WebViewBuilder::new_with_web_context(ctx)
            .with_devtools(devtools)
            .with_visible(floating)
            .with_ipc_handler(ipc);
        if let Some(port) = block_port {
            b = b.with_proxy_config(ProxyConfig::Http(ProxyEndpoint { host: "127.0.0.1".into(), port: port.to_string() }));
        }
        let mut zoom = None;
        if let Some(page) = page {
            // On Linux the protocol is registered only once in the shared internal profile
            // (by the UI); on Windows/macOS it must be registered for each webview.
            if !cfg!(target_os = "linux") {
                b = b.with_custom_protocol(assets::SCHEME.into(), assets::handle);
            }
            b = b.with_url(assets::url(page)).with_background_color((15, 17, 21, 255));
        } else if let Some((app, start_url, script)) = web {
            let nav_app = app.clone();
            let p_title = proxy.clone();
            let p_load = proxy.clone();
            let p_new = proxy.clone();
            let perms = app.web_permissions.clone();
            b = b
                .with_url(start_url)
                .with_initialization_script_for_main_only(script, true)
                .with_incognito(incognito || app.incognito)
                .with_back_forward_navigation_gestures(false)
                .with_permission_handler(move |kind| permission_policy(&perms, &kind))
                .with_navigation_handler(move |url| {
                    let internal = ["about:", "data:", "blob:", "javascript:"].iter().any(|s| url.starts_with(s));
                    if internal || !nav_app.external_links_in_browser || nav_app.owns_url(&url) {
                        return true;
                    }
                    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
                        let _ = open::that_detached(&url);
                    }
                    false
                })
                .with_new_window_req_handler(move |url, _features| {
                    let _ = p_new.send_event(UserEvent::NewWindow { pane: pane_id, url });
                    NewWindowResponse::Deny
                })
                .with_document_title_changed_handler(move |title| {
                    let _ = p_title.send_event(UserEvent::Title { pane: pane_id, title });
                })
                .with_on_page_load_handler(move |ev, url| {
                    if let PageLoadEvent::Finished = ev {
                        let _ = p_load.send_event(UserEvent::Loaded { pane: pane_id, url });
                    }
                });
            if let Some(ua) = &app.user_agent {
                b = b.with_user_agent(ua);
            }
            zoom = app.zoom;
        }
        let result = match self.tabs[ti].window.as_ref() {
            Some(w) => build_floating(b, w),
            None => self.host.build(b),
        };
        match result {
            Ok(v) => {
                if let Some(z) = zoom {
                    let _ = v.zoom(z);
                }
                self.tabs[ti].panes[pi].view = Some(v);
            }
            Err(e) => {
                log::error!("webview creation: {e}");
                self.tabs[ti].panes[pi].doc_title = format!("error: {e}");
            }
        }
    }

    fn drop_view(&mut self, ti: usize, pi: usize) {
        let p = &mut self.tabs[ti].panes[pi];
        p.view = None;
        p.pty = None;
        p.pty_exited = false;
        p.doc_title.clear();
        p.custom_title = None;
        let id = p.id;
        self.locals.release_pane(id);
        self.calls.retain(|_, c| !matches!(c, Caller::Pane { pane, .. } if *pane == id));
    }

    /* ------------------------------------------------------------ layout */

    fn stage(&self) -> R {
        let (w, h) = self.host.size();
        R::new(PAD, PAD, w - 2.0 * PAD, h - BAR_H - 2.0 * PAD)
    }

    fn pane_rects(&self, t: &Tab) -> (Vec<R>, Vec<layout::Splitter>) {
        t.layout.compute(self.stage(), &t.ratios, GAP)
    }

    /// Positions and shows/hides the webviews of the active tab.
    fn relayout(&mut self) {
        let (w, h) = self.host.size();
        if let Some(s) = &self.shell {
            self.host.place(s, R::new(0.0, 0.0, w, h));
        }
        // Detached windows: hidden while Brust is locked.
        for t in self.tabs.iter().filter(|t| t.is_floating()) {
            if let (Some(w), Some(v)) = (&t.window, t.panes.first().and_then(|p| p.view.as_ref())) {
                let _ = v.set_visible(!self.locked);
                let size = w.inner_size().to_logical::<f64>(w.scale_factor());
                let _ = v.set_bounds(rect(R::new(0.0, 0.0, size.width, size.height)));
            }
        }
        let Some(t) = self.tabs.get(self.active) else { return };
        let (rects, _) = self.pane_rects(t);
        for p in t.panes.iter().skip(rects.len()) {
            if let Some(v) = &p.view {
                let _ = v.set_visible(false);
            }
        }
        for (p, r) in t.panes.iter().zip(rects) {
            if let Some(v) = &p.view {
                let vr = R::new(r.x + 1.0, r.y + BAR_H + HEADER_H, r.w - 2.0, r.h - HEADER_H - 1.0);
                let visible = !self.overlay && !self.locked && self.confirms.is_empty() && !p.picking && vr.w > 4.0 && vr.h > 4.0;
                // Show first, then position: when GTK re-shows a widget it recomputes
                // its allocation and would undo the size just set.
                let _ = v.set_visible(visible);
                self.host.place(v, vr);
            }
        }
    }

    fn activate(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if let Some(w) = &self.tabs[index].window {
            w.set_focus();
            return;
        }
        if index != self.active
            && let Some(old) = self.tabs.get(self.active) {
                for p in &old.panes {
                    if let Some(v) = &p.view {
                        let _ = v.set_visible(false);
                    }
                }
            }
        self.active = index;
        self.ensure_views();
        self.relayout();
        self.focus_current();
        self.push_state();
        self.save_session();
    }

    fn ensure_views(&mut self) {
        let ti = self.active;
        for pi in 0..self.tabs[ti].layout.pane_count().min(self.tabs[ti].panes.len()) {
            let p = &self.tabs[ti].panes[pi];
            if p.view.is_none() && p.content != PaneContent::Empty {
                self.create_view(ti, pi);
            }
        }
    }

    fn focus_current(&self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        if let Some(p) = t.panes.get(t.focused)
            && let Some(v) = &p.view
                && !p.picking && !self.overlay && !self.locked {
                    let _ = v.focus();
                    match p.content {
                        PaneContent::Terminal { .. } => {
                            let _ = v.evaluate_script("window.__term_focus && __term_focus()");
                        }
                        PaneContent::Notes { .. } => {
                            let _ = v.evaluate_script("window.__notes_focus && __notes_focus()");
                        }
                        _ => {}
                    }
                }
    }

    fn push_tab(&mut self, content: PaneContent) {
        let pid = self.id();
        let id = self.id();
        self.tabs.push(Tab::new(id, None, Layout::Single, vec![], vec![Pane::new(pid, content)]));
    }

    fn open_tab(&mut self, content: PaneContent) {
        self.push_tab(content);
        self.activate(self.tabs.len() - 1);
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        for pi in 0..self.tabs[index].panes.len() {
            self.drop_view(index, pi);
        }
        let was_floating = self.tabs[index].is_floating();
        self.tabs.remove(index);
        if was_floating {
            if self.active > index {
                self.active -= 1;
            }
            self.push_state();
            self.save_session();
            return;
        }
        if !self.tabs.iter().any(|t| !t.is_floating()) {
            self.push_tab(PaneContent::Empty);
        }
        let mut next = if self.active > index || self.active >= self.tabs.len() { self.active.saturating_sub(1) } else { self.active };
        next = next.min(self.tabs.len() - 1);
        if self.tabs[next].is_floating() {
            next = self.tabs.iter().position(|t| !t.is_floating()).unwrap_or(0);
        }
        self.active = usize::MAX; // force activation
        self.activate(next);
    }

    fn set_layout(&mut self, layout: Layout) {
        let ti = self.active;
        let n = layout.pane_count();
        if self.tabs[ti].panes.len() > n {
            // When shrinking the layout, panes with content stay visible first;
            // the others are parked (not closed). Extra empty panes are removed.
            let focused_id = self.tabs[ti].panes.get(self.tabs[ti].focused).map(|p| p.id);
            let panes = std::mem::take(&mut self.tabs[ti].panes);
            let (mut full, empty): (Vec<Pane>, Vec<Pane>) = panes.into_iter().partition(|p| p.content != PaneContent::Empty);
            let missing = n.saturating_sub(full.len());
            full.extend(empty.into_iter().take(missing));
            self.tabs[ti].panes = full;
            self.tabs[ti].focused = focused_id.and_then(|id| self.tabs[ti].panes.iter().position(|p| p.id == id)).unwrap_or(0);
        }
        while self.tabs[ti].panes.len() < n {
            let id = self.id();
            self.tabs[ti].panes.push(Pane::new(id, PaneContent::Empty));
        }
        let t = &mut self.tabs[ti];
        t.layout = layout;
        t.focused = t.focused.min(n - 1);
        self.recall_sizes(ti, true);
        self.ensure_views();
        self.relayout();
        self.push_state();
        self.save_session();
    }

    fn find_pane(&self, id: u32) -> Option<(usize, usize)> {
        self.tabs.iter().enumerate().find_map(|(ti, t)| t.panes.iter().position(|p| p.id == id).map(|pi| (ti, pi)))
    }

    fn set_content(&mut self, pane: u32, content: PaneContent) {
        let Some((ti, pi)) = self.find_pane(pane) else { return };
        self.drop_view(ti, pi);
        self.tabs[ti].panes[pi].content = content;
        self.tabs[ti].panes[pi].picking = false;
        self.tabs[ti].focused = pi;
        self.recall_sizes(ti, false);
        if ti == self.active {
            self.ensure_views();
            self.relayout();
            self.focus_current();
        }
        self.push_state();
        self.save_session();
    }

    fn reload_tab(&mut self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        for p in &t.panes {
            if let (Some(v), false) = (&p.view, matches!(p.content, PaneContent::Terminal { .. })) {
                let _ = v.reload();
            }
        }
    }

    /* ---------------------------------------------------------- UI state */

    fn content_label(&self, c: &PaneContent) -> (String, String, Option<String>) {
        match c {
            PaneContent::Empty => ("empty".into(), "Choose content".into(), None),
            PaneContent::App { app, .. } => match self.app_def(app) {
                Some(a) => ("app".into(), a.name.clone(), a.icon.clone()),
                None => ("app".into(), app.clone(), None),
            },
            PaneContent::Terminal { shell } => (
                "terminal".into(),
                self.config.shells.iter().find(|s| &s.id == shell).map(|s| s.name.clone()).unwrap_or_else(|| shell.clone()),
                None,
            ),
            PaneContent::Url { url, private } => {
                let host = config::origin_of(url).map(|o| o.split("://").nth(1).unwrap_or("").to_string()).unwrap_or_else(|| url.clone());
                ("url".into(), host, private.then(|| "🕶".to_string()))
            }
            PaneContent::Notes { notebook } => ("notes".into(), format!("Notes · {notebook}"), Some("📝".into())),
        }
    }

    fn tab_title(&self, t: &Tab) -> (String, Option<String>) {
        if let Some(title) = t.title.as_ref().filter(|s| !s.is_empty()) {
            let icon = t.visible().iter().find_map(|p| self.content_label(&p.content).2);
            return (title.clone(), icon);
        }
        let mut names = Vec::new();
        let mut icon = None;
        let shown = t.visible().len();
        for p in t.visible() {
            if p.content == PaneContent::Empty {
                continue;
            }
            let (kind, name, ic) = self.content_label(&p.content);
            if icon.is_none() {
                icon = ic.or_else(|| Some(if kind == "terminal" { "›_".into() } else { "🌐".into() }));
            }
            let name = match (&p.content, p.doc_title.is_empty(), shown) {
                (PaneContent::Url { .. }, false, 1) => p.doc_title.clone(),
                _ => name,
            };
            names.push(p.custom_title.clone().unwrap_or(name));
        }
        if names.is_empty() { ("New tab".into(), None) } else { (names.join(" · "), icon) }
    }

    fn kiosk(&self) -> bool {
        self.config.kiosk.enabled && !self.kiosk_unlocked
    }

    fn pin_set(&self) -> bool {
        secrets::load_secret(&self.paths.pin_secret()).is_some()
    }

    fn push_state(&self) {
        let Some(shell) = &self.shell else { return };
        let Some(t) = self.tabs.get(self.active) else { return };
        let (rects, splitters) = self.pane_rects(t);
        let panes: Vec<Value> = t
            .panes
            .iter()
            .zip(rects)
            .enumerate()
            .map(|(i, (p, r))| {
                let (kind, name, icon) = self.content_label(&p.content);
                let title = p.custom_title.clone().unwrap_or(name);
                let subtitle = if p.pty_exited { "exited".to_string() } else { p.doc_title.clone() };
                json!({
                    "id": p.id, "index": i, "kind": kind, "title": title, "subtitle": subtitle,
                    "icon": icon, "rect": r, "focused": i == t.focused, "picking": p.picking, "badge": p.badge,
                })
            })
            .collect();
        let tabs: Vec<Value> = self
            .tabs
            .iter()
            .map(|t| {
                let (title, icon) = self.tab_title(t);
                json!({
                    "id": t.id, "title": title, "customTitle": t.title, "icon": icon, "layout": t.layout.id(),
                    "count": t.visible().len(), "floating": t.is_floating(),
                    "badge": t.panes.iter().map(|p| p.badge).sum::<u32>(),
                })
            })
            .collect();
        let confirm = self.confirms.front().map(|c| {
            json!({"seq": c.seq, "app": if c.app.name.is_empty() { c.app.url.clone() } else { c.app.name.clone() },
                   "cmd": c.cmd, "detail": c.detail, "queued": self.confirms.len()})
        });
        let state = json!({
            "version": env!("CARGO_PKG_VERSION"),
            "barHeight": BAR_H, "headerHeight": HEADER_H,
            "tabs": tabs, "active": self.active, "tabId": t.id,
            "layout": t.layout.id(),
            "layouts": layout::ALL.iter().map(|l| l.id()).collect::<Vec<_>>(),
            "panes": panes, "splitters": splitters,
            "apps": self.config.apps.iter().map(|a| json!({"id": a.id, "name": a.name, "icon": a.icon, "url": a.url, "permissions": a.permissions,
                "isolated": a.is_isolated(), "incognito": a.incognito, "clearOnExit": a.clear_on_exit})).collect::<Vec<_>>(),
            "shells": self.config.shells.iter().map(|s| json!({"id": s.id, "name": s.name, "program": s.program})).collect::<Vec<_>>(),
            "appDefs": self.config.apps,
            "theme": self.config.theme,
            "effectiveTheme": self.effective_theme(),
            "privacyConfig": self.config.privacy,
            "privacyManaged": self.remote.as_ref().is_some_and(|r| r.get("privacy").is_some()),
            "appsManaged": self.apps_managed(),
            "permissionNames": config::PERMISSIONS,
            "devtools": self.config.devtools && !self.kiosk(),
            "bridge": {
                "enabled": self.config.bridge.enabled, "port": self.config.bridge.port, "token": self.config.bridge.token,
                "status": self.bridge_status, "clients": self.clients.values().map(|c| c.name.clone()).collect::<Vec<_>>(),
            },
            "privacy": {
                "blockTrackers": self.adblock.is_some(),
                "blocked": self.adblock.as_ref().map(|h| h.blocked_count()).unwrap_or(0),
                "confirmSensitive": self.config.privacy.confirm_sensitive,
                "auditLog": self.config.privacy.audit_log,
                "auditPath": self.audit.path().to_string_lossy(),
                "grants": self.grants.list(),
                "lockAfter": self.config.privacy.lock_after_minutes,
            },
            "pinSet": self.pin_set(),
            "locked": self.locked,
            "kiosk": self.kiosk(),
            "kioskConfigured": self.config.kiosk.enabled,
            "confirm": confirm,
            "templates": self.templates.iter().map(|t| &t.name).collect::<Vec<_>>(),
            "notesDir": self.notes.dir().to_string_lossy(),
            "remote": {"configured": self.config.remote_config.is_some(), "status": self.remote_status},
            "update": {
                "configured": self.config.update.as_ref().is_some_and(|u| !u.manifest_url.is_empty()),
                "status": self.update_status,
                "available": self.update_info.as_ref().map(|u| json!({"version": u.version, "notes": u.notes})),
            },
            "configPath": self.paths.config().to_string_lossy(),
            "configError": self.config_error,
        });
        let _ = shell.evaluate_script(&format!("window.brustShell && brustShell.update({state})"));
        let badge: u32 = self.tabs.iter().flat_map(|t| t.panes.iter()).map(|p| p.badge).sum();
        let title = self.tab_title(t).0;
        self.host.window.set_title(&if badge > 0 { format!("({badge}) {title} – Brust") } else { format!("{title} – Brust") });
    }

    /// Gives keyboard focus to the UI (palette, lock screen, confirmations).
    fn focus_shell(&self) {
        if let Some(s) = &self.shell {
            let _ = s.focus();
        }
    }

    /// Calls `brustShell.<name>(arg)` in the UI.
    fn shell_call(&self, name: &str, arg: &Value) {
        if let Some(s) = &self.shell {
            let _ = s.evaluate_script(&format!("window.brustShell && brustShell.{name} && brustShell.{name}({arg})"));
        }
    }

    fn toast(&self, msg: &str) {
        if let Some(s) = &self.shell {
            let _ = s.evaluate_script(&format!("window.brustShell && brustShell.toast({})", json!(msg)));
        }
    }

    /* ---------------------------------------------------------- IPC shell */

    fn on_shell(&mut self, body: &str) {
        let Ok(m) = serde_json::from_str::<Value>(body) else { return };
        let cmd = m["cmd"].as_str().unwrap_or("");
        let idx = |k: &str| m[k].as_u64().map(|v| v as usize);
        let pane = m["pane"].as_u64().map(|v| v as u32);
        self.last_activity = Instant::now();
        if self.locked && !matches!(cmd, "ready" | "unlock") {
            return;
        }
        // In kiosk mode the tab structure and the settings are frozen.
        if self.kiosk()
            && matches!(
                cmd,
                "new_tab" | "close_tab" | "move_tab" | "rename_tab" | "set_layout" | "close_pane" | "swap_panes" | "pick"
                    | "set_content" | "detach" | "open_config" | "open_config_dir" | "reload_config" | "save_template"
                    | "delete_template" | "devtools" | "set_pin" | "clear_pin" | "revoke_grant" | "install_update"
                    | "app_save" | "app_delete" | "privacy_save" | "set_theme"
            )
        {
            return;
        }
        match cmd {
            "ready" => {
                self.push_state();
                if let Some(e) = &self.config_error {
                    self.toast(e);
                }
            }
            "new_tab" => self.open_tab(PaneContent::Empty),
            "close_tab" => {
                if let Some(i) = idx("index") {
                    self.close_tab(i)
                }
            }
            "select_tab" => {
                if let Some(i) = idx("index") {
                    self.activate(i)
                }
            }
            "move_tab" => {
                if let (Some(from), Some(to)) = (idx("from"), idx("to"))
                    && from < self.tabs.len() && to < self.tabs.len() {
                        let active_id = self.tabs[self.active].id;
                        let t = self.tabs.remove(from);
                        self.tabs.insert(to, t);
                        self.active = self.tabs.iter().position(|t| t.id == active_id).unwrap_or(0);
                        self.push_state();
                        self.save_session();
                    }
            }
            "rename_tab" => {
                if let Some(t) = idx("index").and_then(|i| self.tabs.get_mut(i)) {
                    let title = m["title"].as_str().unwrap_or("").trim().to_string();
                    t.title = (!title.is_empty()).then_some(title);
                    self.push_state();
                    self.save_session();
                }
            }
            "set_layout" => {
                if let Some(l) = m["layout"].as_str().and_then(Layout::from_id) {
                    self.set_layout(l)
                }
            }
            "ratio" => {
                if let (Some(i), Some(v)) = (idx("index"), m["value"].as_f64()) {
                    let t = &mut self.tabs[self.active];
                    let mut r = t.ratios.clone();
                    if i < r.len() {
                        r[i] = v;
                        t.ratios = t.layout.sanitize(&r);
                        self.relayout();
                        self.push_state();
                    }
                }
            }
            "ratio_reset" => {
                if let Some(i) = idx("index") {
                    let t = &mut self.tabs[self.active];
                    let def = t.layout.default_ratios();
                    if i < t.ratios.len() {
                        t.ratios[i] = def[i];
                        t.ratios = t.layout.sanitize(&t.ratios);
                    }
                    self.remember_sizes();
                    self.relayout();
                    self.push_state();
                    self.save_session();
                }
            }
            "ratio_end" => {
                self.remember_sizes();
                self.save_session();
            }
            "set_content" => {
                if let (Some(p), Ok(c)) = (pane, serde_json::from_value::<PaneContent>(m["content"].clone())) {
                    self.set_content(p, c);
                }
            }
            "pick" => {
                if let Some((ti, pi)) = pane.and_then(|p| self.find_pane(p)) {
                    let on = m["on"].as_bool().unwrap_or(true);
                    self.tabs[ti].panes[pi].picking = on && self.tabs[ti].panes[pi].content != PaneContent::Empty;
                    self.tabs[ti].focused = pi;
                    self.relayout();
                    self.push_state();
                    if !on {
                        self.focus_current();
                    }
                }
            }
            "close_pane" => {
                if let Some(p) = pane {
                    self.close_pane(p);
                }
            }
            "swap_panes" => {
                let (a, b) = (m["a"].as_u64().map(|v| v as u32), m["b"].as_u64().map(|v| v as u32));
                if let (Some((ta, pa)), Some((tb, pb))) = (a.and_then(|a| self.find_pane(a)), b.and_then(|b| self.find_pane(b)))
                    && ta == tb {
                        self.tabs[ta].panes.swap(pa, pb);
                        self.tabs[ta].focused = pb;
                        self.recall_sizes(ta, false);
                        self.relayout();
                        self.push_state();
                        self.save_session();
                    }
            }
            "reload_tab" => self.reload_tab(),
            "reload_pane" => {
                if let Some((ti, pi)) = pane.and_then(|p| self.find_pane(p)) {
                    let p = &self.tabs[ti].panes[pi];
                    if matches!(p.content, PaneContent::Terminal { .. }) {
                        // Restart the terminal.
                        self.tabs[ti].panes[pi].pty = None;
                        self.tabs[ti].panes[pi].pty_exited = false;
                        if let Some(v) = &self.tabs[ti].panes[pi].view {
                            let _ = v.reload();
                        }
                    } else if let Some(v) = &p.view {
                        let _ = v.reload();
                    } else {
                        self.ensure_views();
                        self.relayout();
                    }
                }
            }
            "focus_pane" => {
                if let Some((ti, pi)) = pane.and_then(|p| self.find_pane(p)) {
                    self.set_focus(ti, pi, true);
                }
            }
            "devtools" => {
                if let Some(v) = pane.and_then(|p| self.find_pane(p)).and_then(|(ti, pi)| self.tabs[ti].panes[pi].view.as_ref()) {
                    v.open_devtools();
                }
            }
            "overlay" => {
                self.overlay = m["on"].as_bool().unwrap_or(false);
                self.relayout();
                if !self.overlay {
                    self.focus_current();
                }
            }
            "shortcut" => self.shortcut(&m),
            "open_config" => {
                let _ = open::that_detached(self.paths.config());
            }
            "open_config_dir" => {
                let _ = open::that_detached(&self.paths.config_dir);
            }
            "reload_config" => self.reload_config(),
            "app_save" => {
                let original = m["original"].as_str().map(str::to_string);
                match serde_json::from_value::<AppDef>(m["app"].clone()) {
                    Ok(app) => self.save_app(app, original.as_deref()),
                    Err(e) => self.shell_call("appError", &json!(format!("Invalid data: {e}"))),
                }
            }
            "set_theme" => {
                let theme = m["theme"].as_str().unwrap_or("system");
                if matches!(theme, "system" | "dark" | "light") {
                    match config::save_config_key(&self.paths, "theme", json!(theme)) {
                        Ok(()) => {
                            self.reload_config();
                            self.apply_theme();
                        }
                        Err(e) => self.toast(&e),
                    }
                }
            }
            "privacy_save" => match serde_json::from_value::<config::PrivacyConfig>(m["privacy"].clone()) {
                Ok(p) => self.save_privacy(p),
                Err(e) => self.shell_call("privacyError", &json!(format!("Invalid data: {e}"))),
            },
            "app_delete" => {
                if let Some(id) = m["id"].as_str().map(str::to_string) {
                    self.delete_app(&id);
                }
            }
            "confirm_reply" => {
                let seq = m["seq"].as_u64().unwrap_or(0);
                let decision = m["decision"].as_str().unwrap_or("deny").to_string();
                self.confirm_reply(seq, &decision);
            }
            "lock" => self.lock(),
            "unlock" => {
                let pin = m["pin"].as_str().unwrap_or("");
                let stored = secrets::load_secret(&self.paths.pin_secret());
                let ok = match &stored {
                    Some(h) => secrets::verify_pin(pin, h),
                    None => true,
                };
                if ok {
                    let kiosk_exit = m["kiosk"].as_bool().unwrap_or(false);
                    if kiosk_exit {
                        self.kiosk_unlocked = true;
                        self.host.window.set_fullscreen(None);
                    }
                    self.locked = false;
                    self.overlay = false;
                    self.relayout();
                    self.focus_current();
                    self.push_state();
                } else {
                    self.audit.record("brust", "unlock", "", "denied", None);
                    self.shell_call("pinError", &json!("Wrong PIN"));
                }
            }
            "set_pin" => {
                let old = m["old"].as_str().unwrap_or("");
                let new = m["pin"].as_str().unwrap_or("");
                match secrets::load_secret(&self.paths.pin_secret()) {
                    Some(h) if !secrets::verify_pin(old, &h) => self.shell_call("pinError", &json!("The current PIN is wrong")),
                    _ if new.chars().count() < 4 => self.shell_call("pinError", &json!("The PIN must have at least 4 characters")),
                    _ => match secrets::save_secret(&self.paths.pin_secret(), &secrets::hash_pin(new)) {
                        Ok(()) => {
                            self.toast("PIN saved");
                            self.push_state();
                        }
                        Err(e) => self.toast(&format!("Cannot save the PIN: {e}")),
                    },
                }
            }
            "clear_pin" => {
                let old = m["old"].as_str().unwrap_or("");
                match secrets::load_secret(&self.paths.pin_secret()) {
                    Some(h) if secrets::verify_pin(old, &h) => {
                        let _ = std::fs::remove_file(self.paths.pin_secret());
                        self.toast("PIN removed");
                        self.push_state();
                    }
                    Some(_) => self.shell_call("pinError", &json!("The current PIN is wrong")),
                    None => {}
                }
            }
            "revoke_grant" => {
                match m["key"].as_str() {
                    Some("*") => self.grants.clear(),
                    Some(k) => self.grants.remove(k),
                    None => {}
                }
                self.push_state();
            }
            "audit_tail" => {
                let entries = self.audit.tail(300);
                self.shell_call("auditLog", &Value::Array(entries));
            }
            "open_audit" => {
                let _ = open::that_detached(self.audit.path());
            }
            "save_template" => {
                let name = m["name"].as_str().unwrap_or("").trim().to_string();
                if !name.is_empty() {
                    let tab = Self::saved_tab(&self.tabs[self.active]);
                    self.templates.retain(|t| t.name != name);
                    self.templates.push(Template { name: name.clone(), tab });
                    self.templates.sort_by_key(|a| a.name.to_lowercase());
                    config::save_json(&self.paths.templates(), &self.templates);
                    self.toast(&format!("Template \"{name}\" saved"));
                    self.push_state();
                }
            }
            "open_template" => {
                let name = m["name"].as_str().unwrap_or("");
                self.open_template(name);
            }
            "delete_template" => {
                let name = m["name"].as_str().unwrap_or("");
                self.templates.retain(|t| t.name != name);
                config::save_json(&self.paths.templates(), &self.templates);
                self.push_state();
            }
            "open" => {
                // From the command palette: open content in a new tab or in the focused pane.
                if let Ok(c) = serde_json::from_value::<PaneContent>(m["content"].clone()) {
                    if m["here"].as_bool().unwrap_or(false) {
                        let t = &self.tabs[self.active];
                        if let Some(p) = t.panes.get(t.focused).map(|p| p.id) {
                            self.set_content(p, c);
                        }
                    } else {
                        self.open_tab(c);
                    }
                }
            }
            "detach" => {
                let target = pane.or_else(|| {
                    let t = &self.tabs[self.active];
                    t.panes.get(t.focused).map(|p| p.id)
                });
                if let Some(p) = target {
                    self.pending_detach.push(p);
                }
            }
            "reattach" => {
                if let Some(i) = idx("index") {
                    self.reattach(i);
                }
            }
            "open_notes_dir" => {
                let _ = open::that_detached(self.notes.dir());
            }
            "check_update" => {
                self.check_update();
                self.push_state();
            }
            "install_update" => {
                if let Some(info) = self.update_info.clone() {
                    self.update_status = format!("downloading {}…", info.version);
                    self.push_state();
                    let p = self.proxy.clone();
                    std::thread::spawn(move || {
                        let _ = p.send_event(UserEvent::UpdateDownloaded(update::download(&info)));
                    });
                }
            }
            "refresh_remote" => {
                self.remote_status = "loading…".into();
                self.fetch_remote_config();
                self.push_state();
            }
            _ => log::debug!("unknown shell command: {cmd}"),
        }
    }

    fn close_pane(&mut self, pane: u32) {
        let Some((ti, pi)) = self.find_pane(pane) else { return };
        if self.tabs[ti].panes[pi].content == PaneContent::Empty {
            // Pane already empty: shrink the tileset.
            let n = self.tabs[ti].panes.len();
            let smaller = match self.tabs[ti].layout.pane_count() {
                4 => Layout::MainLeft,
                3 => Layout::SplitH,
                _ => Layout::Single,
            };
            let shown = self.tabs[ti].layout.pane_count();
            if shown > 1 && ti == self.active {
                self.tabs[ti].panes.remove(pi);
                let smaller = if n > shown { self.tabs[ti].layout } else { smaller };
                self.set_layout(smaller);
            }
            return;
        }
        self.drop_view(ti, pi);
        self.tabs[ti].panes[pi].content = PaneContent::Empty;
        self.tabs[ti].panes[pi].picking = false;
        self.relayout();
        self.push_state();
        self.save_session();
    }

    fn lock(&mut self) {
        if !self.pin_set() {
            self.toast("Set a PIN in Settings → Privacy first");
            return;
        }
        self.locked = true;
        self.overlay = false;
        self.relayout();
        self.push_state();
        self.focus_shell();
    }

    /// Opens a template in a new tab.
    fn open_template(&mut self, name: &str) {
        match self.templates.iter().find(|t| t.name == name).cloned() {
            Some(t) => {
                self.push_saved_tab(t.tab);
                if self.tabs.last().is_some_and(|t| t.title.is_none())
                    && let Some(t) = self.tabs.last_mut() {
                        t.title = Some(name.to_string());
                    }
                self.activate(self.tabs.len() - 1);
            }
            None => self.toast(&format!("Template \"{name}\" not found")),
        }
    }

    /// Moves a pane into its own window (needs the event loop target to create the window).
    fn detach(&mut self, pane: u32, target: &EventLoopWindowTarget<UserEvent>) {
        let Some((ti, pi)) = self.find_pane(pane) else { return };
        if self.tabs[ti].is_floating() || self.tabs[ti].panes[pi].content == PaneContent::Empty {
            return;
        }
        let (_, name, _) = self.content_label(&self.tabs[ti].panes[pi].content);
        let window = match WindowBuilder::new()
            .with_title(format!("{name} – Brust"))
            .with_window_icon(window_icon())
            .with_inner_size(LogicalSize::new(1100.0, 750.0))
            .build(target)
        {
            Ok(w) => w,
            Err(e) => return self.toast(&format!("Cannot open a window: {e}")),
        };
        // The pane (with its terminal session, if any) moves; its webview is recreated in the new window.
        let new_id = self.id();
        let mut moved = std::mem::replace(&mut self.tabs[ti].panes[pi], Pane::new(new_id, PaneContent::Empty));
        moved.view = None;
        moved.picking = false;
        let id = self.id();
        let mut tab = Tab::new(id, None, Layout::Single, vec![], vec![moved]);
        tab.window = Some(window);
        self.tabs.push(tab);
        let fi = self.tabs.len() - 1;
        self.create_view(fi, 0);
        self.relayout();
        self.push_state();
        self.save_session();
    }

    /// Brings a detached window back as a tab.
    fn reattach(&mut self, index: usize) {
        if !self.tabs.get(index).is_some_and(|t| t.is_floating()) {
            return;
        }
        let mut tab = self.tabs.remove(index);
        if self.active > index {
            self.active -= 1;
        }
        for p in &mut tab.panes {
            p.view = None;
        }
        tab.window = None;
        self.tabs.push(tab);
        self.activate(self.tabs.len() - 1);
    }

    /// Queues a sensitive command for the user's decision.
    fn ask_confirm(&mut self, pane: u32, id: u64, app: AppDef, cmd: &str, args: Value) {
        let seq = self.next_confirm;
        self.next_confirm += 1;
        let detail = audit::summarize(cmd, &args);
        self.confirms.push_back(Confirm { seq, pane, id, app, cmd: cmd.to_string(), args, detail });
        self.relayout();
        self.push_state();
        self.focus_shell();
        if !self.host.window.is_focused() {
            self.host.window.request_user_attention(Some(tao::window::UserAttentionType::Critical));
        }
    }

    fn confirm_reply(&mut self, seq: u64, decision: &str) {
        let Some(pos) = self.confirms.iter().position(|c| c.seq == seq) else { return };
        let Some(c) = self.confirms.remove(pos) else { return };
        let app_label = if c.app.id.is_empty() { c.app.url.clone() } else { c.app.id.clone() };
        match decision {
            "once" | "always" => {
                if decision == "always" {
                    self.grants.insert(Grants::key(&app_label, &c.cmd, &c.args));
                }
                let label = if decision == "always" { "always" } else { "confirmed" };
                self.run_local(c.pane, c.id, c.app, &c.cmd, c.args, label);
            }
            _ => {
                self.audit.record(&app_label, &c.cmd, &c.detail, "denied", None);
                self.send_to_pane(c.pane, &ok_reply(c.id, Err("denied by the user".into())));
            }
        }
        self.relayout();
        self.focus_current();
        self.push_state();
    }

    /// Runs a local capability on a worker thread; the result is audited and sent back to the pane.
    fn run_local(&self, pane: u32, id: u64, app: AppDef, cmd: &str, args: Value, decision: &str) {
        let locals = self.locals.clone();
        let proxy = self.proxy.clone();
        let cmd = cmd.to_string();
        let decision = decision.to_string();
        let app_label = if app.id.is_empty() { app.url.clone() } else { app.id.clone() };
        let detail = audit::summarize(&cmd, &args);
        std::thread::spawn(move || {
            let result = locals.execute(&app, pane, &cmd, &args);
            let _ = proxy.send_event(UserEvent::Audited { pane, id, app: app_label, cmd, detail, decision, result });
        });
    }

    fn set_focus(&mut self, ti: usize, pi: usize, give_focus: bool) {
        if ti >= self.tabs.len() || pi >= self.tabs[ti].visible().len() {
            return;
        }
        let changed = self.tabs[ti].focused != pi;
        self.tabs[ti].focused = pi;
        if give_focus && ti == self.active {
            self.focus_current();
        }
        if changed {
            self.push_state();
        }
    }

    /// Next/previous tab in the tab bar (detached windows are skipped).
    fn cycle_tab(&mut self, forward: bool) {
        let docked: Vec<usize> = (0..self.tabs.len()).filter(|&i| !self.tabs[i].is_floating()).collect();
        let Some(pos) = docked.iter().position(|&i| i == self.active) else { return };
        let n = docked.len();
        let next = if forward { docked[(pos + 1) % n] } else { docked[(pos + n - 1) % n] };
        self.activate(next);
    }

    fn shortcut(&mut self, m: &Value) {
        if self.locked {
            return;
        }
        match m["name"].as_str().unwrap_or("") {
            "reload_tab" => self.reload_tab(),
            "next_tab" => self.cycle_tab(true),
            "prev_tab" => self.cycle_tab(false),
            "new_tab" if !self.kiosk() => self.open_tab(PaneContent::Empty),
            "palette" => {
                self.focus_shell();
                self.shell_call("palette", &Value::Null);
            }
            "lock" => self.lock(),
            "kiosk_exit" if self.kiosk() => {
                self.focus_shell();
                self.shell_call("kioskExit", &Value::Null);
            }
            "focus_pane" => {
                let i = m["index"].as_u64().unwrap_or(0) as usize;
                self.set_focus(self.active, i, true);
            }
            _ => {}
        }
    }

    /// True when the central configuration defines the app list (editing is then disabled).
    fn apps_managed(&self) -> bool {
        self.remote.as_ref().is_some_and(|r| r.get("apps").is_some())
    }

    /// Adds or updates an app from Settings → Apps and saves it to config.json.
    fn save_app(&mut self, mut app: AppDef, original: Option<&str>) {
        if self.apps_managed() {
            return self.shell_call("appError", &json!("The apps are managed by the central configuration"));
        }
        let mut apps = self.config.apps.clone();
        if let Err(e) = config::validate_app(&mut app, &apps, original) {
            return self.shell_call("appError", &json!(e));
        }
        match original.and_then(|o| apps.iter().position(|a| a.id == o)) {
            Some(i) => apps[i] = app.clone(),
            None => apps.push(app.clone()),
        }
        if let Err(e) = config::save_apps(&self.paths, &apps) {
            return self.shell_call("appError", &json!(e));
        }
        self.reload_config();
        // An ID change also updates the panes and the saved session.
        if let Some(old) = original.filter(|o| *o != app.id) {
            for p in self.tabs.iter_mut().flat_map(|t| t.panes.iter_mut()) {
                if let PaneContent::App { app: a, .. } = &mut p.content {
                    if a == old {
                        *a = app.id.clone();
                    }
                }
            }
        }
        self.reopen_app_panes(&app.id);
        self.save_session();
        self.shell_call("appSaved", &json!(app.id));
        self.toast(&format!("\"{}\" saved", app.name));
    }

    /// Saves Settings → Privacy. Panes whose tracker blocking changes are reopened.
    fn save_privacy(&mut self, mut p: config::PrivacyConfig) {
        if self.remote.as_ref().is_some_and(|r| r.get("privacy").is_some()) {
            return self.shell_call("privacyError", &json!("Privacy settings are managed by the central configuration"));
        }
        if let Err(e) = config::validate_privacy(&mut p) {
            return self.shell_call("privacyError", &json!(e));
        }
        let before: Vec<(u32, String)> = self.web_pane_profiles();
        if let Err(e) = config::save_config_key(&self.paths, "privacy", serde_json::to_value(&p).unwrap_or_default()) {
            return self.shell_call("privacyError", &json!(e));
        }
        let lists_changed = p.blocklists != self.config.privacy.blocklists
            || p.block_domains != self.config.privacy.block_domains
            || p.allow_domains != self.config.privacy.allow_domains;
        self.reload_config();
        if lists_changed && self.adblock.is_some() {
            self.toast("Block list changes take effect on the next start of Brust");
        }
        // Reopen panes whose browser profile changed (tracker blocking switched on or off).
        let after: HashMap<u32, String> = self.web_pane_profiles().into_iter().collect();
        for (id, profile) in before {
            if after.get(&id).is_some_and(|p| *p != profile) {
                if let Some((ti, pi)) = self.find_pane(id) {
                    self.tabs[ti].panes[pi].view = None;
                    if self.tabs[ti].is_floating() {
                        self.create_view(ti, pi);
                    }
                }
            }
        }
        self.ensure_views();
        self.relayout();
        self.push_state();
        self.shell_call("privacySaved", &Value::Null);
    }

    /// Browser profile of every loaded web pane.
    fn web_pane_profiles(&self) -> Vec<(u32, String)> {
        self.tabs
            .iter()
            .flat_map(|t| t.panes.iter())
            .filter(|p| p.view.is_some() && matches!(p.content, PaneContent::App { .. } | PaneContent::Url { .. }))
            .map(|p| (p.id, self.profile_for(&p.content).0))
            .collect()
    }

    /// Theme actually used ("dark" or "light").
    fn effective_theme(&self) -> &'static str {
        match self.config.theme.as_str() {
            "light" => "light",
            "dark" => "dark",
            _ => match self.host.window.theme() {
                tao::window::Theme::Light => "light",
                _ => "dark",
            },
        }
    }

    /// Applies the theme to the window frame and to the internal pages (UI, terminals, notes).
    fn apply_theme(&self) {
        #[cfg_attr(target_os = "linux", allow(unused_variables))]
        let frame = match self.config.theme.as_str() {
            "light" => Some(tao::window::Theme::Light),
            "dark" => Some(tao::window::Theme::Dark),
            _ => None,
        };
        // Title bar colour (Windows/macOS). On Linux GTK re-themes the whole window instead,
        // which breaks the embedded webviews, so the frame follows the desktop theme there.
        #[cfg(not(target_os = "linux"))]
        self.host.window.set_theme(frame);
        let theme = self.effective_theme();
        for t in &self.tabs {
            #[cfg(not(target_os = "linux"))]
            if let Some(w) = &t.window {
                w.set_theme(frame);
            }
            for p in &t.panes {
                if let (PaneContent::Terminal { .. } | PaneContent::Notes { .. }, Some(v)) = (&p.content, &p.view) {
                    let _ = v.evaluate_script(&format!("window.__brust_theme && __brust_theme('{theme}')"));
                }
            }
        }
        self.push_state();
    }

    fn delete_app(&mut self, id: &str) {
        if self.apps_managed() {
            return self.shell_call("appError", &json!("The apps are managed by the central configuration"));
        }
        let mut apps = self.config.apps.clone();
        apps.retain(|a| a.id != id);
        if let Err(e) = config::save_apps(&self.paths, &apps) {
            return self.shell_call("appError", &json!(e));
        }
        for ti in 0..self.tabs.len() {
            for pi in 0..self.tabs[ti].panes.len() {
                if matches!(&self.tabs[ti].panes[pi].content, PaneContent::App { app, .. } if app == id) {
                    self.drop_view(ti, pi);
                    self.tabs[ti].panes[pi].content = PaneContent::Empty;
                }
            }
        }
        self.reload_config();
        self.relayout();
        self.push_state();
        self.save_session();
        self.shell_call("appSaved", &Value::Null);
    }

    /// Recreates the open panes of an app so that new settings apply immediately.
    fn reopen_app_panes(&mut self, id: &str) {
        for ti in 0..self.tabs.len() {
            for pi in 0..self.tabs[ti].panes.len() {
                if matches!(&self.tabs[ti].panes[pi].content, PaneContent::App { app, .. } if app == id) && self.tabs[ti].panes[pi].view.is_some() {
                    self.tabs[ti].panes[pi].view = None;
                    if self.tabs[ti].is_floating() {
                        self.create_view(ti, pi);
                    }
                }
            }
        }
        self.ensure_views();
        self.relayout();
        self.push_state();
    }

    fn reload_config(&mut self) {
        let (mut cfg, err) = config::load_config(&self.paths, self.remote.as_ref());
        Self::resolve_token(&self.paths, &mut cfg);
        let port_changed = cfg.bridge.port != self.config.bridge.port || cfg.bridge.enabled != self.config.bridge.enabled;
        if err.is_none() {
            self.config = cfg;
            self.audit.set_enabled(self.config.privacy.audit_log);
            let wants_block = self.config.privacy.block_trackers || self.config.apps.iter().any(|a| a.block_trackers == Some(true));
            if wants_block && self.adblock.is_none() {
                self.adblock = Self::start_adblock(&self.config);
            }
        }
        self.config_error = err.clone();
        if let Some(e) = err {
            self.toast(&e);
        } else if port_changed {
            self.toast("WebSocket bridge changes will take effect on next launch.");
        }
        self.push_state();
    }

    /* ------------------------------------------------------------ pane IPC */

    fn send_to_pane(&self, pane: u32, msg: &Value) -> bool {
        match self.find_pane(pane).and_then(|(ti, pi)| self.tabs[ti].panes[pi].view.as_ref()) {
            Some(v) => v.evaluate_script(&format!("window.__brust_recv && window.__brust_recv({msg})")).is_ok(),
            None => false,
        }
    }

    fn app_panes(&self, target: &str) -> Vec<u32> {
        self.tabs
            .iter()
            .flat_map(|t| t.panes.iter())
            .filter(|p| p.view.is_some())
            .filter(|p| match &p.content {
                PaneContent::App { app, .. } => target == "*" || app == target,
                PaneContent::Url { .. } => target == "*",
                _ => false,
            })
            .map(|p| p.id)
            .collect()
    }

    /// Pane that answers calls for an app (preferring the active tab).
    fn call_target(&self, target: &str) -> Option<u32> {
        let panes = self.app_panes(target);
        let in_active: Vec<u32> = self.tabs.get(self.active).map(|t| t.panes.iter().map(|p| p.id).collect()).unwrap_or_default();
        panes.iter().copied().find(|p| in_active.contains(p)).or(panes.first().copied())
    }

    fn broadcast_ws(&self, msg: &Value) {
        let text = msg.to_string();
        for c in self.clients.values() {
            let _ = c.tx.send(text.clone());
        }
    }

    fn deliver_event(&self, target: Option<&str>, event: &str, data: &Value, source: &str, except: Option<u32>) {
        let msg = json!({"t": "event", "event": event, "data": data, "source": source});
        match target {
            None | Some("external") => {}
            Some(t) => {
                for p in self.app_panes(t) {
                    if Some(p) != except {
                        self.send_to_pane(p, &msg);
                    }
                }
            }
        }
        let ws_msg = json!({"type": "event", "source": source, "event": event, "data": data});
        if matches!(target, None | Some("*") | Some("external")) && !source.starts_with("external:") {
            self.broadcast_ws(&ws_msg);
        } else if let Some(name) = target.and_then(|t| t.strip_prefix("external:")) {
            // Event addressed to one local program (e.g. progress sent back to a calling program).
            if let Some(c) = self.ws_client_named(name).and_then(|id| self.clients.get(&id)) {
                let _ = c.tx.send(ws_msg.to_string());
            }
        }
    }

    /// WebSocket client with the given name (e.g. `?name=tester`); also accepts `external:name`.
    fn ws_client_named(&self, target: &str) -> Option<u32> {
        let name = target.strip_prefix("external:").unwrap_or(target);
        let mut ids: Vec<u32> = self.clients.iter().filter(|(_, c)| c.name == name).map(|(id, _)| *id).collect();
        ids.sort();
        ids.last().copied()
    }

    /// Starts a call to an open app or, if none exists, to external software with that name.
    fn start_call(&mut self, caller: Caller, target: &str, method: &str, params: &Value, from: &str) -> Result<(), String> {
        let pane = if target.starts_with("external:") { None } else { self.call_target(target) };
        let client = if pane.is_none() { self.ws_client_named(target) } else { None };
        if pane.is_none() && client.is_none() {
            return Err(format!("'{target}' unavailable: no open app or connected software with this name"));
        }
        let id = self.next_call;
        self.next_call += 1;
        self.calls.insert(id, caller);
        if let Some(pane) = pane {
            self.send_to_pane(pane, &json!({"t": "call", "id": id, "method": method, "params": params, "from": from}));
        } else if let Some(client) = client {
            self.calls_to_ws.insert(id, client);
            let msg = json!({"type": "call", "id": id, "method": method, "params": params, "from": from});
            if let Some(c) = self.clients.get(&client) {
                let _ = c.tx.send(msg.to_string());
            }
        }
        Ok(())
    }

    fn finish_call(&mut self, id: u64, result: Result<Value, String>) {
        self.calls_to_ws.remove(&id);
        match self.calls.remove(&id) {
            Some(Caller::Pane { pane, id }) => {
                self.send_to_pane(pane, &ok_reply(id, result));
            }
            Some(Caller::Ws { client, id }) => {
                let msg = match result {
                    Ok(v) => json!({"type": "result", "id": id, "ok": true, "result": v}),
                    Err(e) => json!({"type": "result", "id": id, "ok": false, "error": e}),
                };
                if let Some(c) = self.clients.get(&client) {
                    let _ = c.tx.send(msg.to_string());
                }
            }
            None => {}
        }
    }

    fn on_pane(&mut self, pane: u32, url: &str, body: &str) {
        let Some((ti, pi)) = self.find_pane(pane) else { return };
        let Ok(m) = serde_json::from_str::<Value>(body) else { return };
        let t = m["t"].as_str().unwrap_or("");
        if matches!(t, "focus" | "activity" | "shortcut" | "pty_in" | "notes_write" | "capture") {
            self.last_activity = Instant::now();
        }
        match t {
            "activity" => return,
            "focus" => {
                if !self.tabs[ti].is_floating() {
                    self.set_focus(ti, pi, false);
                }
                return;
            }
            "shortcut" => {
                if m["name"] == "focus_pane" || ti == self.active || self.tabs[ti].is_floating() {
                    self.shortcut(&m);
                }
                return;
            }
            "capture" => {
                if !self.locked {
                    let text: String = m["text"].as_str().unwrap_or("").chars().take(20_000).collect();
                    if text.trim().is_empty() {
                        self.toast("Select some text first, then press Ctrl+Shift+S");
                    } else {
                        let title = m["title"].as_str().unwrap_or("").to_string();
                        let page = m["url"].as_str().unwrap_or(url).to_string();
                        self.capture_to_notes(&text, &title, &page);
                    }
                }
                return;
            }
            _ => {}
        }
        let content = self.tabs[ti].panes[pi].content.clone();
        if let PaneContent::Terminal { shell } = &content {
            if !assets::is_internal(url) {
                return;
            }
            self.on_terminal(ti, pi, shell, &m);
            return;
        }
        if let PaneContent::Notes { notebook } = &content {
            if assets::is_internal(url) {
                self.on_notes(ti, pi, notebook, &m);
            }
            return;
        }
        let Some(app) = self.effective_app(&content) else { return };
        let app_id = app.id.clone();
        let source = if app_id.is_empty() { format!("pane:{pane}") } else { app_id.clone() };
        // The bridge only answers the app's own origins.
        if !app.owns_url(url) {
            if t == "invoke" {
                let id = m["id"].as_u64().unwrap_or(0);
                self.send_to_pane(pane, &ok_reply(id, Err(format!("origin not authorized for the bridge: {}", config::origin_of(url).unwrap_or_default()))));
            }
            return;
        }
        match t {
            "invoke" => {
                let id = m["id"].as_u64().unwrap_or(0);
                let cmd = m["cmd"].as_str().unwrap_or("").to_string();
                let args = m["args"].clone();
                self.invoke(ti, pi, &app, id, &cmd, args);
            }
            "emit" => {
                if !app.has("messaging") {
                    log::warn!("{source}: emit without 'messaging' permission");
                    return;
                }
                let event = m["event"].as_str().unwrap_or("").to_string();
                let target = m["target"].as_str().map(str::to_string);
                self.deliver_event(target.as_deref(), &event, &m["data"], &source, Some(pane));
            }
            "call_result" => {
                let id = m["id"].as_u64().unwrap_or(0);
                let res = if m["ok"].as_bool().unwrap_or(false) {
                    Ok(m["result"].clone())
                } else {
                    Err(m["error"].as_str().unwrap_or("error").to_string())
                };
                self.finish_call(id, res);
            }
            _ => {}
        }
    }

    fn invoke(&mut self, ti: usize, pi: usize, app: &AppDef, id: u64, cmd: &str, args: Value) {
        let pane = self.tabs[ti].panes[pi].id;
        let reply = |s: &App, r: Result<Value, String>| {
            s.send_to_pane(pane, &ok_reply(id, r));
        };
        match cmd {
            "ui.open" => {
                let content = if let Some(a) = args["app"].as_str() {
                    if self.app_def(a).is_none() {
                        return reply(self, Err(format!("unknown app: {a}")));
                    }
                    PaneContent::App { app: a.to_string(), url: args["url"].as_str().map(str::to_string) }
                } else if let Some(u) = args["url"].as_str() {
                    PaneContent::Url { url: u.to_string(), private: args["private"].as_bool().unwrap_or(false) }
                } else {
                    return reply(self, Err("specify 'app' or 'url'".into()));
                };
                self.open_tab(content);
                reply(self, Ok(Value::Null));
            }
            "ui.set_title" => {
                let title = args["title"].as_str().map(str::to_string).filter(|s| !s.is_empty());
                self.tabs[ti].panes[pi].custom_title = title;
                self.push_state();
                reply(self, Ok(Value::Null));
            }
            "ui.focus" => {
                let ti_now = ti;
                self.activate(ti_now);
                self.set_focus(ti_now, pi, true);
                self.host.window.set_focus();
                reply(self, Ok(Value::Null));
            }
            "ui.notify" => {
                let message = args["message"].as_str().unwrap_or("").to_string();
                let title = args["title"].as_str().map(str::to_string).unwrap_or_else(|| if app.name.is_empty() { "Brust".into() } else { app.name.clone() });
                self.toast(&message);
                if !self.host.window.is_focused() || args["native"].as_bool().unwrap_or(false) {
                    self.host.window.request_user_attention(Some(tao::window::UserAttentionType::Informational));
                    native_notification(&title, &message);
                }
                reply(self, Ok(Value::Null));
            }
            "ui.badge" => {
                self.tabs[ti].panes[pi].badge = args["count"].as_u64().unwrap_or(0).min(9999) as u32;
                self.push_state();
                reply(self, Ok(Value::Null));
            }
            "notes.append" | "notes.read" | "notes.list" => {
                if !app.has("notes") {
                    self.audit.record(&app.id, cmd, "", "refused", None);
                    return reply(self, Err("'notes' permission not granted".into()));
                }
                let nb = args["notebook"].as_str().unwrap_or("Inbox").to_string();
                let r = match cmd {
                    "notes.list" => Ok(Value::Array(self.notes.list())),
                    "notes.read" => self.notes.read(&nb).map(Value::String),
                    _ => {
                        let text = args["text"].as_str().unwrap_or("");
                        let block = if text.ends_with('\n') { text.to_string() } else { format!("{text}\n") };
                        let r = self.notes.append(&nb, &block).map(|_| Value::Null);
                        if r.is_ok() {
                            self.notify_notes_append(&nb, &block);
                        }
                        r
                    }
                };
                if cmd == "notes.append" {
                    self.audit.record(&app.id, cmd, &nb, "allowed", Some(&r));
                }
                reply(self, r);
            }
            "print.page" => {
                if !app.has("print") {
                    self.audit.record(&app.id, cmd, "", "refused", None);
                    return reply(self, Err("'print' permission not granted".into()));
                }
                let printer = args["printer"].as_str().map(str::to_string).or_else(|| self.config.default_printer.clone());
                let silent = args["silent"].as_bool().unwrap_or(true);
                self.audit.record(&app.id, cmd, printer.as_deref().unwrap_or("default printer"), "allowed", None);
                let p = self.proxy.clone();
                if let Some(v) = &self.tabs[ti].panes[pi].view {
                    print::print_page(
                        v,
                        printer.as_deref(),
                        silent,
                        Box::new(move |result| {
                            let _ = p.send_event(UserEvent::TaskDone { pane, id, result: result.map(|_| Value::Null) });
                        }),
                    );
                }
            }
            "bridge.call" => {
                if !app.has("messaging") {
                    return reply(self, Err("'messaging' permission not granted".into()));
                }
                let target = args["target"].as_str().unwrap_or("").to_string();
                let method = args["method"].as_str().unwrap_or("").to_string();
                let from = if app.id.is_empty() { format!("pane:{pane}") } else { app.id.clone() };
                if let Err(e) = self.start_call(Caller::Pane { pane, id }, &target, &method, &args["params"], &from) {
                    reply(self, Err(e));
                }
            }
            "bridge.clients" => {
                if !app.has("messaging") {
                    return reply(self, Err("'messaging' permission not granted".into()));
                }
                let list: Vec<Value> = self.clients.iter().map(|(id, c)| json!({"id": id, "name": c.name})).collect();
                reply(self, Ok(Value::Array(list)));
            }
            "bridge.apps" => {
                let mut open: Vec<String> = self
                    .tabs
                    .iter()
                    .flat_map(|t| t.panes.iter())
                    .filter_map(|p| match &p.content {
                        PaneContent::App { app, .. } if p.view.is_some() => Some(app.clone()),
                        _ => None,
                    })
                    .collect();
                open.sort();
                open.dedup();
                reply(self, Ok(json!({"configured": self.config.apps.iter().map(|a| &a.id).collect::<Vec<_>>(), "open": open})));
            }
            _ => {
                let app_label = if app.id.is_empty() { app.url.clone() } else { app.id.clone() };
                match locals::permission_for(cmd) {
                    None => reply(self, Err(format!("unknown command: {cmd}"))),
                    Some(perm) if !perm.is_empty() && !app.has(perm) => {
                        self.audit.record(&app_label, cmd, &audit::summarize(cmd, &args), "refused", None);
                        reply(self, Err(format!("'{perm}' permission not granted to the app")));
                    }
                    Some(_) => {
                        let must_confirm = audit::is_sensitive(cmd)
                            && app.confirm.unwrap_or(self.config.privacy.confirm_sensitive)
                            && !self.grants.contains(&Grants::key(&app_label, cmd, &args));
                        if must_confirm {
                            self.ask_confirm(pane, id, app.clone(), cmd, args);
                        } else {
                            self.run_local(pane, id, app.clone(), cmd, args, "allowed");
                        }
                    }
                }
            }
        }
    }

    /// Notebook that receives quick captures: the one open in the active tab, otherwise "Inbox".
    fn capture_notebook(&self) -> String {
        self.tabs
            .get(self.active)
            .and_then(|t| {
                t.visible().iter().find_map(|p| match &p.content {
                    PaneContent::Notes { notebook } => Some(notebook.clone()),
                    _ => None,
                })
            })
            .unwrap_or_else(|| "Inbox".into())
    }

    fn capture_to_notes(&mut self, text: &str, title: &str, page: &str) {
        let nb = self.capture_notebook();
        let block = notes::capture_block(text, title, page);
        match self.notes.append(&nb, &block) {
            Ok(()) => {
                self.notify_notes_append(&nb, &block);
                self.toast(&format!("Saved to notes: {nb}"));
            }
            Err(e) => self.toast(&format!("Cannot save the note: {e}")),
        }
    }

    /// Tells the notes panes showing `notebook` that text was appended on disk.
    fn notify_notes_append(&self, notebook: &str, text: &str) {
        let msg = json!({"kind": "append", "name": notebook, "text": text});
        for p in self.tabs.iter().flat_map(|t| t.panes.iter()) {
            if let (PaneContent::Notes { notebook: nb }, Some(v)) = (&p.content, &p.view)
                && nb == notebook {
                    let _ = v.evaluate_script(&format!("window.__notes && __notes({msg})"));
                }
        }
    }

    fn on_notes(&mut self, ti: usize, pi: usize, notebook: &str, m: &Value) {
        let send = |s: &App, v: Value| {
            if let Some(view) = &s.tabs[ti].panes[pi].view {
                let _ = view.evaluate_script(&format!("window.__notes && __notes({v})"));
            }
        };
        let name = m["name"].as_str().unwrap_or("").to_string();
        let error = |e: String| json!({"kind": "error", "message": e});
        match m["t"].as_str().unwrap_or("") {
            "notes_init" => {
                if let Some(view) = &self.tabs[ti].panes[pi].view {
                    let _ = view.evaluate_script(&format!("window.__brust_theme && __brust_theme('{}')", self.effective_theme()));
                }
                send(self, json!({"kind": "init", "notebook": notebook, "list": self.notes.list()}));
            }
            "notes_list" => send(self, json!({"kind": "list", "list": self.notes.list()})),
            "notes_read" => match self.notes.read(&name) {
                Ok(text) => send(self, json!({"kind": "read", "name": name, "text": text})),
                Err(e) => send(self, error(e)),
            },
            "notes_write" => match self.notes.write(&name, m["text"].as_str().unwrap_or("")) {
                Ok(()) => send(self, json!({"kind": "saved", "name": name})),
                Err(e) => send(self, error(e)),
            },
            "notes_search" => {
                let q = m["q"].as_str().unwrap_or("");
                send(self, json!({"kind": "search", "q": q, "results": self.notes.search(q)}));
            }
            "notes_open" => match notes::Notes::sanitize(&name) {
                Ok(clean) => {
                    self.tabs[ti].panes[pi].content = PaneContent::Notes { notebook: clean.clone() };
                    let text = self.notes.read(&clean).unwrap_or_default();
                    send(self, json!({"kind": "read", "name": clean, "text": text}));
                    self.push_state();
                    self.save_session();
                }
                Err(e) => send(self, error(e)),
            },
            "notes_delete" => {
                if let Err(e) = self.notes.delete(&name) {
                    send(self, error(e));
                }
                send(self, json!({"kind": "list", "list": self.notes.list()}));
            }
            "notes_rename" => {
                let from = m["from"].as_str().unwrap_or("").to_string();
                let to = m["to"].as_str().unwrap_or("").to_string();
                match self.notes.rename(&from, &to) {
                    Ok(()) => {
                        let clean = notes::Notes::sanitize(&to).unwrap_or(to);
                        send(self, json!({"kind": "list", "list": self.notes.list()}));
                        if from == notebook {
                            self.tabs[ti].panes[pi].content = PaneContent::Notes { notebook: clean.clone() };
                            let text = self.notes.read(&clean).unwrap_or_default();
                            send(self, json!({"kind": "read", "name": clean, "text": text}));
                            self.push_state();
                            self.save_session();
                        }
                    }
                    Err(e) => send(self, error(e)),
                }
            }
            "open_link" => {
                let link = m["url"].as_str().unwrap_or("");
                if link.starts_with("http://") || link.starts_with("https://") || link.starts_with("mailto:") {
                    let _ = open::that_detached(link);
                }
            }
            _ => {}
        }
    }

    /// Handles a launch request (command line, brust:// link, second instance).
    fn launch(&mut self, l: Launch) {
        match l {
            Launch::App { app, url } => {
                if self.app_def(&app).is_some() {
                    self.open_tab(PaneContent::App { app, url });
                } else {
                    self.toast(&format!("Unknown app: {app}"));
                }
            }
            Launch::Url(url) => self.open_tab(PaneContent::Url { url, private: false }),
            Launch::Template(name) => self.open_template(&name),
            Launch::Notes(nb) => self.open_tab(PaneContent::Notes { notebook: nb.unwrap_or_else(|| "Inbox".into()) }),
        }
    }

    fn on_terminal(&mut self, ti: usize, pi: usize, shell_id: &str, m: &Value) {
        let cols = m["cols"].as_u64().unwrap_or(80).clamp(2, 1000) as u16;
        let rows = m["rows"].as_u64().unwrap_or(24).clamp(2, 1000) as u16;
        let pane_id = self.tabs[ti].panes[pi].id;
        match m["t"].as_str().unwrap_or("") {
            "pty_ready" | "pty_restart" => {
                if let Some(v) = &self.tabs[ti].panes[pi].view {
                    let _ = v.evaluate_script(&format!("window.__brust_theme && __brust_theme('{}')", self.effective_theme()));
                }
                let restart = m["t"] == "pty_restart";
                let p = &mut self.tabs[ti].panes[pi];
                if let (Some(pty), false) = (&p.pty, restart) {
                    pty.resize(cols, rows);
                    return;
                }
                if p.pty_exited && !restart {
                    return;
                }
                let Some(shell) = self.config.shells.iter().find(|s| s.id == shell_id).cloned() else {
                    let msg = format!("\r\nshell '{shell_id}' not configured\r\n");
                    if let Some(v) = &self.tabs[ti].panes[pi].view {
                        let _ = v.evaluate_script(&format!("__pty_out({})", json!(B64.encode(msg))));
                    }
                    return;
                };
                match PtySession::spawn(&shell, cols, rows, pane_id, self.proxy.clone()) {
                    Ok(s) => {
                        let p = &mut self.tabs[ti].panes[pi];
                        p.pty = Some(s);
                        p.pty_exited = false;
                    }
                    Err(e) => {
                        let msg = format!("\r\ncannot start {}: {e}\r\n", shell.program);
                        if let Some(v) = &self.tabs[ti].panes[pi].view {
                            let _ = v.evaluate_script(&format!("__pty_out({}); __pty_exit()", json!(B64.encode(msg))));
                        }
                    }
                }
                self.push_state();
            }
            "pty_in" => {
                if let (Some(pty), Some(d)) = (&mut self.tabs[ti].panes[pi].pty, m["data"].as_str()) {
                    pty.write(d.as_bytes());
                }
            }
            "pty_resize" => {
                if let Some(pty) = &self.tabs[ti].panes[pi].pty {
                    pty.resize(cols, rows);
                }
            }
            "copy" => {
                if let Some(text) = m["text"].as_str()
                    && let Ok(mut cb) = arboard::Clipboard::new() {
                        let _ = cb.set_text(text.to_string());
                    }
            }
            "paste" => {
                let text = arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok()).unwrap_or_default();
                if let Some(pty) = &mut self.tabs[ti].panes[pi].pty {
                    // Bracketed paste is not guaranteed on every shell: send the normalized text.
                    let text = if cfg!(windows) { text.replace("\r\n", "\r") } else { text.replace("\r\n", "\n") };
                    pty.write(text.as_bytes());
                }
            }
            _ => {}
        }
    }

    /* ---------------------------------------------------------- WebSocket */

    fn on_ws(&mut self, ev: WsEvent) {
        match ev {
            WsEvent::Connected { client, name, tx } => {
                let hello = json!({
                    "type": "welcome", "client": client, "version": env!("CARGO_PKG_VERSION"),
                    "apps": self.config.apps.iter().map(|a| &a.id).collect::<Vec<_>>(),
                });
                let _ = tx.send(hello.to_string());
                self.clients.insert(client, WsClient { name: name.clone(), tx });
                if name != "brust-launcher" {
                    self.deliver_event(Some("*"), "bridge.client_connected", &json!({"id": client, "name": name}), "brust", None);
                }
                self.push_state();
            }
            WsEvent::Disconnected { client } => {
                if let Some(c) = self.clients.remove(&client).filter(|c| c.name != "brust-launcher") {
                    self.deliver_event(Some("*"), "bridge.client_disconnected", &json!({"id": client, "name": c.name}), "brust", None);
                }
                self.calls.retain(|_, c| !matches!(c, Caller::Ws { client: cl, .. } if *cl == client));
                // Calls awaiting a reply from this software fail immediately.
                let orphaned: Vec<u64> = self.calls_to_ws.iter().filter(|(_, c)| **c == client).map(|(id, _)| *id).collect();
                for id in orphaned {
                    self.finish_call(id, Err("the external software disconnected before replying".into()));
                }
                self.push_state();
            }
            WsEvent::Message { client, text } => {
                let reply = |s: &App, v: Value| {
                    if let Some(c) = s.clients.get(&client) {
                        let _ = c.tx.send(v.to_string());
                    }
                };
                let Ok(m) = serde_json::from_str::<Value>(&text) else {
                    return reply(self, json!({"type": "error", "error": "invalid JSON"}));
                };
                let id = m["id"].clone();
                let name = self.clients.get(&client).map(|c| c.name.clone()).unwrap_or_default();
                let ok = |result: Value| json!({"type": "result", "id": id, "ok": true, "result": result});
                let err = |e: String| json!({"type": "result", "id": id, "ok": false, "error": e});
                match m["type"].as_str().unwrap_or("") {
                    "ping" => reply(self, json!({"type": "pong", "id": id})),
                    "list" => {
                        let tabs: Vec<Value> = self
                            .tabs
                            .iter()
                            .enumerate()
                            .map(|(i, t)| {
                                json!({
                                    "index": i, "title": self.tab_title(t).0, "active": i == self.active, "layout": t.layout.id(),
                                    "panes": t.panes.iter().map(|p| json!({"id": p.id, "content": p.content, "loaded": p.view.is_some()})).collect::<Vec<_>>(),
                                })
                            })
                            .collect();
                        reply(self, ok(json!({"tabs": tabs, "apps": self.config.apps.iter().map(|a| json!({"id": a.id, "name": a.name, "url": a.url})).collect::<Vec<_>>()})));
                    }
                    "emit" => {
                        let target = m["target"].as_str().unwrap_or("*").to_string();
                        let event = m["event"].as_str().unwrap_or("").to_string();
                        self.deliver_event(Some(&target), &event, &m["data"], &format!("external:{name}"), None);
                        reply(self, ok(Value::Null));
                    }
                    "call" => {
                        let target = m["target"].as_str().unwrap_or("").to_string();
                        let method = m["method"].as_str().unwrap_or("").to_string();
                        if let Err(e) = self.start_call(Caller::Ws { client, id: id.clone() }, &target, &method, &m["params"], &format!("external:{name}")) {
                            reply(self, err(e));
                        }
                    }
                    "open" => {
                        let content = if let Some(a) = m["app"].as_str() {
                            if self.app_def(a).is_none() {
                                return reply(self, err(format!("unknown app: {a}")));
                            }
                            PaneContent::App { app: a.to_string(), url: m["url"].as_str().map(str::to_string) }
                        } else if let Some(u) = m["url"].as_str() {
                            PaneContent::Url { url: u.to_string(), private: m["private"].as_bool().unwrap_or(false) }
                        } else {
                            return reply(self, err("specify 'app' or 'url'".into()));
                        };
                        self.open_tab(content);
                        self.host.window.set_focus();
                        reply(self, ok(Value::Null));
                    }
                    "focus" => {
                        let target = m["app"].as_str().unwrap_or("");
                        match self.call_target(target).and_then(|p| self.find_pane(p)) {
                            Some((ti, pi)) => {
                                self.activate(ti);
                                self.set_focus(ti, pi, true);
                                self.host.window.set_focus();
                                reply(self, ok(Value::Null));
                            }
                            None => reply(self, err(format!("app '{target}' is not open"))),
                        }
                    }
                    "reply" => {
                        // Reply to a call received from an app ({"type":"call",...}).
                        let call_id = m["id"].as_u64().unwrap_or(0);
                        if self.calls_to_ws.get(&call_id) == Some(&client) {
                            let res = if m["ok"].as_bool().unwrap_or(true) {
                                Ok(m["result"].clone())
                            } else {
                                Err(m["error"].as_str().unwrap_or("error").to_string())
                            };
                            self.finish_call(call_id, res);
                        }
                    }
                    "reload" => {
                        self.reload_tab();
                        reply(self, ok(Value::Null));
                    }
                    "open_template" => {
                        let name = m["name"].as_str().unwrap_or("").to_string();
                        if self.templates.iter().any(|t| t.name == name) {
                            self.open_template(&name);
                            reply(self, ok(Value::Null));
                        } else {
                            reply(self, err(format!("template '{name}' not found")));
                        }
                    }
                    "notes_append" => {
                        // Local software can write into the user's notebooks (e.g. a test log).
                        let nb = m["notebook"].as_str().unwrap_or("Inbox").to_string();
                        let text = m["text"].as_str().unwrap_or("");
                        let block = if text.ends_with('\n') { text.to_string() } else { format!("{text}\n") };
                        let r = self.notes.append(&nb, &block);
                        self.audit.record(&format!("external:{name}"), "notes.append", &nb, "allowed", Some(&r.clone().map(|_| Value::Null)));
                        match r {
                            Ok(()) => {
                                self.notify_notes_append(&nb, &block);
                                reply(self, ok(Value::Null));
                            }
                            Err(e) => reply(self, err(e)),
                        }
                    }
                    "cli" => {
                        // A second Brust instance forwarded its command line.
                        let args: Vec<String> = m["args"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
                        for l in cli::parse(&args) {
                            self.launch(l);
                        }
                        self.host.window.set_minimized(false);
                        self.host.window.set_focus();
                        reply(self, ok(Value::Null));
                    }
                    other => reply(self, err(format!("unknown message type: {other}"))),
                }
            }
        }
    }

    /* ---------------------------------------------------------- events */

    fn handle(&mut self, ev: UserEvent) {
        match ev {
            UserEvent::ShellIpc(body) => self.on_shell(&body),
            UserEvent::PaneIpc { pane, url, body } => self.on_pane(pane, &url, &body),
            UserEvent::PtyOutput { pane, data } => {
                if let Some(v) = self.find_pane(pane).and_then(|(ti, pi)| self.tabs[ti].panes[pi].view.as_ref()) {
                    let _ = v.evaluate_script(&format!("__pty_out(\"{}\")", B64.encode(data)));
                }
            }
            UserEvent::PtyExit { pane } => {
                if let Some((ti, pi)) = self.find_pane(pane) {
                    let p = &mut self.tabs[ti].panes[pi];
                    if p.pty.is_some() {
                        p.pty = None;
                        p.pty_exited = true;
                        if let Some(v) = &p.view {
                            let _ = v.evaluate_script("__pty_exit()");
                        }
                        self.push_state();
                    }
                }
            }
            UserEvent::TaskDone { pane, id, result } => {
                self.send_to_pane(pane, &ok_reply(id, result));
            }
            UserEvent::SerialData { pane, handle, data } => {
                let msg = json!({"t": "event", "event": "serial.data", "source": "brust",
                    "data": {"handle": handle, "base64": B64.encode(&data), "text": String::from_utf8_lossy(&data)}});
                self.send_to_pane(pane, &msg);
            }
            UserEvent::SerialClosed { pane, handle, error } => {
                let msg = json!({"t": "event", "event": "serial.closed", "source": "brust", "data": {"handle": handle, "error": error}});
                self.send_to_pane(pane, &msg);
            }
            UserEvent::Title { pane, title } => {
                if let Some((ti, pi)) = self.find_pane(pane) {
                    self.tabs[ti].panes[pi].doc_title = title;
                    if ti == self.active {
                        self.push_state();
                    }
                }
            }
            UserEvent::Loaded { pane, url } => {
                log::debug!("pane {pane} loaded: {url}");
            }
            UserEvent::NewWindow { pane, url } => {
                let Some((ti, pi)) = self.find_pane(pane) else { return };
                let content = self.tabs[ti].panes[pi].content.clone();
                match self.effective_app(&content) {
                    Some(app) if !app.id.is_empty() && app.owns_url(&url) => {
                        self.open_tab(PaneContent::App { app: app.id, url: Some(url) })
                    }
                    Some(app) if app.id.is_empty() || !app.external_links_in_browser => self.open_tab(PaneContent::Url { url, private: false }),
                    _ => {
                        let _ = open::that_detached(&url);
                    }
                }
            }
            UserEvent::Ws(e) => self.on_ws(e),
            UserEvent::Tick => self.on_tick(),
            UserEvent::Audited { pane, id, app, cmd, detail, decision, result } => {
                self.audit.record(&app, &cmd, &detail, &decision, Some(&result));
                self.send_to_pane(pane, &ok_reply(id, result));
            }
            UserEvent::RemoteConfig(r) => match r {
                Ok(v) => {
                    let changed = self.remote.as_ref() != Some(&v);
                    self.remote_status = format!("verified {}", chrono::Local::now().format("%Y-%m-%d %H:%M"));
                    if changed {
                        config::save_json(&self.paths.remote_cache(), &v);
                        self.remote = Some(v);
                        self.reload_config();
                        self.toast("Central configuration updated");
                    }
                    self.push_state();
                }
                Err(e) => {
                    log::warn!("central configuration: {e}");
                    self.remote_status = format!("error: {e}");
                    self.push_state();
                }
            },
            UserEvent::UpdateChecked(r) => {
                match r {
                    Ok(Some(info)) => {
                        self.update_status = format!("version {} available", info.version);
                        native_notification("Brust", &format!("Version {} is available (Settings → Updates)", info.version));
                        self.toast(&format!("Brust {} is available: Settings → Updates", info.version));
                        self.update_info = Some(info);
                    }
                    Ok(None) => self.update_status = "up to date".into(),
                    Err(e) => self.update_status = format!("error: {e}"),
                }
                self.push_state();
            }
            UserEvent::UpdateDownloaded(r) => match r.and_then(|path| {
                self.save_session();
                update::apply_and_restart(&path)
            }) {
                Ok(()) => self.exit_requested = true,
                Err(e) => {
                    self.update_status = format!("error: {e}");
                    self.toast(&format!("Update failed: {e}"));
                    self.push_state();
                }
            },
        }
    }

    fn on_tick(&mut self) {
        self.ticks += 1;
        let minutes = self.config.privacy.lock_after_minutes;
        if minutes > 0 && !self.locked && self.last_activity.elapsed() > Duration::from_secs(minutes as u64 * 60) && self.pin_set() {
            self.lock();
        }
        if let Some(rc) = &self.config.remote_config {
            // A tick every 20 s: three per minute.
            if rc.refresh_minutes > 0 && self.ticks.is_multiple_of(rc.refresh_minutes as u64 * 3) {
                self.fetch_remote_config();
            }
        }
    }

    /// Handles events of detached windows. Returns true if the event belonged to one.
    fn on_floating_window(&mut self, id: tao::window::WindowId, event: &WindowEvent) -> bool {
        let Some(ti) = self.tabs.iter().position(|t| t.window.as_ref().is_some_and(|w| w.id() == id)) else { return false };
        match event {
            WindowEvent::CloseRequested => self.close_tab(ti),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => self.relayout(),
            _ => {}
        }
        true
    }

    /// Frees the browser profiles and deletes the data of `clear_on_exit` apps.
    fn shutdown(&mut self) {
        self.save_session();
        for t in &mut self.tabs {
            for p in &mut t.panes {
                p.view = None;
                p.pty = None;
            }
        }
        self.tabs.clear();
        self.shell = None;
        self.contexts.clear();
        Self::clear_profiles(&self.paths, &self.config);
    }
}

/// Native desktop notification (Windows toast, Linux notification daemon).
fn native_notification(title: &str, body: &str) {
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        if let Err(e) = notify_rust::Notification::new().appname("Brust").summary(&title).body(&body).show() {
            log::debug!("notification: {e}");
        }
    });
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Signing tools: brust --keygen / --sign / --verify / --hash-file
    #[cfg(windows)]
    if args.first().is_some_and(|a| matches!(a.as_str(), "--keygen" | "--sign" | "--verify" | "--hash-file")) {
        // The release build has no console of its own: print into the calling terminal.
        unsafe {
            windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS);
        }
    }
    if let Some(code) = trust::run_cli(&args) {
        std::process::exit(code);
    }
    update::cleanup_old();
    // Single instance: if Brust is already running, hand over the command line and exit.
    if cli::forward_to_running_instance(&args) {
        return;
    }
    let launch = cli::parse(&args);

    #[cfg(target_os = "linux")]
    {
        // WebKitGTK: the DMABUF renderer produces black panes on some VMs/drivers.
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
        }
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("Brust")
        .with_window_icon(window_icon())
        .with_inner_size(LogicalSize::new(1400.0, 900.0))
        .with_min_inner_size(LogicalSize::new(640.0, 420.0))
        .build(&event_loop)
        .expect("window creation");
    let main_window = window.id();
    let host = Host::new(window);
    let mut app = App::new(host, proxy, launch);

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { window_id, event, .. } if window_id != main_window => {
                app.on_floating_window(window_id, &event);
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    app.relayout();
                    app.push_state();
                }
                WindowEvent::ThemeChanged(_) => app.apply_theme(),
                WindowEvent::CloseRequested => {
                    if app.kiosk() {
                        app.toast("Kiosk mode: press Ctrl+Alt+Shift+K and enter the PIN to exit");
                    } else {
                        app.shutdown();
                        *control_flow = ControlFlow::Exit;
                    }
                }
                _ => {}
            },
            Event::UserEvent(e) => app.handle(e),
            _ => {}
        }
        for pane in std::mem::take(&mut app.pending_detach) {
            app.detach(pane, target);
        }
        if app.exit_requested {
            app.shutdown();
            *control_flow = ControlFlow::Exit;
        }
    });
}
