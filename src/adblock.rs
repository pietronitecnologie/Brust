//! Local filtering HTTP proxy used to block trackers, ads and analytics
//! for selected webviews.
//!
//! The proxy listens on `127.0.0.1:<ephemeral port>` and understands:
//! - `CONNECT host:port` tunnels (HTTPS, WSS): blocked hosts get `403`,
//!   others are tunnelled byte-for-byte.
//! - Absolute-form plain HTTP requests (`GET http://host/path`): blocked
//!   hosts get an empty `204`, others are forwarded in origin-form with
//!   `Connection: close`.
//!
//! Only the standard library is used; one thread is spawned per connection.

use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

/// Maximum size of a request head (request line + headers).
const MAX_HEAD: usize = 64 * 1024;
/// Timeout for reading the request head from the client.
const HEAD_TIMEOUT: Duration = Duration::from_secs(15);
/// Timeout for connecting to the upstream server.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Built-in list of well-known tracker / ad / analytics domains.
/// Every entry also blocks all of its subdomains.
const BUILTIN_BLOCKLIST: &[&str] = &[
    // Google ads & analytics
    "google-analytics.com",
    "googletagmanager.com",
    "googletagservices.com",
    "doubleclick.net",
    "googlesyndication.com",
    "googleadservices.com",
    "adservice.google.com",
    "pagead2.googlesyndication.com",
    "2mdn.net",
    "app-measurement.com",
    "googleoptimize.com",
    "analytics.google.com",
    "stats.g.doubleclick.net",
    // Meta / Facebook
    "facebook.net",
    "connect.facebook.net",
    "pixel.facebook.com",
    "an.facebook.com",
    // Microsoft / LinkedIn / Twitter / TikTok / Pinterest / Snap / Reddit
    "clarity.ms",
    "bat.bing.com",
    "ads.linkedin.com",
    "px.ads.linkedin.com",
    "snap.licdn.com",
    "analytics.twitter.com",
    "ads-twitter.com",
    "static.ads-twitter.com",
    "ads-api.twitter.com",
    "analytics.tiktok.com",
    "ads.tiktok.com",
    "ct.pinterest.com",
    "analytics.pinterest.com",
    "sc-static.net",
    "tr.snapchat.com",
    "events.redditmedia.com",
    "alb.reddit.com",
    // Yandex / Baidu
    "mc.yandex.ru",
    "mc.yandex.com",
    "an.yandex.ru",
    "hm.baidu.com",
    // Product analytics / session recording
    "hotjar.com",
    "hotjar.io",
    "mixpanel.com",
    "mxpnl.com",
    "segment.io",
    "segment.com",
    "cdn.segment.com",
    "api.segment.io",
    "amplitude.com",
    "heap.io",
    "heapanalytics.com",
    "fullstory.com",
    "mouseflow.com",
    "crazyegg.com",
    "luckyorange.com",
    "luckyorange.net",
    "inspectlet.com",
    "logrocket.com",
    "lr-ingest.io",
    "smartlook.com",
    "quantummetric.com",
    "contentsquare.net",
    "clicktale.net",
    "sessioncam.com",
    "kissmetrics.com",
    "kissmetrics.io",
    "woopra.com",
    "pendo.io",
    "posthog.com",
    "statsig.com",
    "optimizely.com",
    "vwo.com",
    "visualwebsiteoptimizer.com",
    "abtasty.com",
    "kameleoon.com",
    // Web analytics
    "chartbeat.com",
    "chartbeat.net",
    "scorecardresearch.com",
    "quantserve.com",
    "quantcount.com",
    "comscore.com",
    "statcounter.com",
    "newrelic.com",
    "nr-data.net",
    "omtrdc.net",
    "2o7.net",
    "demdex.net",
    "everesttech.net",
    "matomo.cloud",
    "parsely.com",
    "parse.ly",
    "gaug.es",
    "clicky.com",
    "getclicky.com",
    "histats.com",
    "openstat.net",
    "webtrends.com",
    "webtrendslive.com",
    "branch.io",
    "app.link",
    "adjust.com",
    "appsflyer.com",
    "kochava.com",
    "singular.net",
    "bugsnag.com",
    "sentry-cdn.com",
    // Ad networks / exchanges / DSPs
    "adnxs.com",
    "adsrvr.org",
    "criteo.com",
    "criteo.net",
    "taboola.com",
    "outbrain.com",
    "rubiconproject.com",
    "pubmatic.com",
    "openx.net",
    "casalemedia.com",
    "indexww.com",
    "smartadserver.com",
    "adform.net",
    "advertising.com",
    "amazon-adsystem.com",
    "media.net",
    "moatads.com",
    "adsafeprotected.com",
    "doubleverify.com",
    "serving-sys.com",
    "flashtalking.com",
    "teads.tv",
    "yieldmo.com",
    "sharethrough.com",
    "33across.com",
    "contextweb.com",
    "lijit.com",
    "sovrn.com",
    "bidswitch.net",
    "mathtag.com",
    "turn.com",
    "bluekai.com",
    "krxd.net",
    "exelator.com",
    "agkn.com",
    "rlcdn.com",
    "tapad.com",
    "crwdcntrl.net",
    "bounceexchange.com",
    "zemanta.com",
    "revcontent.com",
    "mgid.com",
    "propellerads.com",
    "popads.net",
    "popcash.net",
    "adroll.com",
    "quantcast.com",
    "yieldlab.net",
    "adition.com",
    "imrworldwide.com",
    "ib-ibi.com",
    "spotxchange.com",
    "springserve.com",
    "lkqd.net",
    "innovid.com",
    "zedo.com",
    "infolinks.com",
    "buysellads.com",
    "carbonads.com",
    "carbonads.net",
    "adzerk.net",
    "pixel.wp.com",
    "stats.wp.com",
    // Marketing automation / CRM trackers
    "hs-analytics.net",
    "hs-scripts.com",
    "hsadspixel.net",
    "hubspot.net",
    "marketo.net",
    "mktoresp.com",
    "pardot.com",
    "trk.klclick.com",
    "intercom-clicks.com",
    "ml314.com",
    "licdn-ads.com",
];

