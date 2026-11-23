//! Command line, `brust://` links, single instance and link-scheme registration.
//!
//! ```text
//! brust --open ordo                 open an app in a new tab
//! brust --open ordo --url <url>     open a page of the app
//! brust --url https://example.com   open a web page
//! brust --template "Testing"        open a saved tileset
//! brust --notes [notebook]          open the notes
//! brust brust://open?app=ordo       same, as a link (also: brust://url?u=…, brust://template?name=…, brust://notes?notebook=…)
//! ```

use std::io::ErrorKind;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum Launch {
    App { app: String, url: Option<String> },
    Url(String),
    Template(String),
    Notes(Option<String>),
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query(link: &str, key: &str) -> Option<String> {
    let q = link.split_once('?')?.1;
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == key).then(|| percent_decode(v))
    })
}

/// Parses a `brust://` link.
pub fn parse_link(link: &str) -> Option<Launch> {
    let rest = link.strip_prefix("brust://").or_else(|| link.strip_prefix("brust:"))?;
    let action = rest.split(['?', '/']).next().unwrap_or("").to_ascii_lowercase();
    let nonempty = |v: Option<String>| v.filter(|s| !s.is_empty());
    match action.as_str() {
        "open" | "app" => nonempty(query(link, "app")).map(|app| Launch::App { app, url: nonempty(query(link, "url")) }),
        "url" => nonempty(query(link, "u").or_else(|| query(link, "url")))
            .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
            .map(Launch::Url),
        "template" => nonempty(query(link, "name")).map(Launch::Template),
        "notes" => Some(Launch::Notes(nonempty(query(link, "notebook")))),
        _ => None,
    }
}

/// Parses the command line (unknown arguments are ignored).
pub fn parse(args: &[String]) -> Vec<Launch> {
    let mut out = Vec::new();
    let mut i = 0;
    let next = |i: usize| args.get(i + 1).filter(|a| !a.starts_with("--")).cloned();
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--open" | "--app" => {
                if let Some(app) = next(i) {
                    i += 1;
                    let url = if args.get(i + 1).map(String::as_str) == Some("--url") { next(i + 1) } else { None };
                    if url.is_some() {
                        i += 2;
                    }
                    out.push(Launch::App { app, url });
                }
            }
            "--url" => {
                if let Some(u) = next(i) {
                    i += 1;
                    out.push(Launch::Url(u));
                }
            }
            "--template" => {
                if let Some(t) = next(i) {
                    i += 1;
                    out.push(Launch::Template(t));
                }
            }
            "--notes" => {
                let nb = next(i);
                if nb.is_some() {
                    i += 1;
                }
                out.push(Launch::Notes(nb));
            }
            _ if a.starts_with("brust:") => out.extend(parse_link(a)),
            _ => {}
        }
        i += 1;
    }
    out
}

/// If another Brust is running (its bridge answers with our token), sends it the command line
/// and returns true. Returns false when no instance is running or the bridge is disabled.
pub fn forward_to_running_instance(args: &[String]) -> bool {
    use tungstenite::client::IntoClientRequest;
    let paths = crate::config::Paths::new();
    let remote = crate::config::load_json::<serde_json::Value>(&paths.remote_cache());
    let (cfg, _) = crate::config::load_config(&paths, remote.as_ref());
    if !cfg.bridge.enabled {
        return false;
    }
    let token = if cfg.bridge.token.is_empty() {
        match crate::secrets::load_secret(&paths.token_secret()) {
            Some(t) => t,
            None => return false,
        }
    } else {
        cfg.bridge.token.clone()
    };
    let addr: SocketAddr = ([127, 0, 0, 1], cfg.bridge.port).into();
    let stream = match TcpStream::connect_timeout(&addr, Duration::from_millis(400)) {
        Ok(s) => s,
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => return false,
        Err(_) => return false,
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let Ok(req) = format!("ws://127.0.0.1:{}/?token={}&name=brust-launcher", cfg.bridge.port, token).into_client_request() else {
        return false;
    };
    let Ok((mut ws, _)) = tungstenite::client::client(req, stream) else { return false };
    let msg = serde_json::json!({"type": "cli", "id": 1, "args": args});
    if ws.send(tungstenite::Message::text(msg.to_string())).is_err() {
        return false;
    }
    // Wait for the acknowledgement (the welcome message comes first).
    for _ in 0..4 {
        match ws.read() {
            Ok(tungstenite::Message::Text(t)) if t.contains("\"result\"") => break,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    let _ = ws.close(None);
    true
}

/// Registers `brust://` for the current user (Windows registry, HKCU). No-op elsewhere.
pub fn register_protocol() {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let Ok(exe) = std::env::current_exe() else { return };
        let command = format!("\"{}\" \"%1\"", exe.display());
        let base = r"HKCU\Software\Classes\brust";
        let steps: [Vec<&str>; 3] = [
            vec!["add", base, "/ve", "/d", "URL:Brust", "/f"],
            vec!["add", base, "/v", "URL Protocol", "/d", "", "/f"],
            vec!["add", r"HKCU\Software\Classes\brust\shell\open\command", "/ve", "/d", &command, "/f"],
        ];
        for args in steps {
            let r = std::process::Command::new("reg").args(&args).creation_flags(CREATE_NO_WINDOW).output();
            if let Err(e) = r {
                log::warn!("registering brust://: {e}");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn args() {
        assert_eq!(parse(&v(&["--open", "ordo"])), vec![Launch::App { app: "ordo".into(), url: None }]);
        assert_eq!(
            parse(&v(&["--open", "ordo", "--url", "http://x/y"])),
            vec![Launch::App { app: "ordo".into(), url: Some("http://x/y".into()) }]
        );
        assert_eq!(parse(&v(&["--notes"])), vec![Launch::Notes(None)]);
        assert_eq!(parse(&v(&["--template", "Testing", "--url", "https://a.b"])), vec![
            Launch::Template("Testing".into()),
            Launch::Url("https://a.b".into())
        ]);
        assert!(parse(&v(&["--weird", "--open"])).is_empty());
    }

    #[test]
    fn links() {
        assert_eq!(parse_link("brust://open?app=ordo"), Some(Launch::App { app: "ordo".into(), url: None }));
        assert_eq!(
            parse_link("brust://open/?app=ordo&url=http%3A%2F%2Fh%2Fp%3Fq%3D1"),
            Some(Launch::App { app: "ordo".into(), url: Some("http://h/p?q=1".into()) })
        );
        assert_eq!(parse_link("brust://template?name=Line+1"), Some(Launch::Template("Line 1".into())));
        assert_eq!(parse_link("brust://url?u=javascript:alert(1)"), None);
        assert_eq!(parse_link("brust://notes?notebook=Inbox"), Some(Launch::Notes(Some("Inbox".into()))));
        assert_eq!(parse_link("brust://nope"), None);
    }
}
