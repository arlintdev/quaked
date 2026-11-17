//! DIGIN pane data: your open issues, unread inbox (mentions, DMs, assignments), chat
//! unread counts and recently changed pages, from one or more Digin servers.
//!
//! Sign-in is Digin's email code (POST /auth/email/start, then /auth/email/token), which
//! gives a read-only `digin_` API token (90 days, no refresh); a token made in Digin's
//! Settings → Access works too. Tokens live in this Mac's Keychain (see secret.rs), never
//! in config.json. The REST API is /api/v1 with `Authorization: Bearer`.

use crate::config::{Digin as DiginCfg, DiginAccount};
use crate::device::log;
use crate::secret;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Account {
    pub url: String,
    pub label: String,
    pub email: String,
    pub signed_in: bool,
    pub handle: String,
    pub name: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Issue {
    pub account: usize,
    /// "KES-12"
    pub key: String,
    pub title: String,
    /// backlog, todo, in_progress, waiting, done, canceled
    pub status: String,
    pub status_name: String,
    /// 0 none, 1 urgent, 2 high, 3 medium, 4 low
    pub priority: u8,
    pub url: String,
    pub updated: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Note {
    pub account: usize,
    pub id: i64,
    /// assigned, mentioned, commented, status, due, changed, message, call_missed...
    pub kind: String,
    pub title: String,
    pub body: String,
    pub url: String,
    pub created: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DocPage {
    pub account: usize,
    pub title: String,
    pub project: String,
    pub updated: u64,
    /// Someone else edited it since you last read it.
    pub changed: bool,
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub accounts: Vec<Account>,
    pub issues: Vec<Issue>,
    pub inbox: Vec<Note>,
    pub pages: Vec<DocPage>,
    /// Unread chat messages and @-mentions across channels, and how many channels.
    pub chat_unread: u32,
    pub chat_mentions: u32,
    pub chat_channels: u32,
}

impl Status {
    pub fn signed_in(&self) -> bool {
        self.accounts.iter().any(|a| a.signed_in)
    }
}

#[derive(Debug)]
pub enum Cmd {
    Configure(DiginCfg),
    Refresh,
}

pub fn normalize(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    if u.starts_with("http://") || u.starts_with("https://") {
        u.to_string()
    } else {
        format!("https://{u}")
    }
}

fn keychain_account(url: &str) -> String {
    format!("digin:{}", normalize(url))
}

pub fn token_for(url: &str) -> Option<String> {
    secret::get(&keychain_account(url))
}

pub fn host_of(url: &str) -> String {
    normalize(url).trim_start_matches("https://").trim_start_matches("http://").to_string()
}

/// RFC 3339 -> unix ms.
fn time_ms(s: &str) -> u64 {
    crate::github::iso_ms(s)
}

/// One HTTP request through curl. The bearer token goes in on stdin (`-K -`), so it never
/// shows up in the process list. Returns (status, body).
fn curl(method: &str, url: &str, token: Option<&str>, body: Option<&Value>) -> Result<(u16, Vec<u8>), String> {
    let mut args: Vec<String> = vec!["-s".into(), "--max-time".into(), "12".into(), "-X".into(), method.into(), "-K".into(), "-".into(), "-w".into(), "\n%{http_code}".into()];
    let mut config = String::from("header = \"Accept: application/json\"\nheader = \"User-Agent: quaked\"\n");
    if let Some(t) = token {
        config.push_str(&format!("header = \"Authorization: Bearer {}\"\n", t.replace('"', "")));
    }
    if let Some(b) = body {
        config.push_str("header = \"Content-Type: application/json\"\n");
        config.push_str(&format!("data-binary = \"{}\"\n", b.to_string().replace('\\', "\\\\").replace('"', "\\\"")));
    }
    args.push(url.into());
    let mut child = Command::new("/usr/bin/curl").args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|e| e.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(config.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let mut text = out.stdout;
    let nl = text.iter().rposition(|b| *b == b'\n').ok_or("no answer")?;
    let code: u16 = String::from_utf8_lossy(&text[nl + 1..]).trim().parse().map_err(|_| "no answer".to_string())?;
    text.truncate(nl);
    if code == 0 {
        return Err("can't reach the server".into());
    }
    Ok((code, text))
}

fn error_of(code: u16, body: &[u8]) -> String {
    let v: Value = serde_json::from_slice(body).unwrap_or_default();
    let msg = v.pointer("/error/message").and_then(Value::as_str).or_else(|| v.pointer("/error/code").and_then(Value::as_str)).unwrap_or("");
    match code {
        401 => "signed out (sign in again)".into(),
        403 => format!("not allowed{}", if msg.is_empty() { String::new() } else { format!(": {msg}") }),
        _ if msg.is_empty() => format!("HTTP {code}"),
        _ => msg.to_string(),
    }
}

fn get(base: &str, token: &str, path: &str) -> Result<Value, String> {
    let (code, body) = curl("GET", &format!("{base}{path}"), Some(token), None)?;
    if !(200..300).contains(&code) {
        return Err(error_of(code, &body));
    }
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- sign-in

/// What a server offers: `{providers:[{id,kind,name}], gate?, ...}`.
pub fn providers(url: &str) -> Result<Value, String> {
    let (code, body) = curl("GET", &format!("{}/auth/providers", normalize(url)), None, None)?;
    if code != 200 {
        return Err(format!("{} doesn't look like a Digin server ({})", host_of(url), error_of(code, &body)));
    }
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}

fn permits() -> Value {
    serde_json::json!({"issues": "read", "pages": "read", "chat": "read", "profile": "read"})
}

/// The `quaked ctl digin-auth <json>` commands, run off the main thread by the control
/// socket (on the Mac with the Quake, so the token is made and kept there). Returns the
/// reply and, after a sign-in, the account to add to the settings.
pub fn auth(json: &str) -> (Value, Option<DiginAccount>) {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return (serde_json::json!({"error": "bad request"}), None) };
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let url = normalize(&s("url"));
    let fail = |e: String| (serde_json::json!({"error": e}), None);
    match s("step").as_str() {
        "providers" => match providers(&url) {
            Ok(p) => (p, None),
            Err(e) => fail(e),
        },
        "email-start" => match curl("POST", &format!("{url}/auth/email/start"), None, Some(&serde_json::json!({"email": s("email")}))) {
            Ok((204 | 200, _)) => (serde_json::json!({"ok": true}), None),
            Ok((code, body)) => fail(error_of(code, &body)),
            Err(e) => fail(e),
        },
        "email-token" | "token" => {
            let token = if s("step") == "token" {
                s("token")
            } else {
                let mut body = serde_json::json!({"email": s("email"), "code": s("code"), "name": format!("quaked on {}", crate::sync::computer_name()), "permits": permits()});
                if let Some(n) = v.get("notice").filter(|n| !n.is_null()) {
                    body["notice"] = n.clone();
                }
                match curl("POST", &format!("{url}/auth/email/token"), None, Some(&body)) {
                    Ok((code, body)) if (200..300).contains(&code) => {
                        let r: Value = serde_json::from_slice(&body).unwrap_or_default();
                        r.get("token").and_then(Value::as_str).unwrap_or("").to_string()
                    }
                    Ok((409, body)) => {
                        // The organisation has a notice to accept first.
                        let r: Value = serde_json::from_slice(&body).unwrap_or_default();
                        return (serde_json::json!({"notice": r.get("notice")}), None);
                    }
                    Ok((400, _)) => return fail("That code didn't work (wrong, used or expired). Send a new one.".into()),
                    Ok((code, body)) => return fail(error_of(code, &body)),
                    Err(e) => return fail(e),
                }
            };
            if !token.starts_with("digin_") {
                return fail("That isn't a Digin token (they start with digin_).".into());
            }
            // Make sure it works before keeping it.
            let me = match get(&url, &token, "/api/v1/bootstrap") {
                Ok(b) => b.get("me").cloned().unwrap_or_default(),
                Err(e) => return fail(e),
            };
            if let Err(e) = secret::set(&keychain_account(&url), &token) {
                return fail(format!("couldn't save it in the Keychain: {e}"));
            }
            let name = me.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let email = me.get("email").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| s("email"));
            log(format!("digin: signed in to {} as {email}", host_of(&url)));
            (serde_json::json!({"ok": true, "name": name}), Some(DiginAccount { url: url.clone(), label: host_of(&url), email }))
        }
        "sign-out" => {
            secret::delete(&keychain_account(&url));
            log(format!("digin: signed out of {}", host_of(&url)));
            (serde_json::json!({"ok": true}), None)
        }
        _ => fail("unknown step".into()),
    }
}

// ---------------------------------------------------------------- polling

/// Names and links that rarely change, per account (from /api/v1/bootstrap).
#[derive(Default, Clone)]
struct Directory {
    /// project id -> (KEY, workspace uuid)
    projects: HashMap<i64, (String, String)>,
    handle: String,
    name: String,
    at: Option<Instant>,
}

fn directory(base: &str, token: &str) -> Result<Directory, String> {
    let b = get(base, token, "/api/v1/bootstrap")?;
    let mut workspaces: HashMap<i64, String> = HashMap::new();
    for w in b.get("workspaces").and_then(Value::as_array).into_iter().flatten() {
        if let (Some(id), Some(uuid)) = (w.get("id").and_then(Value::as_i64), w.get("uuid").and_then(Value::as_str)) {
            workspaces.insert(id, uuid.to_string());
        }
    }
    let mut projects = HashMap::new();
    for p in b.get("projects").and_then(Value::as_array).into_iter().flatten() {
        let (Some(id), Some(key)) = (p.get("id").and_then(Value::as_i64), p.get("key").and_then(Value::as_str)) else { continue };
        let ws = p.get("workspaceId").and_then(Value::as_i64).and_then(|w| workspaces.get(&w).cloned()).unwrap_or_default();
        projects.insert(id, (key.to_string(), ws));
    }
    Ok(Directory {
        projects,
        handle: b.pointer("/me/handle").and_then(Value::as_str).unwrap_or("").to_string(),
        name: b.pointer("/me/name").and_then(Value::as_str).unwrap_or("").to_string(),
        at: Some(Instant::now()),
    })
}

fn link(base: &str, dir: &Directory, project: i64, rest: &str) -> String {
    match dir.projects.get(&project) {
        Some((key, ws)) if !ws.is_empty() => format!("{base}/w/{ws}/{key}/{rest}"),
        Some((key, _)) => format!("{base}/{key}/{rest}"),
        None => base.to_string(),
    }
}

struct Fetched {
    issues: Vec<Issue>,
    inbox: Vec<Note>,
    pages: Option<Vec<DocPage>>,
    chat: (u32, u32, u32),
}

fn fetch(i: usize, base: &str, token: &str, dir: &Directory, with_pages: bool) -> Result<Fetched, String> {
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let my = get(base, token, "/api/v1/my")?;
    let issues = my
        .get("open")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|x| {
            let project = x.get("projectId").and_then(Value::as_i64).unwrap_or(0);
            let number = x.get("number").and_then(Value::as_i64).unwrap_or(0);
            let key = s(x, "key");
            Issue {
                account: i,
                key: format!("{key}-{number}"),
                title: s(x, "title"),
                status: s(x, "status"),
                status_name: s(x, "statusName"),
                priority: x.get("priority").and_then(Value::as_u64).unwrap_or(0) as u8,
                url: link(base, dir, project, &format!("issues/{number}")),
                updated: time_ms(&s(x, "updatedAt")),
            }
        })
        .collect();
    let notes = get(base, token, "/api/v1/notifications?unread=1&limit=30")?;
    let inbox = notes
        .get("notifications")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|n| {
            let url = s(n, "url");
            Note {
                account: i,
                id: n.get("id").and_then(Value::as_i64).unwrap_or(0),
                kind: s(n, "kind"),
                title: s(n, "title"),
                body: s(n, "body"),
                url: if url.starts_with('/') { format!("{base}{url}") } else if url.is_empty() { format!("{base}/inbox") } else { url },
                created: time_ms(&s(n, "createdAt")),
            }
        })
        .collect();
    let mut chat = (0, 0, 0);
    if let Ok(channels) = get(base, token, "/api/v1/channels") {
        for c in channels.as_array().into_iter().flatten() {
            let unread = c.get("unread").and_then(Value::as_u64).unwrap_or(0) as u32;
            let mentions = c.get("mentions").and_then(Value::as_u64).unwrap_or(0) as u32;
            if unread > 0 || mentions > 0 {
                chat.0 += unread;
                chat.1 += mentions;
                chat.2 += 1;
            }
        }
    }
    let pages = if with_pages {
        let all = get(base, token, "/api/v1/pages")?;
        let mut list: Vec<DocPage> = all
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| {
                let project = p.get("projectId").and_then(Value::as_i64).unwrap_or(0);
                let id = p.get("id").map(|v| v.to_string().trim_matches('"').to_string()).unwrap_or_default();
                DocPage {
                    account: i,
                    title: s(p, "title"),
                    project: dir.projects.get(&project).map(|(k, _)| k.clone()).unwrap_or_default(),
                    updated: time_ms(&s(p, "updatedAt")),
                    changed: p.get("changed").and_then(Value::as_bool).unwrap_or(false),
                    url: link(base, dir, project, &format!("pages/{id}")),
                }
            })
            .collect();
        list.sort_by(|a, b| b.updated.cmp(&a.updated));
        list.truncate(30);
        Some(list)
    } else {
        None
    };
    Ok(Fetched { issues, inbox, pages, chat })
}