/// A set of blocked domains with an allow-list that overrides it.
pub struct Blocker {
    blocked: HashSet<String>,
    allowed: HashSet<String>,
}

impl Blocker {
    /// Built-in list plus `extra_block` domains, minus `allow` domains
    /// (an allowed domain also allows all of its subdomains).
    pub fn new(extra_block: &[String], allow: &[String]) -> Self {
        let mut blocked: HashSet<String> = BUILTIN_BLOCKLIST
            .iter()
            .map(|d| d.to_string())
            .collect();
        blocked.extend(extra_block.iter().filter_map(|d| normalize_host(d)));
        let allowed = allow.iter().filter_map(|d| normalize_host(d)).collect();
        Blocker { blocked, allowed }
    }

    /// Load additional domains from text in hosts format (`0.0.0.0 domain`,
    /// several domains per line allowed) or one domain per line.
    /// `#` starts a comment. Adblock-style `||domain^` entries are accepted too.
    pub fn load_list(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() || line.starts_with('!') || line.starts_with('[') {
                continue;
            }
            let mut tokens = line.split_whitespace();
            let Some(first) = tokens.next() else { continue };
            let first_is_ip = first.parse::<std::net::IpAddr>().is_ok();
            let candidates: Vec<&str> = if first_is_ip {
                tokens.collect()
            } else {
                vec![first]
            };
            for raw in candidates {
                let raw = raw.trim_start_matches("||").trim_end_matches('^');
                let Some(domain) = normalize_host(raw) else { continue };
                if matches!(
                    domain.as_str(),
                    "localhost"
                        | "localhost.localdomain"
                        | "local"
                        | "broadcasthost"
                        | "ip6-localhost"
                        | "ip6-loopback"
                ) || domain.parse::<std::net::IpAddr>().is_ok()
                    || !domain.contains('.')
                {
                    continue;
                }
                self.blocked.insert(domain);
            }
        }
    }

    /// True if `host` or any parent domain is blocked and neither `host` nor
    /// any parent domain is allowed. Case-insensitive; a port and a trailing
    /// dot are ignored.
    pub fn is_blocked(&self, host: &str) -> bool {
        let Some(host) = normalize_host(host) else {
            return false;
        };
        if host.parse::<std::net::IpAddr>().is_ok() {
            return self.blocked.contains(&host) && !self.allowed.contains(&host);
        }
        let suffixes: Vec<&str> = domain_suffixes(&host).collect();
        if suffixes.iter().any(|s| self.allowed.contains(*s)) {
            return false;
        }
        suffixes.iter().any(|s| self.blocked.contains(*s))
    }

    /// Number of blocked domain entries.
    pub fn len(&self) -> usize {
        self.blocked.len()
    }
}

