//! Internal resources served via the custom `brust://` protocol.

use std::borrow::Cow;
use wry::http::{header::CONTENT_TYPE, Request, Response};

pub const SCHEME: &str = "brust";
pub const BRIDGE_JS: &str = include_str!("../assets/bridge.js");

fn lookup(path: &str) -> Option<(&'static [u8], &'static str)> {
    Some(match path {
        "shell.html" => (include_bytes!("../assets/shell.html"), "text/html; charset=utf-8"),
        "terminal.html" => (include_bytes!("../assets/terminal.html"), "text/html; charset=utf-8"),
        "vendor/xterm.js" => (include_bytes!("../assets/vendor/xterm.js"), "text/javascript; charset=utf-8"),
        "vendor/xterm.css" => (include_bytes!("../assets/vendor/xterm.css"), "text/css; charset=utf-8"),
        "vendor/addon-fit.js" => (include_bytes!("../assets/vendor/addon-fit.js"), "text/javascript; charset=utf-8"),
        _ => return None,
    })
}

/// URL of an internal resource (on Windows WebView2 uses `http://brust.localhost/`).
pub fn url(path: &str) -> String {
    if cfg!(windows) {
        format!("http://{SCHEME}.localhost/{path}")
    } else {
        format!("{SCHEME}://localhost/{path}")
    }
}

pub fn is_internal(url: &str) -> bool {
    url.starts_with(&format!("{SCHEME}://")) || url.starts_with(&format!("http://{SCHEME}.localhost/"))
}

pub fn handle(_id: wry::WebViewId, req: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let path = req.uri().path().trim_start_matches('/');
    match lookup(path) {
        Some((body, mime)) => Response::builder()
            .header(CONTENT_TYPE, mime)
            .header("Cache-Control", "no-cache")
            .body(Cow::Borrowed(body))
            .unwrap(),
        None => Response::builder().status(404).body(Cow::Borrowed(&b"not found"[..])).unwrap(),
    }
}
