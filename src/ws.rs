//! Local WebSocket server (127.0.0.1) that lets external software
//! talk to the apps open in the client.

use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::Duration;
use tao::event_loop::EventLoopProxy;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::{Error, Message};

use crate::UserEvent;

pub enum WsEvent {
    Connected { client: u32, name: String, tx: Sender<String> },
    Message { client: u32, text: String },
    Disconnected { client: u32 },
}

pub fn start(port: u16, token: String, proxy: EventLoopProxy<UserEvent>) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("port {port}: {e}"))?;
    std::thread::spawn(move || {
        let mut next_id = 1u32;
        for stream in listener.incoming().flatten() {
            let id = next_id;
            next_id += 1;
            let token = token.clone();
            let proxy = proxy.clone();
            std::thread::spawn(move || handle(stream, id, token, proxy));
        }
    });
    Ok(())
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

fn handle(stream: TcpStream, client: u32, token: String, proxy: EventLoopProxy<UserEvent>) {
    let mut name = format!("client-{client}");
    let callback = |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        let query = req.uri().query().unwrap_or("");
        let header_token = req
            .headers()
            .get("x-brust-token")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let given = query_param(query, "token").or(header_token).unwrap_or_default();
        if given != token {
            let mut r = ErrorResponse::new(Some("invalid token".into()));
            *r.status_mut() = tungstenite::http::StatusCode::UNAUTHORIZED;
            return Err(r);
        }
        if let Some(n) = query_param(query, "name") {
            name = n;
        }
        Ok(resp)
    };
    let mut ws = match tungstenite::accept_hdr(stream, callback) {
        Ok(ws) => ws,
        Err(e) => {
            log::warn!("WS handshake rejected: {e}");
            return;
        }
    };
    let _ = ws.get_ref().set_read_timeout(Some(Duration::from_millis(30)));
    let (tx, rx): (Sender<String>, Receiver<String>) = mpsc::channel();
    if proxy.send_event(UserEvent::Ws(WsEvent::Connected { client, name, tx })).is_err() {
        return;
    }
    'outer: loop {
        match ws.read() {
            Ok(Message::Text(t)) => {
                let _ = proxy.send_event(UserEvent::Ws(WsEvent::Message { client, text: t.to_string() }));
            }
            Ok(Message::Binary(b)) => {
                if let Ok(t) = String::from_utf8(b.to_vec()) {
                    let _ = proxy.send_event(UserEvent::Ws(WsEvent::Message { client, text: t }));
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
        loop {
            match rx.try_recv() {
                Ok(msg) => {
                    if ws.send(Message::text(msg)).is_err() {
                        break 'outer;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'outer,
            }
        }
    }
    let _ = proxy.send_event(UserEvent::Ws(WsEvent::Disconnected { client }));
}