/// Iterate over `a.b.c`, `b.c`, `c`.
fn domain_suffixes(host: &str) -> impl Iterator<Item = &str> {
    std::iter::once(host).chain(
        host.char_indices()
            .filter(|&(_, c)| c == '.')
            .map(move |(i, _)| &host[i + 1..])
            .filter(|s| !s.is_empty()),
    )
}

/// Lowercase, strip port, brackets and trailing dots. Returns `None` if empty.
fn normalize_host(raw: &str) -> Option<String> {
    let s = raw.trim();
    let s = if let Some(rest) = s.strip_prefix('[') {
        // [ipv6]:port or [ipv6]
        rest.split(']').next().unwrap_or("")
    } else if s.matches(':').count() == 1 {
        s.split(':').next().unwrap_or("")
    } else {
        s
    };
    let s = s.trim_end_matches('.').trim_start_matches('.').to_ascii_lowercase();
    if s.is_empty() { None } else { Some(s) }
}

/// Handle to a running proxy. The proxy runs for the lifetime of the process.
pub struct ProxyHandle {
    pub port: u16,
    blocked: Arc<AtomicU64>,
    allowed: Arc<AtomicU64>,
}

impl ProxyHandle {
    /// Number of requests/tunnels refused so far.
    pub fn blocked_count(&self) -> u64 {
        self.blocked.load(Ordering::Relaxed)
    }

    /// Number of requests/tunnels forwarded so far.
    pub fn allowed_count(&self) -> u64 {
        self.allowed.load(Ordering::Relaxed)
    }
}

struct Shared {
    blocker: Blocker,
    blocked: Arc<AtomicU64>,
    allowed: Arc<AtomicU64>,
}

/// Bind `127.0.0.1:0`, spawn the accept thread and return the handle.
pub fn start(blocker: Blocker) -> Result<ProxyHandle, String> {
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("adblock proxy bind: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("adblock proxy local_addr: {e}"))?
        .port();
    let blocked = Arc::new(AtomicU64::new(0));
    let allowed = Arc::new(AtomicU64::new(0));
    let shared = Arc::new(Shared {
        blocker,
        blocked: blocked.clone(),
        allowed: allowed.clone(),
    });
    thread::Builder::new()
        .name("adblock-accept".into())
        .spawn(move || accept_loop(listener, shared))
        .map_err(|e| format!("adblock proxy spawn: {e}"))?;
    Ok(ProxyHandle {
        port,
        blocked,
        allowed,
    })
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let shared = shared.clone();
                let res = thread::Builder::new()
                    .name("adblock-conn".into())
                    .spawn(move || {
                        if let Err(e) = handle_conn(stream, &shared) {
                            log::debug!("adblock proxy connection: {e}");
                        }
                    });
                if let Err(e) = res {
                    log::warn!("adblock proxy: cannot spawn connection thread: {e}");
                }
            }
            Err(e) => {
                log::debug!("adblock proxy accept: {e}");
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Read until the end of the head (`\r\n\r\n`). Returns (head_len, buffer);
/// the buffer may contain bytes past the head (e.g. a request body).
fn read_head(stream: &mut TcpStream) -> io::Result<(usize, Vec<u8>)> {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "client closed before end of request head",
            ));
        }
        let search_from = buf.len().saturating_sub(3);
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_subslice(&buf[search_from..], b"\r\n\r\n") {
            return Ok((search_from + pos + 4, buf));
        }
        if buf.len() > MAX_HEAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request head too large",
            ));
        }
    }
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

const RESP_400: &[u8] = b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const RESP_403: &[u8] = b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n";
const RESP_204: &[u8] = b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const RESP_502: &[u8] = b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

fn reply_and_close(stream: &mut TcpStream, resp: &[u8]) {
    let _ = stream.write_all(resp);
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
}