pub fn spawn(cfg: DiginCfg, fast: Arc<AtomicBool>, emit: impl Fn(Status) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("digin".into()).spawn(move || run(cfg, fast, rx, emit)).unwrap();
    tx
}

fn run(mut cfg: DiginCfg, fast: Arc<AtomicBool>, rx: Receiver<Cmd>, emit: impl Fn(Status)) {
    let mut last = Status::default();
    let mut dirs: HashMap<String, Directory> = HashMap::new();
    let mut pages: HashMap<String, Vec<DocPage>> = HashMap::new();
    let mut next = Instant::now();
    let mut next_pages = Instant::now();
    let mut errors: HashMap<String, String> = HashMap::new();
    loop {
        if Instant::now() >= next {
            let with_pages = Instant::now() >= next_pages;
            let mut st = Status::default();
            for (i, acct) in cfg.accounts.iter().enumerate() {
                let base = normalize(&acct.url);
                let mut a = Account { url: base.clone(), label: if acct.label.is_empty() { host_of(&base) } else { acct.label.clone() }, email: acct.email.clone(), ..Default::default() };
                let Some(token) = token_for(&base) else {
                    a.error = Some("not signed in on this Mac".into());
                    st.accounts.push(a);
                    continue;
                };
                a.signed_in = true;
                let stale = dirs.get(&base).and_then(|d| d.at).is_none_or(|t| t.elapsed() > Duration::from_secs(600));
                if stale {
                    match directory(&base, &token) {
                        Ok(d) => {
                            dirs.insert(base.clone(), d);
                        }
                        Err(e) => {
                            a.error = Some(e.clone());
                            if e.starts_with("signed out") {
                                a.signed_in = false;
                            }
                        }
                    }
                }
                let dir = dirs.get(&base).cloned().unwrap_or_default();
                a.handle = dir.handle.clone();
                a.name = dir.name.clone();
                if a.error.is_none() {
                    match fetch(i, &base, &token, &dir, with_pages) {
                        Ok(f) => {
                            st.issues.extend(f.issues);
                            st.inbox.extend(f.inbox);
                            if let Some(p) = f.pages {
                                pages.insert(base.clone(), p);
                            }
                            st.chat_unread += f.chat.0;
                            st.chat_mentions += f.chat.1;
                            st.chat_channels += f.chat.2;
                        }
                        Err(e) => a.error = Some(e),
                    }
                }
                if let Some(e) = &a.error {
                    if errors.get(&base) != Some(e) {
                        log(format!("digin {}: {e}", host_of(&base)));
                        errors.insert(base.clone(), e.clone());
                    }
                } else {
                    errors.remove(&base);
                }
                if let Some(p) = pages.get(&base) {
                    st.pages.extend(p.iter().cloned().map(|mut d| {
                        d.account = i;
                        d
                    }));
                }
                st.accounts.push(a);
            }
            // Most urgent first: in progress, then by priority (urgent = 1 ... none = 0 last).
            let rank = |s: &str| match s {
                "in_progress" => 0,
                "waiting" => 1,
                "todo" => 2,
                _ => 3,
            };
            st.issues.sort_by_key(|x| (rank(&x.status), if x.priority == 0 { 9 } else { x.priority }, std::cmp::Reverse(x.updated)));
            st.inbox.sort_by(|a, b| b.created.cmp(&a.created));
            st.pages.sort_by(|a, b| b.updated.cmp(&a.updated));
            if st != last {
                last = st.clone();
                emit(st);
            }
            if with_pages {
                next_pages = Instant::now() + Duration::from_secs(300);
            }
            let secs = cfg.refresh_seconds.max(15);
            next = Instant::now() + Duration::from_secs(if fast.load(Ordering::Relaxed) { (secs / 3).max(15) } else { secs });
        }
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Cmd::Configure(c)) => {
                cfg = c;
                next = Instant::now();
                next_pages = Instant::now();
            }
            Ok(Cmd::Refresh) => {
                next = Instant::now();
                next_pages = Instant::now();
                dirs.clear();
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_server_addresses() {
        assert_eq!(normalize("digin.work/"), "https://digin.work");
        assert_eq!(normalize(" https://x.example.com "), "https://x.example.com");
        assert_eq!(host_of("https://digin.work"), "digin.work");
    }
}
