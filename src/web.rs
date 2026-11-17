//! The settings page, served by quaked itself on http://127.0.0.1:47380 (this Mac only).
//! It replaces the old companion app: live mirror and knob, panes, shortcuts, soundboard,
//! lighting, general settings and pairing. On a Mac without the Quake, the live view and
//! board controls go through to the Mac that has it.

use crate::app::Ctl;
use crate::device::log;
use crate::sync;
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

pub const PORT: u16 = 47380;
const PAGE: &str = include_str!("web/index.html");
/// Largest request body accepted (clip uploads).
const MAX_BODY: usize = 64 * 1024 * 1024;

pub fn url() -> String {
    format!("http://127.0.0.1:{PORT}/")
}

pub fn spawn(ctl: Ctl) {
    let listener = match TcpListener::bind(("127.0.0.1", PORT)) {
        Ok(l) => l,
        Err(e) => return log(format!("web: can't listen on {PORT}: {e}")),
    };
    log(format!("web: settings page at {}", url()));
    std::thread::Builder::new()
        .name("web".into())
        .spawn(move || {
            for conn in listener.incoming().flatten() {
                let ctl = ctl.clone();
                std::thread::spawn(move || handle(conn, ctl));
            }
        })
        .unwrap();
}

struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn q(&self, k: &str) -> Option<&str> {
        self.query.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(a, _)| a.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
}

struct Response {
    status: u16,
    kind: &'static str,
    body: Vec<u8>,
}