fn handle_conn(mut client: TcpStream, shared: &Shared) -> io::Result<()> {
    client.set_read_timeout(Some(HEAD_TIMEOUT))?;
    let _ = client.set_nodelay(true);
    let (head_len, buf) = match read_head(&mut client) {
        Ok(v) => v,
        Err(e) => {
            if e.kind() == io::ErrorKind::InvalidData {
                reply_and_close(&mut client, RESP_400);
            }
            return Err(e);
        }
    };
    let Ok(head) = std::str::from_utf8(&buf[..head_len]) else {
        reply_and_close(&mut client, RESP_400);
        return Ok(());
    };
    let rest = &buf[head_len..];
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        reply_and_close(&mut client, RESP_400);
        return Ok(());
    };
    if method.is_empty() || !version.starts_with("HTTP/") {
        reply_and_close(&mut client, RESP_400);
        return Ok(());
    }

    if method.eq_ignore_ascii_case("CONNECT") {
        let Some((host, port)) = split_host_port(target, None) else {
            reply_and_close(&mut client, RESP_400);
            return Ok(());
        };
        if shared.blocker.is_blocked(&host) {
            shared.blocked.fetch_add(1, Ordering::Relaxed);
            reply_and_close(&mut client, RESP_403);
            return Ok(());
        }
        shared.allowed.fetch_add(1, Ordering::Relaxed);
        let mut upstream = match connect_upstream(&host, port) {
            Ok(s) => s,
            Err(e) => {
                reply_and_close(&mut client, RESP_502);
                return Err(e);
            }
        };
        client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
        if !rest.is_empty() {
            upstream.write_all(rest)?;
        }
        client.set_read_timeout(None)?;
        return pipe(client, upstream);
    }

    // Plain HTTP: absolute-form target required.
    let Some((host, port, path)) = parse_absolute_http(target) else {
        reply_and_close(&mut client, RESP_400);
        return Ok(());
    };
    if shared.blocker.is_blocked(&host) {
        shared.blocked.fetch_add(1, Ordering::Relaxed);
        reply_and_close(&mut client, RESP_204);
        return Ok(());
    }
    shared.allowed.fetch_add(1, Ordering::Relaxed);

    let mut out = format!("{method} {path} {version}\r\n");
    let mut has_host = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let name = line.split(':').next().unwrap_or("").trim();
        if name.eq_ignore_ascii_case("proxy-connection")
            || name.eq_ignore_ascii_case("connection")
            || name.eq_ignore_ascii_case("keep-alive")
            || name.eq_ignore_ascii_case("proxy-authorization")
        {
            continue;
        }
        if name.eq_ignore_ascii_case("host") {
            has_host = true;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    if !has_host {
        if port == 80 {
            out.push_str(&format!("Host: {}\r\n", host_for_header(&host)));
        } else {
            out.push_str(&format!("Host: {}:{port}\r\n", host_for_header(&host)));
        }
    }
    out.push_str("Connection: close\r\n\r\n");

    let mut upstream = match connect_upstream(&host, port) {
        Ok(s) => s,
        Err(e) => {
            reply_and_close(&mut client, RESP_502);
            return Err(e);
        }
    };
    upstream.write_all(out.as_bytes())?;
    if !rest.is_empty() {
        upstream.write_all(rest)?;
    }
    client.set_read_timeout(None)?;
    pipe(client, upstream)
}

fn host_for_header(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

/// Split `host:port` / `[v6]:port`. Uses `default_port` if no port is given.
fn split_host_port(s: &str, default_port: Option<u16>) -> Option<(String, u16)> {
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (h, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if after.is_empty() => default_port?,
            None => return None,
        };
        (h.to_string(), port)
    } else {
        match s.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => (h.to_string(), p.parse().ok()?),
            Some(_) => return None,
            None => (s.to_string(), default_port?),
        }
    };
    if host.is_empty() || port == 0 {
        return None;
    }
    Some((host, port))
}

