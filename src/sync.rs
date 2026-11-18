//! Bonjour sync between your Macs. Every quaked advertises `_quaked._tcp` on the local
//! network and serves its Claude sessions to peers that present the shared key; the one
//! with the Quake attached pulls everyone else's and shows them next to its own.
//!
//! The Macs also keep one set of settings: each config carries `updatedAt`, and a Mac
//! that sees newer settings on a peer takes them (all but its own `sync` section).
//!
//! Discovery uses macOS's own Bonjour (dns_sd in libSystem); the data goes over a plain
//! TCP connection: one line in ("<key> summary", "<key> config", "<key> ctl status"...),
//! one reply out. "pair <name>" asks to pair; you allow it on the other Mac.

use crate::config::SyncSettings as SyncCfg;
use crate::device::log;
use crate::status::{self, Session};
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SERVICE: &str = "_quaked._tcp";
const POLL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Peer {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub ok: bool,
    pub error: Option<String>,
    pub sessions: Vec<Session>,
    /// It has the Quake attached.
    pub board: bool,
    /// `updatedAt` of its settings.
    pub config_at: u64,
    /// Its build (git commit), when it's new enough to say.
    pub build: String,
    /// Its version ("0.3.0"), when it's new enough to say (and to update itself).
    pub version: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// The name this Mac advertises.
    pub name: String,
    pub port: u16,
    pub peers: Vec<Peer>,
}

#[derive(Debug)]
pub enum Cmd {
    Configure(SyncCfg),
}

// ---------------------------------------------------------------- dns_sd (libSystem)

type DnsRef = *mut c_void;
type RegisterReply = extern "C" fn(DnsRef, u32, i32, *const c_char, *const c_char, *const c_char, *mut c_void);
type BrowseReply = extern "C" fn(DnsRef, u32, u32, i32, *const c_char, *const c_char, *const c_char, *mut c_void);
type ResolveReply = extern "C" fn(DnsRef, u32, u32, i32, *const c_char, *const c_char, u16, u16, *const u8, *mut c_void);

const FLAG_ADD: u32 = 0x2;

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

