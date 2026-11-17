//! Bonjour sync between your Macs. Every quaked advertises `_quaked._tcp` on the local
//! network and serves its Claude sessions to peers that present the shared key; the one
//! with the Quake attached pulls everyone else's and shows them next to its own.
//!
//! Discovery uses macOS's own Bonjour (dns_sd in libSystem); the data goes over a plain
//! TCP connection: one line in ("<key> sessions"), one JSON reply out.

use crate::config::SyncSettings as SyncCfg;
use crate::device::log;
use crate::status::{self, Session};
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
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
    if let Some(entry) = d.peers.lock().unwrap().get_mut(&service) {
        entry.2 += 1;
        return;
    }
    // Resolve to host and port right here (a separate dns_sd connection).
    let mut found: Option<(String, u16)> = None;
    let mut sd: DnsRef = std::ptr::null_mut();
    let rc = unsafe { DNSServiceResolve(&mut sd, 0, interface, name, regtype, domain, on_resolve, &mut found as *mut _ as *mut c_void) };
    if rc == 0 {
        process(sd, 5000);
        unsafe { DNSServiceRefDeallocate(sd) };
    }
    if let Some((host, port)) = found {
        log(format!("sync: found {service} at {host}:{port}"));
        d.peers.lock().unwrap().insert(service, (host, port, 1));
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

fn serve(listener: TcpListener, cfg: Arc<Mutex<SyncCfg>>, name: Arc<Mutex<String>>) {
    for conn in listener.incoming().flatten() {
        let cfg = cfg.clone();
        let name = name.clone();
        std::thread::spawn(move || {
            let _ = conn.set_read_timeout(Some(Duration::from_secs(3)));
            let mut line = String::new();
            if BufReader::new(&conn).read_line(&mut line).is_err() {
                return;
            }
            let (key, what) = line.trim().split_once(' ').unwrap_or(("", ""));
            let c = cfg.lock().unwrap().clone();
            let reply = if !c.enabled {
                "disabled\n".to_string()
            } else if c.key.is_empty() || !same(key, &c.key) {
                "denied\n".to_string()
            } else if what == "sessions" {
                let me = name.lock().unwrap().clone();
                serde_json::json!({"name": me, "sessions": status::load(status::now_ms())}).to_string() + "\n"
            } else {
                "unknown\n".to_string()
            };
            let mut w = &conn;
            let _ = w.write_all(reply.as_bytes());
        });
    }
}

fn fetch(host: &str, port: u16, key: &str) -> Result<Vec<Session>, String> {
    let addr = (host.trim_end_matches('.'), port).to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("can't resolve")?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|e| e.to_string().split(" (os error").next().unwrap_or("").to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(4))).ok();
    writeln!(s, "{key} sessions").map_err(|e| e.to_string())?;
    let mut reply = String::new();
    s.read_to_string(&mut reply).map_err(|e| e.to_string())?;
    match reply.trim() {
        "denied" => return Err("key doesn't match".into()),
        "disabled" => return Err("sync is off there".into()),
        _ => {}
    }
    let v: serde_json::Value = serde_json::from_str(&reply).map_err(|_| "bad reply".to_string())?;
    serde_json::from_value(v.get("sessions").cloned().unwrap_or_default()).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- the thread

/// `want` is set while this Mac has the Quake attached: only then are peers polled.
pub fn spawn(cfg: SyncCfg, want: Arc<AtomicBool>, emit: impl Fn(Status) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("sync".into()).spawn(move || run(cfg, want, rx, emit)).unwrap();
    tx
}

fn run(cfg: SyncCfg, want: Arc<AtomicBool>, rx: Receiver<Cmd>, emit: impl Fn(Status)) {
    let shared = Arc::new(Mutex::new(cfg.clone()));
    let name = Arc::new(Mutex::new(if cfg.name.is_empty() { computer_name() } else { cfg.name.clone() }));
    let listener = match TcpListener::bind("0.0.0.0:0") {
        Ok(l) => l,
        Err(e) => return log(format!("sync: can't listen: {e}")),
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    {
        let (shared, name) = (shared.clone(), name.clone());
        std::thread::Builder::new().name("sync-serve".into()).spawn(move || serve(listener, shared, name)).unwrap();
    }
    let disc: &'static Discovered = Box::leak(Box::new(Discovered { peers: Mutex::new(HashMap::new()), own: Mutex::new(String::new()) }));
    let ctx = disc as *const Discovered as *mut c_void;
    let regtype = CString::new(SERVICE).unwrap();

    // Browse for the others (always; cheap, and the list shows up in the companion).
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
    let mut next = Instant::now();
    loop {
        if Instant::now() >= next {
            next = Instant::now() + POLL;
            let own = disc.own.lock().unwrap().clone();
            let found: Vec<(String, (String, u16, u32))> = disc.peers.lock().unwrap().iter().filter(|(n, _)| **n != own && **n != display).map(|(n, v)| (n.clone(), v.clone())).collect();
            known.retain(|n, _| found.iter().any(|(f, _)| f == n));
            let polling = cfg.enabled && want.load(Ordering::Relaxed);
            for (n, (host, port, _)) in &found {
                let p = known.entry(n.clone()).or_insert_with(|| Peer { name: n.clone(), ..Default::default() });
                p.host = host.clone();
                p.port = *port;
                if !polling {
                    p.sessions.clear();
                    continue;
                }
                match fetch(host, *port, &cfg.key) {
                    Ok(s) => {
                        if !p.ok {
                            log(format!("sync: connected to {n}"));
                        }
                        p.ok = true;
                        p.error = None;
                        p.sessions = s;
                    }
                    Err(e) => {
                        if p.error.as_ref() != Some(&e) {
                            log(format!("sync: {n}: {e}"));
                        }
                        p.ok = false;
                        p.error = Some(e);
                        p.sessions.clear();
                    }
                }
            }
            let mut peers: Vec<Peer> = known.values().cloned().collect();
            peers.sort_by(|a, b| a.name.cmp(&b.name));
            let st = Status { name: if own.is_empty() { display.clone() } else { own }, port, peers };
            if st != last {
                last = st.clone();
                emit(st);
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