/// Parse `http://host[:port]/path?query` into (host, port, origin-form path).
fn parse_absolute_http(target: &str) -> Option<(String, u16, String)> {
    if target.len() < 7 || !target[..7].eq_ignore_ascii_case("http://") {
        return None;
    }
    let rest = &target[7..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let mut path = rest[end..].split('#').next().unwrap_or("").to_string();
    if path.is_empty() {
        path = "/".into();
    } else if path.starts_with('?') {
        path.insert(0, '/');
    }
    let (host, port) = split_host_port(authority, Some(80))?;
    Some((host, port, path))
}

fn connect_upstream(host: &str, port: u16) -> io::Result<TcpStream> {
    let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs()?.collect();
    let mut last_err = io::Error::new(io::ErrorKind::NotFound, "no address for host");
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

/// Copy bytes both ways until either side closes. When the client stops
/// sending, the upstream write side is half-closed; when the upstream side
/// finishes, both sockets are shut down.
fn pipe(client: TcpStream, upstream: TcpStream) -> io::Result<()> {
    let mut client_r = client.try_clone()?;
    let mut upstream_w = upstream.try_clone()?;
    let up_thread = thread::Builder::new()
        .name("adblock-pipe".into())
        .spawn(move || {
            match io::copy(&mut client_r, &mut upstream_w) {
                Ok(_) => {
                    let _ = upstream_w.shutdown(Shutdown::Write);
                }
                Err(_) => {
                    let _ = upstream_w.shutdown(Shutdown::Both);
                    let _ = client_r.shutdown(Shutdown::Both);
                }
            }
        });

    let mut upstream_r = upstream;
    let mut client_w = client;
    let _ = io::copy(&mut upstream_r, &mut client_w);
    let _ = client_w.flush();
    let _ = client_w.shutdown(Shutdown::Both);
    let _ = upstream_r.shutdown(Shutdown::Both);
    if let Ok(handle) = up_thread {
        let _ = handle.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn adblock_builtin_and_subdomains() {
        let b = Blocker::new(&[], &[]);
        assert!(b.len() >= 150, "builtin list has {} entries", b.len());
        assert!(b.is_blocked("google-analytics.com"));
        assert!(b.is_blocked("www.google-analytics.com"));
        assert!(b.is_blocked("a.b.c.doubleclick.net"));
        assert!(!b.is_blocked("notdoubleclick.net"));
        assert!(!b.is_blocked("example.com"));
        assert!(!b.is_blocked(""));
    }

    #[test]
    fn adblock_case_port_trailing_dot() {
        let b = Blocker::new(&s(&["Blocked.Test"]), &[]);
        assert!(b.is_blocked("BLOCKED.test"));
        assert!(b.is_blocked("sub.blocked.test:443"));
        assert!(b.is_blocked("blocked.test."));
        assert!(b.is_blocked("Sub.Blocked.Test.:8080"));
        assert!(!b.is_blocked("[::1]:443"));
    }

    #[test]
    fn adblock_allow_overrides() {
        let b = Blocker::new(&s(&["tracker.test"]), &s(&["ok.tracker.test", "hotjar.com"]));
        assert!(b.is_blocked("tracker.test"));
        assert!(b.is_blocked("bad.tracker.test"));
        assert!(!b.is_blocked("ok.tracker.test"));
        assert!(!b.is_blocked("deep.ok.tracker.test"));
        assert!(!b.is_blocked("static.hotjar.com"));
    }

    #[test]
    fn adblock_hosts_file_parsing() {
        let mut b = Blocker::new(&[], &s(&["allowed.example"]));
        let before = b.len();
        b.load_list(
            "# comment line\n\
             127.0.0.1 localhost\n\
             0.0.0.0 ads.example.org   # trailing comment\n\
             0.0.0.0 one.example two.example\n\
             ::1 ip6-localhost\n\
             plain.example\n\
             ||adblock.example^\n\
             \n\
             0.0.0.0 allowed.example\n",
        );
        assert!(b.is_blocked("ads.example.org"));
        assert!(b.is_blocked("x.ads.example.org"));
        assert!(b.is_blocked("one.example"));
        assert!(b.is_blocked("two.example"));
        assert!(b.is_blocked("plain.example"));
        assert!(b.is_blocked("adblock.example"));
        assert!(!b.is_blocked("allowed.example"));
        assert!(!b.is_blocked("localhost"));
        assert!(!b.is_blocked("example.org"));
        assert_eq!(b.len(), before + 6);
    }

    #[test]
    fn adblock_parse_targets() {
        assert_eq!(
            parse_absolute_http("http://Example.com:8080/a?b=1"),
            Some(("Example.com".into(), 8080, "/a?b=1".into()))
        );
        assert_eq!(
            parse_absolute_http("http://example.com"),
            Some(("example.com".into(), 80, "/".into()))
        );
        assert_eq!(parse_absolute_http("/relative"), None);
        assert_eq!(split_host_port("[::1]:443", None), Some(("::1".into(), 443)));
        assert_eq!(split_host_port("host", None), None);
    }

    /// Tiny server: answers an HTTP request with a fixed body (echoing the
    /// request line), or echoes raw bytes if the first bytes are not HTTP.
    fn spawn_test_server() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                thread::spawn(move || {
                    let mut first = [0u8; 4];
                    if c.read_exact(&mut first).is_err() {
                        return;
                    }
                    if &first == b"GET " {
                        let mut r = io::BufReader::new(c.try_clone().unwrap());
                        let mut line = String::new();
                        r.read_line(&mut line).unwrap();
                        let mut headers = String::new();
                        loop {
                            let mut h = String::new();
                            if r.read_line(&mut h).unwrap() == 0 || h == "\r\n" {
                                break;
                            }
                            headers.push_str(&h);
                        }
                        let body = format!("GET {}|{}", line.trim_end(), headers);
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = c.write_all(resp.as_bytes());
                    } else {
                        let _ = c.write_all(&first);
                        let mut buf = [0u8; 1024];
                        while let Ok(n) = c.read(&mut buf) {
                            if n == 0 || c.write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                    }
                });
            }
        });
        port
    }

    fn read_all(mut s: TcpStream) -> String {
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn adblock_proxy_integration() {
        let server_port = spawn_test_server();
        let handle = start(Blocker::new(&s(&["blocked.test"]), &[])).unwrap();
        let proxy = ("127.0.0.1", handle.port);

        // CONNECT tunnel to the local echo server.
        let mut c = TcpStream::connect(proxy).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(
            c,
            "CONNECT 127.0.0.1:{server_port} HTTP/1.1\r\nHost: 127.0.0.1:{server_port}\r\n\r\n"
        )
        .unwrap();
        let mut resp = [0u8; 39];
        c.read_exact(&mut resp).unwrap();
        assert_eq!(&resp[..], b"HTTP/1.1 200 Connection Established\r\n\r\n");
        c.write_all(b"ping-pong").unwrap();
        let mut echo = [0u8; 9];
        c.read_exact(&mut echo).unwrap();
        assert_eq!(&echo, b"ping-pong");
        drop(c);

        // Plain HTTP forwarding.
        let mut c = TcpStream::connect(proxy).unwrap();
        write!(
            c,
            "GET http://127.0.0.1:{server_port}/hello?x=1 HTTP/1.1\r\n\
             Host: 127.0.0.1:{server_port}\r\nProxy-Connection: keep-alive\r\n\
             Connection: keep-alive\r\nX-Test: yes\r\n\r\n"
        )
        .unwrap();
        let text = read_all(c);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
        assert!(text.contains("GET /hello?x=1 HTTP/1.1"), "{text}");
        assert!(text.contains("X-Test: yes"), "{text}");
        assert!(text.contains("Connection: close"), "{text}");
        assert!(!text.contains("keep-alive"), "{text}");

        // Blocked CONNECT.
        let mut c = TcpStream::connect(proxy).unwrap();
        c.write_all(b"CONNECT ads.blocked.test:443 HTTP/1.1\r\n\r\n").unwrap();
        assert!(read_all(c).starts_with("HTTP/1.1 403 Forbidden"));

        // Blocked plain HTTP.
        let mut c = TcpStream::connect(proxy).unwrap();
        c.write_all(b"GET http://blocked.test/pixel.gif HTTP/1.1\r\nHost: blocked.test\r\n\r\n")
            .unwrap();
        assert!(read_all(c).starts_with("HTTP/1.1 204 No Content"));

        // Malformed request.
        let mut c = TcpStream::connect(proxy).unwrap();
        c.write_all(b"garbage\r\n\r\n").unwrap();
        assert!(read_all(c).starts_with("HTTP/1.1 400"));

        assert_eq!(handle.blocked_count(), 2);
        assert_eq!(handle.allowed_count(), 2);
    }
}