fn ok(kind: &'static str, body: Vec<u8>) -> Response {
    Response { status: 200, kind, body }
}
fn json(v: Value) -> Response {
    ok("application/json", v.to_string().into_bytes())
}
fn fail(status: u16, msg: impl Into<String>) -> Response {
    Response { status, kind: "application/json", body: serde_json::json!({"error": msg.into()}).to_string().into_bytes() }
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn read_request(conn: &TcpStream) -> Option<Request> {
    conn.set_read_timeout(Some(Duration::from_secs(30))).ok();
    let mut r = BufReader::new(conn);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).ok()? == 0 {
            break;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let len: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    if len > MAX_BODY {
        return None;
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    let (path, qs) = target.split_once('?').unwrap_or((&target, ""));
    let query = qs.split('&').filter(|p| !p.is_empty()).map(|p| p.split_once('=').unwrap_or((p, ""))).map(|(k, v)| (decode(k), decode(v))).collect();
    Some(Request { method, path: path.to_string(), query, headers, body })
}

fn handle(mut conn: TcpStream, ctl: Ctl) {
    let Some(req) = read_request(&conn) else { return };
    // Only pages from this server may call the API (no other site can drive the board).
    let cross_site = req.header("Origin").is_some_and(|o| o != "http://127.0.0.1:47380" && o != "http://localhost:47380");
    // And only under its own name, so a site that points its domain at 127.0.0.1 (DNS
    // rebinding) can't read the page's data either.
    let foreign_host = !matches!(req.header("Host"), Some("127.0.0.1:47380" | "localhost:47380"));
    let res = if cross_site || foreign_host { fail(403, "not from this Mac's settings page") } else { route(&req, &ctl) };
    let reason = match res.status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        res.status,
        res.kind,
        res.body.len()
    );
    let _ = conn.write_all(head.as_bytes());
    let _ = conn.write_all(&res.body);
}

/// Where board requests go: this Mac, or the paired Mac that has the Quake.
enum Board {
    Local,
    Remote { name: String, host: String, port: u16, key: String },
    None,
}

fn local_state(ctl: &Ctl) -> Option<Value> {
    serde_json::from_slice(&ctl("state")).ok()
}

fn board_of(state: &Value) -> Board {
    let r = &state["route"];
    match r["kind"].as_str() {
        Some("local") => Board::Local,
        Some("remote") => Board::Remote {
            name: r["name"].as_str().unwrap_or("").into(),
            host: r["host"].as_str().unwrap_or("").into(),
            port: r["port"].as_u64().unwrap_or(0) as u16,
            key: state["config"]["sync"]["key"].as_str().unwrap_or("").into(),
        },
        _ => Board::None,
    }
}

/// A board command on whichever Mac has the Quake.
fn on_board(ctl: &Ctl, board: &Board, line: &str) -> Result<Vec<u8>, String> {
    match board {
        Board::Local | Board::None => Ok(ctl(line)),
        Board::Remote { host, port, key, name } => {
            // Digin sign-in talks to the Digin server from there: allow it longer.
            let wait = if line.starts_with("digin-auth ") { Duration::from_secs(40) } else { Duration::from_secs(8) };
            sync::request_long(host, *port, key, &format!("ctl {line}"), wait).map_err(|e| format!("{name}: {e}"))
        }
    }
}

fn route(req: &Request, ctl: &Ctl) -> Response {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => ok("text/html; charset=utf-8", PAGE.as_bytes().to_vec()),
        ("GET", "/api/state") => {
            let Some(mut state) = local_state(ctl) else { return fail(500, "quaked didn't answer") };
            // The board's own status when another Mac has it.
            if let Board::Remote { .. } = board_of(&state) {
                match on_board(ctl, &board_of(&state), "status").and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| e.to_string())) {
                    Ok(st) => state["status"] = st,
                    Err(e) => state["boardError"] = Value::String(e),
                }
            }
            json(state)
        }
        ("PUT", "/api/config") => {
            let Ok(v) = serde_json::from_slice::<Value>(&req.body) else { return fail(400, "not JSON") };
            let reply = ctl(&format!("config-put {v}"));
            if reply.starts_with(b"ok") {
                json(serde_json::json!({"ok": true}))
            } else {
                fail(400, String::from_utf8_lossy(&reply).trim().to_string())
            }
        }
        ("GET", "/api/frame") => {
            let Some(state) = local_state(ctl) else { return fail(500, "quaked didn't answer") };
            let line = match req.q("page") {
                Some(p) if !p.is_empty() => format!("frame {p}"),
                _ => "frame".into(),
            };
            match on_board(ctl, &board_of(&state), &line) {
                Ok(png) if png.starts_with(b"\x89PNG") => ok("image/png", png),
                Ok(other) => fail(500, String::from_utf8_lossy(&other).trim().to_string()),
                Err(e) => fail(502, e),
            }
        }
        ("POST", "/api/ctl") => {
            let line = String::from_utf8_lossy(&req.body).trim().to_string();
            let allowed = ["turn ", "press", "double", "hold", "tap ", "page ", "shortcut ", "play ", "stop", "alert "];
            if !allowed.iter().any(|a| line == a.trim() || line.starts_with(a)) {
                return fail(400, "not a board command");
            }
            let Some(state) = local_state(ctl) else { return fail(500, "quaked didn't answer") };
            match on_board(ctl, &board_of(&state), &line) {
                Ok(r) => ok("text/plain", r),
                Err(e) => fail(502, e),
            }
        }
        ("GET", "/api/audio-devices") => {
            let Some(state) = local_state(ctl) else { return fail(500, "quaked didn't answer") };
            match on_board(ctl, &board_of(&state), "audio-devices") {
                Ok(r) => ok("application/json", r),
                Err(e) => fail(502, e),
            }
        }
        ("GET", "/api/symbol") => {
            let name: String = req.q("name").unwrap_or("questionmark").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '.').collect();
            let px = req.q("px").and_then(|p| p.parse::<u32>().ok()).unwrap_or(32).clamp(8, 128);
            let png = ctl(&format!("symbol {name} {px}"));
            if png.starts_with(b"\x89PNG") {
                ok("image/png", png)
            } else {
                fail(404, "unknown symbol")
            }
        }
        ("POST", "/api/digin") => {
            // Sign-in steps run on the Mac with the Quake, so its Keychain keeps the token.
            let Ok(v) = serde_json::from_slice::<Value>(&req.body) else { return fail(400, "not JSON") };
            let Some(state) = local_state(ctl) else { return fail(500, "quaked didn't answer") };
            match on_board(ctl, &board_of(&state), &format!("digin-auth {v}")) {
                Ok(r) => ok("application/json", r),
                Err(e) => fail(502, e),
            }
        }
        ("POST", "/api/pair-answer") => {
            let yes = req.q("allow") == Some("1");
            ctl(&format!("pair-answer {}", if yes { "yes" } else { "no" }));
            json(serde_json::json!({"ok": true}))
        }
        ("POST", "/api/pair") => {
            let Some(state) = local_state(ctl) else { return fail(500, "quaked didn't answer") };
            let want = req.q("peer").unwrap_or("");
            let Some(peer) = state["peers"].as_array().and_then(|ps| ps.iter().find(|p| p["name"] == want)) else { return fail(404, "no such Mac") };
            let me = state["machine"].as_str().unwrap_or("Mac");
            match sync::ask_to_pair(peer["host"].as_str().unwrap_or(""), peer["port"].as_u64().unwrap_or(0) as u16, me) {
                Ok(key) => {
                    ctl(&format!("set-key {key}"));
                    json(serde_json::json!({"ok": true}))
                }
                Err(e) => fail(409, e),
            }
        }
        ("POST", "/api/clips") => {
            let Some(state) = local_state(ctl) else { return fail(500, "quaked didn't answer") };
            if !matches!(board_of(&state), Board::Local | Board::None) {
                return fail(409, "add clips on the Mac that has the Quake");
            }
            let raw = req.q("name").unwrap_or("clip");
            let name: String = std::path::Path::new(raw).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default().chars().filter(|c| !c.is_control() && *c != '/').collect();
            let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
            if !["wav", "mp3", "m4a", "aac", "aiff", "aif", "flac", "caf"].contains(&ext.as_str()) || name.starts_with('.') {
                return fail(400, "not an audio file");
            }
            let folder = std::path::PathBuf::from(state["clipsFolder"].as_str().unwrap_or(""));
            let _ = std::fs::create_dir_all(&folder);
            let mut dest = folder.join(&name);
            let stem = name.trim_end_matches(&format!(".{ext}")).to_string();
            let mut n = 2;
            while dest.exists() {
                dest = folder.join(format!("{stem} {n}.{ext}"));
                n += 1;
            }
            match std::fs::write(&dest, &req.body) {
                Ok(()) => {
                    ctl("reload");
                    json(serde_json::json!({"ok": true, "file": dest.file_name().map(|f| f.to_string_lossy().into_owned())}))
                }
                Err(e) => fail(500, e.to_string()),
            }
        }
        ("POST", "/api/open-clips") => {
            let Some(state) = local_state(ctl) else { return fail(500, "quaked didn't answer") };
            let folder = state["clipsFolder"].as_str().unwrap_or("").to_string();
            let _ = std::fs::create_dir_all(&folder);
            let _ = std::process::Command::new("/usr/bin/open").arg(&folder).spawn();
            json(serde_json::json!({"ok": true}))
        }
        _ => fail(404, "not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_query_values() {
        assert_eq!(decode("Mac%20mini+2"), "Mac mini 2");
        assert_eq!(decode("a%2Fb"), "a/b");
        assert_eq!(decode("100%"), "100%");
    }
}