extern "C" {
    fn DNSServiceRegister(
        sd: *mut DnsRef,
        flags: u32,
        interface: u32,
        name: *const c_char,
        regtype: *const c_char,
        domain: *const c_char,
        host: *const c_char,
        port: u16,
        txt_len: u16,
        txt: *const c_void,
        callback: Option<RegisterReply>,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceBrowse(sd: *mut DnsRef, flags: u32, interface: u32, regtype: *const c_char, domain: *const c_char, callback: BrowseReply, context: *mut c_void) -> i32;
    fn DNSServiceResolve(sd: *mut DnsRef, flags: u32, interface: u32, name: *const c_char, regtype: *const c_char, domain: *const c_char, callback: ResolveReply, context: *mut c_void) -> i32;
    fn DNSServiceProcessResult(sd: DnsRef) -> i32;
    fn DNSServiceRefSockFD(sd: DnsRef) -> c_int;
    fn DNSServiceRefDeallocate(sd: DnsRef);
    fn poll(fds: *mut PollFd, n: u32, timeout_ms: c_int) -> c_int;
}

/// Waits up to `ms` for a reply on a dns_sd connection, then handles it.
fn process(sd: DnsRef, ms: i32) -> bool {
    let mut p = PollFd { fd: unsafe { DNSServiceRefSockFD(sd) }, events: 1, revents: 0 };
    let ready = unsafe { poll(&mut p, 1, ms) };
    ready > 0 && unsafe { DNSServiceProcessResult(sd) } == 0
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

struct Discovered {
    /// name -> (host, port, interfaces it was seen on)
    peers: Mutex<HashMap<String, (String, u16, u32)>>,
    own: Mutex<String>,
}

extern "C" fn on_register(_: DnsRef, _: u32, err: i32, name: *const c_char, _: *const c_char, _: *const c_char, ctx: *mut c_void) {
    let d = unsafe { &*(ctx as *const Discovered) };
    if err == 0 {
        let name = cstr(name);
        log(format!("sync: advertising as \"{name}\""));
        *d.own.lock().unwrap() = name;
    } else {
        log(format!("sync: Bonjour registration failed ({err})"));
    }
}

extern "C" fn on_resolve(_: DnsRef, _: u32, _: u32, err: i32, _: *const c_char, host: *const c_char, port: u16, _: u16, _: *const u8, ctx: *mut c_void) {
    if err == 0 {
        let out = unsafe { &mut *(ctx as *mut Option<(String, u16)>) };
        *out = Some((cstr(host), u16::from_be(port)));
    }
}

extern "C" fn on_browse(_: DnsRef, flags: u32, interface: u32, err: i32, name: *const c_char, regtype: *const c_char, domain: *const c_char, ctx: *mut c_void) {
    if err != 0 {
        return;
    }
    let d = unsafe { &*(ctx as *const Discovered) };
    let service = cstr(name);
    if flags & FLAG_ADD == 0 {
        let mut peers = d.peers.lock().unwrap();
        if let Some(entry) = peers.get_mut(&service) {
            entry.2 = entry.2.saturating_sub(1);
            if entry.2 == 0 {
                peers.remove(&service);
                log(format!("sync: {service} left"));
            }
        }
        return;
    }
    // Resolve to host and port right here (a separate dns_sd connection). Do it on
    // every announcement: a restarted quaked comes back on a new port.
    let mut found: Option<(String, u16)> = None;
    let mut sd: DnsRef = std::ptr::null_mut();
    let rc = unsafe { DNSServiceResolve(&mut sd, 0, interface, name, regtype, domain, on_resolve, &mut found as *mut _ as *mut c_void) };
    if rc == 0 {
        process(sd, 5000);
        unsafe { DNSServiceRefDeallocate(sd) };
    }
    let mut peers = d.peers.lock().unwrap();
    match (peers.get_mut(&service), found) {
        (Some(entry), Some((host, port))) => {
            if entry.0 != host || entry.1 != port {
                log(format!("sync: {service} moved to {host}:{port}"));
            }
            *entry = (host, port, entry.2 + 1);
        }
        (Some(entry), None) => entry.2 += 1,
        (None, Some((host, port))) => {
            log(format!("sync: found {service} at {host}:{port}"));
            peers.insert(service, (host, port, 1));
        }
        (None, None) => {}
    }
}

/// The Mac's name as Sharing settings show it ("Austin's Mac mini").
pub fn computer_name() -> String {
    std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Mac".into())
}

/// A fresh pairing key, like "k7m2-9xqa-p4fd".
pub fn new_key() -> String {
    let mut bytes = [0u8; 12];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut bytes);
    }
    const A: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let chars: String = bytes.iter().map(|b| A[*b as usize % A.len()] as char).collect();
    format!("{}-{}-{}", &chars[0..4], &chars[4..8], &chars[8..12])
}

/// Equal-time comparison, so the key can't be guessed a byte at a time.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------------------------------------------------------------- serving

/// What the app lends the sync thread.
#[derive(Clone)]
pub struct Hooks {
    /// Runs a control command on this Mac's board: the same lines `quaked ctl` takes.
    pub ctl: Arc<dyn Fn(&str) -> Vec<u8> + Send + Sync>,
    /// Asks you to allow a pairing request from the named Mac; blocks until you answer.
    pub pair: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// This Mac has the Quake attached.
    pub board: Arc<AtomicBool>,
    /// `updatedAt` of this Mac's settings.
    pub config_at: Arc<AtomicU64>,
}

/// What the sync thread tells the app.
pub enum Out {
    Status(Status),
    /// A peer has newer settings (its config.json, as JSON).
    Config { from: String, json: String },
}

