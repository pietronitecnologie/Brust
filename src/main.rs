#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Ponte – client desktop dedicato per applicativi web.
//!
//! * schede (tab), ciascuna con un tileset di 1–4 pannelli ridimensionabili;
//! * ogni pannello mostra un applicativo, un URL o un terminale (PowerShell, cmd, …);
//! * bridge JS (`window.ponte`) verso capacità locali e verso software esterni (WebSocket).

mod assets;
mod config;
mod layout;
mod locals;
mod pty;
mod ws;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use tao::dpi::{LogicalPosition, LogicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::{Window, WindowBuilder};
use wry::{NewWindowResponse, PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder};

use config::{AppDef, Config, PaneContent, Paths, SavedTab, Session, SizeMemory};
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
}

/* ------------------------------------------------------------------ host */

/// Finestra che ospita le webview figlie (su Linux tramite un gtk::Fixed).
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

    /// Dimensione logica dell'area client.
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
}

impl Pane {
    fn new(id: u32, content: PaneContent) -> Self {
        Pane { id, content, view: None, doc_title: String::new(), custom_title: None, picking: false, pty: None, pty_exited: false }
    }
}

struct Tab {
    id: u32,
    // `panes` può contenere più pannelli di quanti il layout ne mostri:
    // quelli in eccesso restano "parcheggiati" (nascosti ma attivi) e
    // riappaiono quando si torna a un layout con più tile.
    title: Option<String>,
    layout: Layout,
    ratios: Vec<f64>,
    panes: Vec<Pane>,
    focused: usize,
}

impl Tab {
    /// Pannelli mostrati dal layout corrente.
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

struct App {
    host: Host,
    ctx: WebContext,
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
    next_call: u64,
    bridge_status: String,
}

fn ok_reply(id: u64, result: Result<Value, String>) -> Value {
    match result {
        Ok(v) => json!({"t": "reply", "id": id, "ok": true, "result": v}),
        Err(e) => json!({"t": "reply", "id": id, "ok": false, "error": e}),
    }
}

impl App {
    fn new(host: Host, proxy: EventLoopProxy<UserEvent>) -> Self {
        let paths = Paths::new();
        let (config, config_error) = config::load_config(&paths);
        let sizes: SizeMemory = config::load_json(&paths.sizes()).unwrap_or_default();
        let ctx = WebContext::new(Some(paths.webdata()));
        let locals = Locals::new(proxy.clone());
        let mut app = App {
            host,
            ctx,
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
            next_call: 1,
            bridge_status: "non avviato".into(),
        };
        app.start_bridge();
        app.build_shell();
        app.restore_session();
        app
    }

    fn start_bridge(&mut self) {
        let b = &self.config.bridge;
        if !b.enabled {
            self.bridge_status = "disabilitato".into();
            return;
        }
        self.bridge_status = match ws::start(b.port, b.token.clone(), self.proxy.clone()) {
            Ok(()) => format!("in ascolto su 127.0.0.1:{}", b.port),
            Err(e) => format!("errore: {e}"),
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
        let builder = WebViewBuilder::new_with_web_context(&mut self.ctx)
            .with_custom_protocol(assets::SCHEME.into(), assets::handle)
            .with_url(assets::url("shell.html"))
            .with_bounds(rect(R::new(0.0, 0.0, w, h)))
            .with_background_color((21, 23, 28, 255))
            .with_devtools(self.config.devtools)
            .with_hotkeys_zoom(false)
            .with_ipc_handler(move |req| {
                let _ = proxy.send_event(UserEvent::ShellIpc(req.into_body()));
            });
        match self.host.build(builder) {
            Ok(v) => self.shell = Some(v),
            Err(e) => panic!("impossibile creare l'interfaccia: {e}"),
        }
    }

    /* ------------------------------------------------------------ sessione */

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
            self.tabs.push(Tab { id, title: t.title, layout: t.layout, ratios: t.layout.sanitize(&t.ratios), panes, focused: 0 });
        }
        if self.tabs.is_empty() {
            self.push_tab(PaneContent::Empty);
        }
        self.active = session.active.min(self.tabs.len() - 1);
        self.activate(self.active);
    }