/// One request per connection. Paired peers send "<key> <what>"; an unpaired one may
/// send "pair <its name>", which waits for you to allow it and then hands over the key.
fn serve(listener: TcpListener, cfg: Arc<Mutex<SyncCfg>>, name: Arc<Mutex<String>>, hooks: Hooks) {
    let pairing = Arc::new(AtomicBool::new(false));
    for conn in listener.incoming().flatten() {
        let (cfg, name, hooks, pairing) = (cfg.clone(), name.clone(), hooks.clone(), pairing.clone());
        std::thread::spawn(move || {
            let _ = conn.set_read_timeout(Some(Duration::from_secs(3)));
            let mut line = String::new();
            if BufReader::new(&conn).read_line(&mut line).is_err() {
                return;
            }
            let line = line.trim();
            let c = cfg.lock().unwrap().clone();
            let reply: Vec<u8> = if !c.enabled {
                b"disabled\n".to_vec()
            } else if let Some(who) = line.strip_prefix("pair ") {
                if c.key.is_empty() || pairing.swap(true, Ordering::SeqCst) {
                    b"busy\n".to_vec()
                } else {
                    let who: String = who.chars().filter(|c| !c.is_control()).take(64).collect();
                    log(format!("sync: {who} asks to pair"));
                    let allowed = (hooks.pair)(&who);
                    pairing.store(false, Ordering::SeqCst);
                    log(format!("sync: pairing with {who} {}", if allowed { "allowed" } else { "refused" }));
                    if allowed { format!("key {}\n", c.key).into_bytes() } else { b"denied\n".to_vec() }
                }
            } else {
                let (key, what) = line.split_once(' ').unwrap_or(("", ""));
                if c.key.is_empty() || !same(key, &c.key) {
                    b"denied\n".to_vec()
                } else if what == "sessions" {
                    let me = name.lock().unwrap().clone();
                    (serde_json::json!({"name": me, "sessions": status::load(status::now_ms())}).to_string() + "\n").into_bytes()
                } else if what == "summary" {
                    let me = name.lock().unwrap().clone();
                    (serde_json::json!({"name": me, "board": hooks.board.load(Ordering::Relaxed), "configAt": hooks.config_at.load(Ordering::Relaxed), "build": env!("QUAKED_BUILD"), "version": crate::update::CURRENT}).to_string() + "\n").into_bytes()
                } else if what == "config" {
                    (hooks.ctl)("config")
                } else if let Some(cmd) = what.strip_prefix("ctl ") {
                    (hooks.ctl)(cmd)
                } else {
                    b"unknown\n".to_vec()
                }
            };
            let mut w = &conn;
            let _ = w.write_all(&reply);
        });
    }
}

/// Connects to a peer: IPv4 first (a ".local" name often resolves to unscoped IPv6
/// link-local addresses first, which can't be reached), then the rest.
fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
    let mut addrs: Vec<std::net::SocketAddr> = (host.trim_end_matches('.'), port).to_socket_addrs().map_err(|e| e.to_string())?.collect();
    addrs.sort_by_key(|a| !a.is_ipv4());
    addrs.dedup();
    let mut last = "can't resolve".to_string();
    for a in addrs {
        match TcpStream::connect_timeout(&a, Duration::from_secs(2)) {
            Ok(s) => return Ok(s),
            Err(e) => last = e.to_string().split(" (os error").next().unwrap_or("").to_string(),
        }
    }
    Err(last)
}

/// Sends one line to a peer and returns its reply.
fn exchange(host: &str, port: u16, line: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    let mut s = connect(host, port)?;
    s.set_read_timeout(Some(timeout)).ok();
    writeln!(s, "{line}").map_err(|e| e.to_string())?;
    let mut reply = Vec::new();
    s.read_to_end(&mut reply).map_err(|e| e.to_string())?;
    Ok(reply)
}

/// A request to a paired peer ("summary", "config", "ctl status", ...).
pub fn request(host: &str, port: u16, key: &str, what: &str) -> Result<Vec<u8>, String> {
    request_long(host, port, key, what, Duration::from_secs(8))
}

/// Like `request`, for ones that wait on you (a question on the other Mac's Quake).
pub fn request_long(host: &str, port: u16, key: &str, what: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    let reply = exchange(host, port, &format!("{key} {what}"), timeout)?;
    match reply.as_slice() {
        b"denied\n" => Err("key doesn't match".into()),
        b"disabled\n" => Err("sync is off there".into()),
        b"unknown\n" => Err("that Mac's quaked is older; update it".into()),
        _ => Ok(reply),
    }
}