    fn save_session(&self) {
        let s = Session {
            active: self.active,
            tabs: self
                .tabs
                .iter()
                .map(|t| SavedTab {
                    title: t.title.clone(),
                    layout: t.layout,
                    ratios: t.ratios.clone(),
                    panes: t.panes.iter().map(|p| p.content.clone()).collect(),
                })
                .collect(),
        };
        config::save_json(&self.paths.session(), &s);
    }

    /* ---------------------------------------------------- memoria tile */

    fn size_keys(t: &Tab) -> (String, String) {
        let all: Vec<String> = t.visible().iter().map(|p| p.content.key()).collect();
        (
            format!("{}|{}", t.layout.id(), all.join(",")),
            format!("{}|first:{}", t.layout.id(), all.first().cloned().unwrap_or_default()),
        )
    }

    /// Applica le proporzioni ricordate per questa combinazione di layout e applicazioni.
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

    /// Definizione "effettiva" per un pannello: app configurata, o app anonima per URL liberi.
    fn effective_app(&self, content: &PaneContent) -> Option<AppDef> {
        match content {
            PaneContent::App { app, .. } => self.app_def(app).cloned(),
            PaneContent::Url { url } => Some(AppDef {
                id: String::new(),
                name: config::origin_of(url).unwrap_or_else(|| url.clone()),
                url: url.clone(),
                icon: None,
                allowed_origins: vec![],
                external_links_in_browser: false,
                permissions: vec![],
                exec_allow: vec![],
                fs_roots: vec![],
                user_agent: None,
                zoom: None,
                inject_script: None,
            }),
            _ => None,
        }
    }

    fn create_view(&mut self, ti: usize, pi: usize) {
        let pane_id = self.tabs[ti].panes[pi].id;
        let content = self.tabs[ti].panes[pi].content.clone();
        let devtools = self.config.devtools;
        let proxy = self.proxy.clone();
        let ipc_proxy = self.proxy.clone();
        let ipc = move |req: wry::http::Request<String>| {
            let url = req.uri().to_string();
            let _ = ipc_proxy.send_event(UserEvent::PaneIpc { pane: pane_id, url, body: req.into_body() });
        };
        let result = match &content {
            PaneContent::Empty => return,
            PaneContent::Terminal { .. } => {
                #[allow(unused_mut)]
                let mut b = WebViewBuilder::new_with_web_context(&mut self.ctx);
                // Su Linux il protocollo è registrato una sola volta nel WebContext condiviso
                // (dall'interfaccia), su Windows/macOS va registrato per ogni webview.
                if !cfg!(target_os = "linux") {
                    b = b.with_custom_protocol(assets::SCHEME.into(), assets::handle);
                }
                let b = b
                    .with_url(assets::url("terminal.html"))
                    .with_background_color((15, 17, 21, 255))
                    .with_devtools(devtools)
                    .with_visible(false)
                    .with_ipc_handler(ipc);
                self.host.build(b)
            }
            PaneContent::App { .. } | PaneContent::Url { .. } => {
                let Some(app) = self.effective_app(&content) else {
                    self.tabs[ti].panes[pi].doc_title = "applicativo non trovato nella configurazione".into();
                    return;
                };
                let start_url = match &content {
                    PaneContent::App { url: Some(u), .. } => u.clone(),
                    _ => app.url.clone(),
                };
                let cfg = json!({"paneId": pane_id, "appId": app.id, "version": env!("CARGO_PKG_VERSION")});
                let mut script = assets::BRIDGE_JS.replace("__PONTE_CFG__", &cfg.to_string());
                if let Some(path) = &app.inject_script {
                    match std::fs::read_to_string(path) {
                        Ok(extra) => {
                            script.push_str("\n;");
                            script.push_str(&extra);
                        }
                        Err(e) => log::warn!("inject_script {path}: {e}"),
                    }
                }
                let nav_app = app.clone();
                let p_title = proxy.clone();
                let p_load = proxy.clone();
                let p_new = proxy.clone();
                let mut b = WebViewBuilder::new_with_web_context(&mut self.ctx)
                    .with_url(start_url)
                    .with_initialization_script_for_main_only(script, true)
                    .with_devtools(devtools)
                    .with_visible(false)
                    .with_back_forward_navigation_gestures(false)
                    .with_ipc_handler(ipc)
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
                let r = self.host.build(b);
                if let (Ok(v), Some(z)) = (&r, app.zoom) {
                    let _ = v.zoom(z);
                }
                r
            }
        };
        match result {
            Ok(v) => self.tabs[ti].panes[pi].view = Some(v),
            Err(e) => {
                log::error!("creazione webview: {e}");
                self.tabs[ti].panes[pi].doc_title = format!("errore: {e}");
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

    /// Posiziona e mostra/nasconde le webview della scheda attiva.
    fn relayout(&mut self) {
        let (w, h) = self.host.size();
        if let Some(s) = &self.shell {
            self.host.place(s, R::new(0.0, 0.0, w, h));
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
                let visible = !self.overlay && !p.picking && vr.w > 4.0 && vr.h > 4.0;
                // Prima si rende visibile, poi si posiziona: GTK, quando rimostra un widget,
                // ne ricalcola l'allocazione e annullerebbe le dimensioni appena impostate.
                let _ = v.set_visible(visible);
                self.host.place(v, vr);
            }
        }
    }

    fn activate(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if index != self.active {
            if let Some(old) = self.tabs.get(self.active) {
                for p in &old.panes {
                    if let Some(v) = &p.view {
                        let _ = v.set_visible(false);
                    }
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
        if let Some(p) = t.panes.get(t.focused) {
            if let Some(v) = &p.view {
                if !p.picking && !self.overlay {
                    let _ = v.focus();
                    if matches!(p.content, PaneContent::Terminal { .. }) {
                        let _ = v.evaluate_script("window.__term_focus && __term_focus()");
                    }
                }
            }
        }
    }

    fn push_tab(&mut self, content: PaneContent) {
        let pid = self.id();
        let id = self.id();
        self.tabs.push(Tab { id, title: None, layout: Layout::Single, ratios: vec![], panes: vec![Pane::new(pid, content)], focused: 0 });
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
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.push_tab(PaneContent::Empty);
        }
        let next = if self.active > index || self.active >= self.tabs.len() { self.active.saturating_sub(1) } else { self.active };
        self.active = usize::MAX; // forza attivazione
        self.activate(next.min(self.tabs.len() - 1));
    }

    fn set_layout(&mut self, layout: Layout) {
        let ti = self.active;
        let n = layout.pane_count();
        if self.tabs[ti].panes.len() > n {
            // Restringendo il layout restano visibili prima i pannelli con contenuto;
            // gli altri vengono parcheggiati (non chiusi). I vuoti in eccesso si eliminano.
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

    /* ---------------------------------------------------------- stato UI */

    fn content_label(&self, c: &PaneContent) -> (String, String, Option<String>) {
        match c {
            PaneContent::Empty => ("empty".into(), "Scegli contenuto".into(), None),
            PaneContent::App { app, .. } => match self.app_def(app) {
                Some(a) => ("app".into(), a.name.clone(), a.icon.clone()),
                None => ("app".into(), app.clone(), None),
            },
            PaneContent::Terminal { shell } => (
                "terminal".into(),
                self.config.shells.iter().find(|s| &s.id == shell).map(|s| s.name.clone()).unwrap_or_else(|| shell.clone()),
                None,
            ),
            PaneContent::Url { url } => {
                let host = config::origin_of(url).map(|o| o.split("://").nth(1).unwrap_or("").to_string()).unwrap_or_else(|| url.clone());
                ("url".into(), host, None)
            }
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
        if names.is_empty() { ("Nuova scheda".into(), None) } else { (names.join(" · "), icon) }
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
                let subtitle = if p.pty_exited { "terminato".to_string() } else { p.doc_title.clone() };
                json!({
                    "id": p.id, "index": i, "kind": kind, "title": title, "subtitle": subtitle,
                    "icon": icon, "rect": r, "focused": i == t.focused, "picking": p.picking,
                })
            })
            .collect();
        let tabs: Vec<Value> = self
            .tabs
            .iter()
            .map(|t| {
                let (title, icon) = self.tab_title(t);
                json!({"id": t.id, "title": title, "customTitle": t.title, "icon": icon, "layout": t.layout.id(), "count": t.visible().len()})
            })
            .collect();
        let state = json!({
            "version": env!("CARGO_PKG_VERSION"),
            "barHeight": BAR_H, "headerHeight": HEADER_H,
            "tabs": tabs, "active": self.active, "tabId": t.id,
            "layout": t.layout.id(),
            "layouts": layout::ALL.iter().map(|l| l.id()).collect::<Vec<_>>(),
            "panes": panes, "splitters": splitters,
            "apps": self.config.apps.iter().map(|a| json!({"id": a.id, "name": a.name, "icon": a.icon, "url": a.url, "permissions": a.permissions})).collect::<Vec<_>>(),
            "shells": self.config.shells.iter().map(|s| json!({"id": s.id, "name": s.name, "program": s.program})).collect::<Vec<_>>(),
            "devtools": self.config.devtools,
            "bridge": {
                "enabled": self.config.bridge.enabled, "port": self.config.bridge.port, "token": self.config.bridge.token,
                "status": self.bridge_status, "clients": self.clients.values().map(|c| c.name.clone()).collect::<Vec<_>>(),
            },
            "configPath": self.paths.config().to_string_lossy(),
            "configError": self.config_error,
        });
        let _ = shell.evaluate_script(&format!("window.ponteShell && ponteShell.update({state})"));
    }

    fn toast(&self, msg: &str) {
        if let Some(s) = &self.shell {
            let _ = s.evaluate_script(&format!("window.ponteShell && ponteShell.toast({})", json!(msg)));
        }
    }

    /* ---------------------------------------------------------- IPC shell */

    fn on_shell(&mut self, body: &str) {
        let Ok(m) = serde_json::from_str::<Value>(body) else { return };
        let cmd = m["cmd"].as_str().unwrap_or("");
        let idx = |k: &str| m[k].as_u64().map(|v| v as usize);
        let pane = m["pane"].as_u64().map(|v| v as u32);
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
                if let (Some(from), Some(to)) = (idx("from"), idx("to")) {
                    if from < self.tabs.len() && to < self.tabs.len() {
                        let active_id = self.tabs[self.active].id;
                        let t = self.tabs.remove(from);
                        self.tabs.insert(to, t);
                        self.active = self.tabs.iter().position(|t| t.id == active_id).unwrap_or(0);
                        self.push_state();
                        self.save_session();
                    }
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
                if let (Some((ta, pa)), Some((tb, pb))) = (a.and_then(|a| self.find_pane(a)), b.and_then(|b| self.find_pane(b))) {
                    if ta == tb {
                        self.tabs[ta].panes.swap(pa, pb);
                        self.tabs[ta].focused = pb;
                        self.recall_sizes(ta, false);
                        self.relayout();
                        self.push_state();
                        self.save_session();
                    }
                }
            }
            "reload_tab" => self.reload_tab(),
            "reload_pane" => {
                if let Some((ti, pi)) = pane.and_then(|p| self.find_pane(p)) {
                    let p = &self.tabs[ti].panes[pi];
                    if matches!(p.content, PaneContent::Terminal { .. }) {
                        // Riavvia il terminale.
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
            _ => log::debug!("comando shell sconosciuto: {cmd}"),
        }
    }

    fn close_pane(&mut self, pane: u32) {
        let Some((ti, pi)) = self.find_pane(pane) else { return };
        if self.tabs[ti].panes[pi].content == PaneContent::Empty {
            // Pannello già vuoto: riduce il tileset.
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

    fn shortcut(&mut self, m: &Value) {
        let n = self.tabs.len();
        match m["name"].as_str().unwrap_or("") {
            "reload_tab" => self.reload_tab(),
            "next_tab" => self.activate((self.active + 1) % n),
            "prev_tab" => self.activate((self.active + n - 1) % n),
            "new_tab" => self.open_tab(PaneContent::Empty),
            "focus_pane" => {
                let i = m["index"].as_u64().unwrap_or(0) as usize;
                self.set_focus(self.active, i, true);
            }
            _ => {}
        }
    }

    fn reload_config(&mut self) {
        let (cfg, err) = config::load_config(&self.paths);
        let port_changed = cfg.bridge.port != self.config.bridge.port || cfg.bridge.enabled != self.config.bridge.enabled;
        if err.is_none() {
            self.config = cfg;
        }
        self.config_error = err.clone();
        if let Some(e) = err {
            self.toast(&e);
        } else if port_changed {
            self.toast("Le modifiche al bridge WebSocket saranno attive al prossimo avvio.");
        }
        self.push_state();
    }

    /* ---------------------------------------------------------- IPC pannelli */

    fn send_to_pane(&self, pane: u32, msg: &Value) -> bool {
        match self.find_pane(pane).and_then(|(ti, pi)| self.tabs[ti].panes[pi].view.as_ref()) {
            Some(v) => v.evaluate_script(&format!("window.__ponte_recv && window.__ponte_recv({msg})")).is_ok(),
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

    /// Pannello che risponde alle chiamate per un'app (preferendo la scheda attiva).
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
        if matches!(target, None | Some("*") | Some("external")) && !source.starts_with("external:") {
            self.broadcast_ws(&json!({"type": "event", "source": source, "event": event, "data": data}));
        }
    }

    fn start_call(&mut self, caller: Caller, target: &str, method: &str, params: &Value, from: &str) -> Result<(), String> {
        let pane = self.call_target(target).ok_or_else(|| format!("applicativo '{target}' non aperto"))?;
        let id = self.next_call;
        self.next_call += 1;
        self.calls.insert(id, caller);
        self.send_to_pane(pane, &json!({"t": "call", "id": id, "method": method, "params": params, "from": from}));
        Ok(())
    }

    fn finish_call(&mut self, id: u64, result: Result<Value, String>) {
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
        match t {
            "focus" => {
                self.set_focus(ti, pi, false);
                return;
            }
            "shortcut" => {
                if m["name"] == "focus_pane" || ti == self.active {
                    self.shortcut(&m);
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
        let Some(app) = self.effective_app(&content) else { return };
        let app_id = app.id.clone();
        let source = if app_id.is_empty() { format!("pane:{pane}") } else { app_id.clone() };
        // Il bridge risponde solo alle origini dell'applicativo.
        if !app.owns_url(url) {
            if t == "invoke" {
                let id = m["id"].as_u64().unwrap_or(0);
                self.send_to_pane(pane, &ok_reply(id, Err(format!("origine non autorizzata per il bridge: {}", config::origin_of(url).unwrap_or_default()))));
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
                    log::warn!("{source}: emit senza permesso 'messaging'");
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
                    Err(m["error"].as_str().unwrap_or("errore").to_string())
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
                        return reply(self, Err(format!("applicativo sconosciuto: {a}")));
                    }
                    PaneContent::App { app: a.to_string(), url: args["url"].as_str().map(str::to_string) }
                } else if let Some(u) = args["url"].as_str() {
                    PaneContent::Url { url: u.to_string() }
                } else {
                    return reply(self, Err("specificare 'app' o 'url'".into()));
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
                let _ = self.host.window.set_focus();
                reply(self, Ok(Value::Null));
            }
            "ui.notify" => {
                self.toast(args["message"].as_str().unwrap_or(""));
                if !self.host.window.is_focused() {
                    self.host.window.request_user_attention(Some(tao::window::UserAttentionType::Informational));
                }
                reply(self, Ok(Value::Null));
            }
            "bridge.call" => {
                if !app.has("messaging") {
                    return reply(self, Err("permesso 'messaging' non concesso".into()));
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
                    return reply(self, Err("permesso 'messaging' non concesso".into()));
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
            _ => match locals::permission_for(cmd) {
                None => reply(self, Err(format!("comando sconosciuto: {cmd}"))),
                Some(perm) if !perm.is_empty() && !app.has(perm) => {
                    reply(self, Err(format!("permesso '{perm}' non concesso all'applicativo")));
                }
                Some(_) => {
                    let locals = self.locals.clone();
                    let proxy = self.proxy.clone();
                    let app = app.clone();
                    let cmd = cmd.to_string();
                    std::thread::spawn(move || {
                        let result = locals.execute(&app, pane, &cmd, &args);
                        let _ = proxy.send_event(UserEvent::TaskDone { pane, id, result });
                    });
                }
            },
        }
    }

    fn on_terminal(&mut self, ti: usize, pi: usize, shell_id: &str, m: &Value) {
        let cols = m["cols"].as_u64().unwrap_or(80).clamp(2, 1000) as u16;
        let rows = m["rows"].as_u64().unwrap_or(24).clamp(2, 1000) as u16;
        let pane_id = self.tabs[ti].panes[pi].id;
        match m["t"].as_str().unwrap_or("") {
            "pty_ready" | "pty_restart" => {
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
                    let msg = format!("\r\nshell '{shell_id}' non configurata\r\n");
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
                        let msg = format!("\r\nimpossibile avviare {}: {e}\r\n", shell.program);
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
                if let Some(text) = m["text"].as_str() {
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        let _ = cb.set_text(text.to_string());
                    }
                }
            }
            "paste" => {
                let text = arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok()).unwrap_or_default();
                if let Some(pty) = &mut self.tabs[ti].panes[pi].pty {
                    // Bracketed paste non garantito su tutte le shell: si invia il testo normalizzato.
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
                self.deliver_event(Some("*"), "bridge.client_connected", &json!({"id": client, "name": name}), "ponte", None);
                self.push_state();
            }
            WsEvent::Disconnected { client } => {
                if let Some(c) = self.clients.remove(&client) {
                    self.deliver_event(Some("*"), "bridge.client_disconnected", &json!({"id": client, "name": c.name}), "ponte", None);
                }
                self.calls.retain(|_, c| !matches!(c, Caller::Ws { client: cl, .. } if *cl == client));
                self.push_state();
            }
            WsEvent::Message { client, text } => {
                let reply = |s: &App, v: Value| {
                    if let Some(c) = s.clients.get(&client) {
                        let _ = c.tx.send(v.to_string());
                    }
                };
                let Ok(m) = serde_json::from_str::<Value>(&text) else {
                    return reply(self, json!({"type": "error", "error": "JSON non valido"}));
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
                                return reply(self, err(format!("applicativo sconosciuto: {a}")));
                            }
                            PaneContent::App { app: a.to_string(), url: m["url"].as_str().map(str::to_string) }
                        } else if let Some(u) = m["url"].as_str() {
                            PaneContent::Url { url: u.to_string() }
                        } else {
                            return reply(self, err("specificare 'app' o 'url'".into()));
                        };
                        self.open_tab(content);
                        let _ = self.host.window.set_focus();
                        reply(self, ok(Value::Null));
                    }
                    "focus" => {
                        let target = m["app"].as_str().unwrap_or("");
                        match self.call_target(target).and_then(|p| self.find_pane(p)) {
                            Some((ti, pi)) => {
                                self.activate(ti);
                                self.set_focus(ti, pi, true);
                                let _ = self.host.window.set_focus();
                                reply(self, ok(Value::Null));
                            }
                            None => reply(self, err(format!("applicativo '{target}' non aperto"))),
                        }
                    }
                    "reload" => {
                        self.reload_tab();
                        reply(self, ok(Value::Null));
                    }
                    other => reply(self, err(format!("tipo di messaggio sconosciuto: {other}"))),
                }
            }
        }
    }

    /* ---------------------------------------------------------- eventi */

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
                let msg = json!({"t": "event", "event": "serial.data", "source": "ponte",
                    "data": {"handle": handle, "base64": B64.encode(&data), "text": String::from_utf8_lossy(&data)}});
                self.send_to_pane(pane, &msg);
            }
            UserEvent::SerialClosed { pane, handle, error } => {
                let msg = json!({"t": "event", "event": "serial.closed", "source": "ponte", "data": {"handle": handle, "error": error}});
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
                log::debug!("pannello {pane} caricato: {url}");
            }
            UserEvent::NewWindow { pane, url } => {
                let Some((ti, pi)) = self.find_pane(pane) else { return };
                let content = self.tabs[ti].panes[pi].content.clone();
                match self.effective_app(&content) {
                    Some(app) if !app.id.is_empty() && app.owns_url(&url) => {
                        self.open_tab(PaneContent::App { app: app.id, url: Some(url) })
                    }
                    Some(app) if app.id.is_empty() || !app.external_links_in_browser => self.open_tab(PaneContent::Url { url }),
                    _ => {
                        let _ = open::that_detached(&url);
                    }
                }
            }
            UserEvent::Ws(e) => self.on_ws(e),
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    #[cfg(target_os = "linux")]
    {
        // WebKitGTK: il renderer DMABUF su alcune VM/driver produce pannelli neri.
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
        }
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("Ponte")
        .with_inner_size(LogicalSize::new(1400.0, 900.0))
        .with_min_inner_size(LogicalSize::new(640.0, 420.0))
        .build(&event_loop)
        .expect("creazione finestra");
    let host = Host::new(window);
    let mut app = App::new(host, proxy);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    app.relayout();
                    app.push_state();
                }
                WindowEvent::CloseRequested => {
                    app.save_session();
                    *control_flow = ControlFlow::Exit;
                }
                _ => {}
            },
            Event::UserEvent(e) => app.handle(e),
            _ => {}
        }
    });
}