/// Asks a peer to pair. It waits (up to a minute) for someone to allow it there, then
/// returns that Mac's key.
pub fn ask_to_pair(host: &str, port: u16, me: &str) -> Result<String, String> {
    let reply = exchange(host, port, &format!("pair {me}"), Duration::from_secs(75))?;
    let text = String::from_utf8_lossy(&reply);
    match text.trim().strip_prefix("key ") {
        Some(k) if !k.is_empty() => Ok(k.to_string()),
        _ => Err(match text.trim() {
            "denied" => "not allowed on the other Mac".into(),
            "busy" => "the other Mac is already answering a request".into(),
            "disabled" => "sync is off there".into(),
            "" => "no answer (the other Mac's quaked may be older)".into(),
            other => other.to_string(),
        }),
    }
}

fn fetch_sessions(host: &str, port: u16, key: &str) -> Result<Vec<Session>, String> {
    let reply = request(host, port, key, "sessions")?;
    let v: serde_json::Value = serde_json::from_slice(&reply).map_err(|_| "bad reply".to_string())?;
    serde_json::from_value(v.get("sessions").cloned().unwrap_or_default()).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- the thread

pub fn spawn(cfg: SyncCfg, hooks: Hooks, emit: impl Fn(Out) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("sync".into()).spawn(move || run(cfg, hooks, rx, emit)).unwrap();
    tx
}

fn run(cfg: SyncCfg, hooks: Hooks, rx: Receiver<Cmd>, emit: impl Fn(Out)) {
    let shared = Arc::new(Mutex::new(cfg.clone()));
    let name = Arc::new(Mutex::new(if cfg.name.is_empty() { computer_name() } else { cfg.name.clone() }));
    // IPv6 and IPv4 both (macOS sockets bound to [::] take IPv4 too).
    let listener = match TcpListener::bind("[::]:0").or_else(|_| TcpListener::bind("0.0.0.0:0")) {
        Ok(l) => l,
        Err(e) => return log(format!("sync: can't listen: {e}")),
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    {
        let (shared, name, hooks) = (shared.clone(), name.clone(), hooks.clone());
        std::thread::Builder::new().name("sync-serve".into()).spawn(move || serve(listener, shared, name, hooks)).unwrap();
    }
    let disc: &'static Discovered = Box::leak(Box::new(Discovered { peers: Mutex::new(HashMap::new()), own: Mutex::new(String::new()) }));
    let ctx = disc as *const Discovered as *mut c_void;
    let regtype = CString::new(SERVICE).unwrap();

    // Browse for the others (always: the settings page lists them, paired or not).
    let ctx_addr = ctx as usize;
    std::thread::Builder::new()
        .name("sync-browse".into())
        .spawn(move || {
            let ctx = ctx_addr as *mut c_void;
            let regtype = CString::new(SERVICE).unwrap();
            let mut sd: DnsRef = std::ptr::null_mut();
            let rc = unsafe { DNSServiceBrowse(&mut sd, 0, 0, regtype.as_ptr(), std::ptr::null(), on_browse, ctx) };
            if rc != 0 {
                return log(format!("sync: Bonjour browse failed ({rc})"));
            }
            loop {
                process(sd, 60_000);
            }
        })
        .unwrap();

    // Advertise while enabled.
    struct Advert(DnsRef);
    unsafe impl Send for Advert {}
    let advert: Arc<Mutex<Option<Advert>>> = Arc::new(Mutex::new(None));
    let advertise = |on: bool, display: &str| {
        let mut a = advert.lock().unwrap();
        if let Some(old) = a.take() {
            unsafe { DNSServiceRefDeallocate(old.0) };
        }
        if !on {
            return;
        }
        let cname = CString::new(display.replace('\0', "")).unwrap_or_default();
        let mut sd: DnsRef = std::ptr::null_mut();
        let rc = unsafe { DNSServiceRegister(&mut sd, 0, 0, cname.as_ptr(), regtype.as_ptr(), std::ptr::null(), std::ptr::null(), port.to_be(), 0, std::ptr::null(), Some(on_register), ctx) };
        if rc == 0 {
            process(sd, 3000); // the registration callback
            *a = Some(Advert(sd));
        } else {
            log(format!("sync: Bonjour registration failed ({rc})"));
        }
    };
    let mut cfg = cfg;
    let mut display = name.lock().unwrap().clone();
    advertise(cfg.enabled, &display);

    let mut last = Status::default();
    let mut known: HashMap<String, Peer> = HashMap::new();
    // The newest settings stamp already handed to the app, per peer.
    let mut pulled: HashMap<String, u64> = HashMap::new();
    let mut next = Instant::now();
    loop {
        if Instant::now() >= next {
            next = Instant::now() + POLL;
            let own = disc.own.lock().unwrap().clone();
            let found: Vec<(String, (String, u16, u32))> = disc.peers.lock().unwrap().iter().filter(|(n, _)| **n != own && **n != display).map(|(n, v)| (n.clone(), v.clone())).collect();
            known.retain(|n, _| found.iter().any(|(f, _)| f == n));
            let want_sessions = hooks.board.load(Ordering::Relaxed);
            for (n, (host, port, _)) in &found {
                let p = known.entry(n.clone()).or_insert_with(|| Peer { name: n.clone(), ..Default::default() });
                p.host = host.clone();
                p.port = *port;
                if !cfg.enabled {
                    p.ok = false;
                    p.error = Some("sync is off here".into());
                    p.sessions.clear();
                    continue;
                }
                let summary = match request(host, *port, &cfg.key, "summary") {
                    // An older quaked only shares sessions: no board flag, no settings.
                    Err(e) if e.contains("older") => Ok(serde_json::json!({"board": false, "configAt": 0, "older": true})),
                    other => other.and_then(|r| serde_json::from_slice::<serde_json::Value>(&r).map_err(|_| "bad reply".to_string())),
                };
                match summary {
                    Ok(v) => {
                        if !p.ok {
                            log(format!("sync: connected to {n}"));
                        }
                        p.ok = true;
                        p.error = v.get("older").map(|_| "older quaked there: update it to share settings".to_string());
                        p.board = v.get("board").and_then(|b| b.as_bool()).unwrap_or(false);
                        p.config_at = v.get("configAt").and_then(|b| b.as_u64()).unwrap_or(0);
                        p.build = v.get("build").and_then(|b| b.as_str()).unwrap_or("").to_string();
                        p.version = v.get("version").and_then(|b| b.as_str()).unwrap_or("").to_string();
                    }
                    Err(e) => {
                        if p.error.as_ref() != Some(&e) {
                            log(format!("sync: {n}: {e}"));
                        }
                        p.ok = false;
                        p.board = false;
                        p.error = Some(e);
                        p.sessions.clear();
                        continue;
                    }
                }
                // Newer settings over there: hand them to the app (once per version).
                if p.config_at > hooks.config_at.load(Ordering::Relaxed) && pulled.get(n) != Some(&p.config_at) {
                    match request(host, *port, &cfg.key, "config") {
                        Ok(json) => {
                            pulled.insert(n.clone(), p.config_at);
                            emit(Out::Config { from: n.clone(), json: String::from_utf8_lossy(&json).into_owned() });
                        }
                        Err(e) => log(format!("sync: settings from {n}: {e}")),
                    }
                }
                if want_sessions {
                    match fetch_sessions(host, *port, &cfg.key) {
                        Ok(s) => p.sessions = s,
                        Err(e) => {
                            p.sessions.clear();
                            p.error = Some(e);
                        }
                    }
                } else {
                    p.sessions.clear();
                }
            }
            let mut peers: Vec<Peer> = known.values().cloned().collect();
            peers.sort_by(|a, b| a.name.cmp(&b.name));
            let st = Status { name: if own.is_empty() { display.clone() } else { own }, port, peers };
            if st != last {
                last = st.clone();
                emit(Out::Status(st));
            }
        }
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Cmd::Configure(c)) => {
                let new_display = if c.name.is_empty() { computer_name() } else { c.name.clone() };
                let readvertise = c.enabled != cfg.enabled || new_display != display;
                *shared.lock().unwrap() = c.clone();
                *name.lock().unwrap() = new_display.clone();
                cfg = c;
                if readvertise {
                    display = new_display;
                    advertise(cfg.enabled, &display);
                }
                next = Instant::now();
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
    fn keys_look_right_and_compare() {
        let k = new_key();
        assert_eq!(k.len(), 14);
        assert_eq!(k.matches('-').count(), 2);
        assert!(same(&k, &k.clone()));
        assert!(!same(&k, "nope"));
        assert_ne!(new_key(), k);
    }
}
