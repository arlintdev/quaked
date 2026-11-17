//! Mac health: CPU, memory, GPU, network, disk and uptime of this Mac (and of the paired
//! Macs, asked over sync with `ctl health`), plus the batteries of Bluetooth peripherals.
//!
//! This Mac is sampled every 2 s while the Health page shows (or a paired Mac is asking),
//! every 10 s otherwise. CPU comes from mach's host_statistics, memory from vm_stat and
//! sysctl, network from `netstat -ibn` deltas, GPU from the IOAccelerator's
//! PerformanceStatistics, disk from statfs. Peripheral batteries (ioreg + system_profiler)
//! and the Mac's own battery (pmset) are read every 5 minutes / every minute.

use crate::config::HealthSettings as Cfg;
use crate::device::log;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// Samples kept for the sparklines.
pub const HISTORY: usize = 60;
const FAST: Duration = Duration::from_secs(2);
const SLOW: Duration = Duration::from_secs(10);
/// How long a paired Mac's `ctl health` keeps this Mac sampling fast.
const POLLED_FAST: Duration = Duration::from_secs(15);
const PEER_POLL: Duration = Duration::from_secs(5);
const PERIPHERALS: Duration = Duration::from_secs(300);
const POWER: Duration = Duration::from_secs(60);

/// Recent samples, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Hist {
    /// CPU %, 0-100
    pub cpu: Vec<f32>,
    /// Memory used, 0-1
    pub mem: Vec<f32>,
    /// GPU %, 0-100
    pub gpu: Vec<f32>,
    /// Bytes/s
    pub net_in: Vec<f32>,
    pub net_out: Vec<f32>,
}

/// The Mac's own battery (laptops).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Power {
    pub percent: u8,
    pub charging: bool,
    /// On the charger (charging or full).
    pub plugged: bool,
}

/// A Bluetooth peripheral's battery. AirPods have left / right / case, the rest `main`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Battery {
    pub name: String,
    /// "keyboard", "mouse", "trackpad", "headphones" or "" (anything else).
    pub kind: String,
    pub main: Option<u8>,
    pub left: Option<u8>,
    pub right: Option<u8>,
    pub case: Option<u8>,
}

impl Battery {
    /// The lowest level it reports (the case doesn't count: it's fine to leave it low).
    pub fn lowest(&self) -> Option<u8> {
        [self.main, self.left, self.right].into_iter().flatten().min()
    }
}

/// One Mac's numbers. Also the JSON a paired Mac gets from `ctl health`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Mac {
    pub name: String,
    /// "Apple M5 Pro"
    pub chip: String,
    pub cores: u32,
    /// Has numbers (for a paired Mac: it answered).
    pub ok: bool,
    /// Why a paired Mac has no numbers.
    pub error: Option<String>,
    /// When it was sampled (unix ms).
    pub at_ms: u64,
    pub cpu: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    /// kern.memorystatus_vm_pressure_level: 1 normal, 2 warning, 4 critical.
    pub pressure: u8,
    pub gpu: Option<f32>,
    /// Bytes/s over the physical (en*) interfaces.
    pub net_in: f64,
    pub net_out: f64,
    pub disk_free: u64,
    pub disk_total: u64,
    pub uptime_s: u64,
    pub power: Option<Power>,
    pub batteries: Vec<Battery>,
    pub hist: Hist,
}

/// What the board shows: this Mac first, then the paired Macs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub macs: Vec<Mac>,
    /// Turned off in the settings.
    pub off: bool,
}

impl Status {
    /// Every peripheral battery across the Macs (each device once).
    pub fn batteries(&self) -> Vec<&Battery> {
        let mut seen = HashSet::new();
        self.macs.iter().filter(|m| m.ok).flat_map(|m| m.batteries.iter()).filter(|b| seen.insert(b.name.as_str())).collect()
    }
}

#[derive(Debug)]
pub enum Cmd {
    Configure(Cfg),
    /// The Health page was shown / left.
    PageShown(bool),
    /// The paired Macs to ask (name, host, port) and the shared key.
    Peers(Vec<(String, String, u16)>, String),
    /// A paired Mac asked for this Mac's numbers: sample quickly for a while.
    Polled,
    /// (internal) A paired Mac answered, or didn't.
    PeerReply(String, Result<Box<Mac>, String>),
}

/// What the module's thread sends the app (handled in app/health.rs on the main thread).
#[derive(Debug)]
pub enum Update {
    Status(Status),
}

/// Starts the module's background work; `emit` hands updates to the app.
pub fn spawn(cfg: Cfg, emit: impl Fn(Update) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    let back = tx.clone();
    std::thread::Builder::new().name("health".into()).spawn(move || run(cfg, rx, back, emit)).unwrap();
    tx
}

fn push(q: &mut Vec<f32>, v: f32) {
    q.push(v);
    if q.len() > HISTORY {
        q.drain(..q.len() - HISTORY);
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn run(mut cfg: Cfg, rx: Receiver<Cmd>, back: Sender<Cmd>, emit: impl Fn(Update)) {
    let mut me = Mac { name: crate::sync::computer_name(), chip: sysctl_string("machdep.cpu.brand_string").unwrap_or_default(), cores: sysctl_num::<i32>("hw.ncpu").unwrap_or(0).max(0) as u32, ok: true, ..Default::default() };
    let mut sampler = Sampler::default();
    sampler.prime();
    let mut shown = false;
    let mut polled_until = Instant::now();
    let mut peers: Vec<(String, String, u16)> = Vec::new();
    let mut key = String::new();
    let mut answers: HashMap<String, Mac> = HashMap::new();
    let mut asking: HashSet<String> = HashSet::new();
    let now = Instant::now();
    let mut next_sample = now + Duration::from_secs(1); // a first CPU / network delta
    let mut next_power = now;
    let mut next_peripherals = now;
    let mut next_peers = now;
    let mut last = Status::default();
    loop {
        let now = Instant::now();
        if cfg.enabled {
            if now >= next_power {
                me.power = power();
                next_power = now + POWER;
            }
            if now >= next_peripherals {
                me.batteries = peripherals();
                next_peripherals = now + PERIPHERALS;
            }
            if now >= next_sample {
                sampler.sample(&mut me);
                next_sample = now + if shown || now < polled_until { FAST } else { SLOW };
            }
            if shown && now >= next_peers {
                for (name, host, port) in &peers {
                    if key.is_empty() || !asking.insert(name.clone()) {
                        continue;
                    }
                    let (name, host, port, key, back) = (name.clone(), host.clone(), *port, key.clone(), back.clone());
                    std::thread::Builder::new()
                        .name("health-peer".into())
                        .spawn(move || {
                            let reply = crate::sync::request(&host, port, &key, "ctl health").and_then(|r| parse_peer(&r)).map(Box::new);
                            let _ = back.send(Cmd::PeerReply(name, reply));
                        })
                        .ok();
                }
                next_peers = now + PEER_POLL;
            }
            let mut st = Status { macs: vec![me.clone()], off: false };
            for (name, _, _) in &peers {
                st.macs.push(match answers.get(name) {
                    Some(m) => m.clone(),
                    None => Mac { name: name.clone(), error: Some("asking…".into()), ..Default::default() },
                });
            }
            if st != last {
                emit(Update::Status(st.clone()));
                last = st;
            }
        } else if !last.off {
            last = Status { off: true, ..Default::default() };
            emit(Update::Status(last.clone()));
        }
        let mut wake = next_sample.min(next_power).min(next_peripherals);
        if shown {
            wake = wake.min(next_peers);
        }
        if !cfg.enabled {
            wake = Instant::now() + Duration::from_secs(3600);
        }
        match rx.recv_timeout(wake.saturating_duration_since(Instant::now())) {
            Ok(Cmd::Configure(c)) => cfg = c,
            Ok(Cmd::PageShown(s)) => {
                if s && !shown {
                    // Fresh numbers right away, then every 2 s.
                    next_sample = next_sample.min(Instant::now() + Duration::from_millis(300));
                    next_peers = Instant::now();
                }
                shown = s;
            }
            Ok(Cmd::Peers(p, k)) => {
                if p != peers || k != key {
                    answers.retain(|name, _| p.iter().any(|(n, _, _)| n == name));
                    peers = p;
                    key = k;
                    next_peers = Instant::now();
                }
            }
            Ok(Cmd::Polled) => {
                let soon = Instant::now() + FAST;
                polled_until = Instant::now() + POLLED_FAST;
                next_sample = next_sample.min(soon);
            }
            Ok(Cmd::PeerReply(name, reply)) => {
                asking.remove(&name);
                if !peers.iter().any(|(n, _, _)| *n == name) {
                    continue;
                }
                let m = match reply {
                    Ok(m) => Mac { name: name.clone(), ok: true, error: None, ..*m },
                    Err(e) => {
                        if answers.get(&name).is_none_or(|m| m.ok) {
                            log(format!("health: {name}: {e}"));
                        }
                        Mac { name: name.clone(), error: Some(e), ..Default::default() }
                    }
                };
                answers.insert(name, m);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// A paired Mac's answer to `ctl health`.
pub fn parse_peer(reply: &[u8]) -> Result<Mac, String> {
    let text = String::from_utf8_lossy(reply);
    let text = text.trim();
    if let Some(e) = text.strip_prefix("error:") {
        return Err(if e.contains("unknown command") { "its quaked is older; update it".into() } else { e.trim().to_string() });
    }
    let m: Mac = serde_json::from_str(text).map_err(|_| "unexpected reply".to_string())?;
    if m.at_ms == 0 {
        return Err("no numbers yet".into());
    }
    Ok(m)
}

// ---------------------------------------------------------------- sampling this Mac

#[derive(Default)]
struct Sampler {
    cpu: Option<[u32; 4]>,
    net: Option<(u64, u64, Instant)>,
}

impl Sampler {
    fn prime(&mut self) {
        self.cpu = cpu_ticks();
        self.net = net_bytes().map(|(i, o)| (i, o, Instant::now()));
    }

    fn sample(&mut self, m: &mut Mac) {
        if let Some(t) = cpu_ticks() {
            if let Some(p) = self.cpu {
                m.cpu = cpu_percent(&p, &t);
            }
            self.cpu = Some(t);
        }
        m.mem_total = sysctl_num::<u64>("hw.memsize").unwrap_or(m.mem_total);
        if let Some(used) = exec("/usr/bin/vm_stat", &[]).and_then(|t| parse_vm_stat(&t)) {
            m.mem_used = used.min(m.mem_total.max(used));
        }
        m.swap_used = swap_used().unwrap_or(0);
        m.pressure = sysctl_num::<i32>("kern.memorystatus_vm_pressure_level").map(|p| p.clamp(0, 255) as u8).unwrap_or(1);
        if let Some((i, o)) = net_bytes() {
            let now = Instant::now();
            if let Some((pi, po, at)) = self.net {
                let dt = now.duration_since(at).as_secs_f64().max(0.2);
                m.net_in = i.saturating_sub(pi) as f64 / dt;
                m.net_out = o.saturating_sub(po) as f64 / dt;
            }
            self.net = Some((i, o, now));
        }
        m.gpu = exec("/usr/sbin/ioreg", &["-r", "-d", "1", "-c", "IOAccelerator"]).and_then(|t| parse_gpu(&t));
        if let Some((free, total)) = disk("/") {
            m.disk_free = free;
            m.disk_total = total;
        }
        m.uptime_s = uptime().unwrap_or(0);
        m.at_ms = now_ms();
        let round = |v: f64| ((v * 10.0).round() / 10.0) as f32;
        push(&mut m.hist.cpu, round(m.cpu as f64));
        push(&mut m.hist.mem, ((m.mem_used as f64 / m.mem_total.max(1) as f64) * 1000.0).round() as f32 / 1000.0);
        if let Some(g) = m.gpu {
            push(&mut m.hist.gpu, g);
        }
        push(&mut m.hist.net_in, m.net_in.round() as f32);
        push(&mut m.hist.net_out, m.net_out.round() as f32);
    }
}

fn exec(prog: &str, args: &[&str]) -> Option<String> {
    let mut child = Command::new(prog).args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().ok()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

// mach (libSystem): CPU ticks since boot.
extern "C" {
    fn mach_host_self() -> u32;
    fn host_statistics(host: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
}
const HOST_CPU_LOAD_INFO: i32 = 3;

/// [user, system, idle, nice] ticks, summed over all cores.
fn cpu_ticks() -> Option<[u32; 4]> {
    static HOST: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let host = *HOST.get_or_init(|| unsafe { mach_host_self() });
    let mut info = [0i32; 4];
    let mut count = 4u32;
    let kr = unsafe { host_statistics(host, HOST_CPU_LOAD_INFO, info.as_mut_ptr(), &mut count) };
    (kr == 0).then(|| info.map(|v| v as u32))
}

/// Busy share of the ticks between two readings, in percent.
fn cpu_percent(prev: &[u32; 4], now: &[u32; 4]) -> f32 {
    let d: Vec<u64> = (0..4).map(|i| now[i].wrapping_sub(prev[i]) as u64).collect();
    let total = d.iter().sum::<u64>();
    if total == 0 {
        return 0.0;
    }
    ((d[0] + d[1] + d[3]) as f64 / total as f64 * 100.0) as f32
}

fn sysctl_raw(name: &str, buf: &mut [u8]) -> Option<usize> {
    let cname = std::ffi::CString::new(name).ok()?;
    let mut len = buf.len();
    let r = unsafe { libc::sysctlbyname(cname.as_ptr(), buf.as_mut_ptr() as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) };
    (r == 0).then_some(len)
}

/// A fixed-size sysctl value (integers, plain structs).
fn sysctl_num<T: Copy>(name: &str) -> Option<T> {
    let mut buf = vec![0u8; std::mem::size_of::<T>()];
    let n = sysctl_raw(name, &mut buf)?;
    (n == buf.len()).then(|| unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const T) })
}

fn sysctl_string(name: &str) -> Option<String> {
    let mut buf = vec![0u8; 256];
    let n = sysctl_raw(name, &mut buf)?;
    let s = String::from_utf8_lossy(&buf[..n]).trim_end_matches('\0').trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// struct xsw_usage (vm.swapusage)
#[repr(C)]
#[derive(Clone, Copy)]
struct XswUsage {
    total: u64,
    avail: u64,
    used: u64,
    pagesize: u32,
    encrypted: u32,
}

fn swap_used() -> Option<u64> {
    sysctl_num::<XswUsage>("vm.swapusage").map(|x| x.used)
}

fn uptime() -> Option<u64> {
    let boot = sysctl_num::<libc::timeval>("kern.boottime")?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((now - boot.tv_sec).max(0) as u64)
}

/// (free, total) bytes on the volume holding `path`.
fn disk(path: &str) -> Option<(u64, u64)> {
    let c = std::ffi::CString::new(path).ok()?;
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let bs = st.f_bsize as u64;
    Some((st.f_bavail * bs, st.f_blocks * bs))
}

fn net_bytes() -> Option<(u64, u64)> {
    exec("/usr/sbin/netstat", &["-ibn"]).map(|t| parse_netstat(&t))
}

/// Memory in use the way Activity Monitor counts it: app memory (anonymous minus
/// purgeable) + wired + compressed.
pub fn parse_vm_stat(text: &str) -> Option<u64> {
    let page = text.lines().next().and_then(|l| l.split("page size of ").nth(1)).and_then(|r| r.split_whitespace().next()).and_then(|n| n.parse::<u64>().ok()).unwrap_or(4096);
    let mut vals: HashMap<&str, u64> = HashMap::new();
    for line in text.lines().skip(1) {
        let Some((k, v)) = line.rsplit_once(':') else { continue };
        if let Ok(n) = v.trim().trim_end_matches('.').parse::<u64>() {
            vals.insert(k.trim().trim_matches('"'), n);
        }
    }
    let get = |k: &str| vals.get(k).copied();
    let wired = get("Pages wired down")?;
    let compressed = get("Pages occupied by compressor").unwrap_or(0);
    let app = match get("Anonymous pages") {
        Some(anon) => anon.saturating_sub(get("Pages purgeable").unwrap_or(0)),
        None => get("Pages active")?,
    };
    Some((app + wired + compressed) * page)
}

/// Total bytes in / out over the physical interfaces (en*), from `netstat -ibn`'s link rows.
pub fn parse_netstat(text: &str) -> (u64, u64) {
    let (mut rx, mut tx) = (0u64, 0u64);
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 10 || !f[0].starts_with("en") || !f[2].starts_with("<Link#") {
            continue;
        }
        // The Address column is empty for some interfaces, so count from the right:
        // ... Ibytes Opkts Oerrs Obytes Coll
        let n = f.len();
        if let (Ok(i), Ok(o)) = (f[n - 5].parse::<u64>(), f[n - 2].parse::<u64>()) {
            rx += i;
            tx += o;
        }
    }
    (rx, tx)
}

/// "Device Utilization %" from the IOAccelerator (the busiest GPU, if there are several).
pub fn parse_gpu(text: &str) -> Option<f32> {
    const KEY: &str = "\"Device Utilization %\"=";
    text.match_indices(KEY)
        .filter_map(|(i, _)| {
            let rest = &text[i + KEY.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<f32>().ok()
        })
        .reduce(f32::max)
        .map(|v| v.clamp(0.0, 100.0))
}

/// The Mac's own battery from `pmset -g batt` (None on desktops).
pub fn parse_pmset(text: &str) -> Option<Power> {
    let plugged_in = text.lines().next().is_some_and(|l| l.contains("AC Power"));
    let line = text.lines().find(|l| l.contains("InternalBattery"))?;
    let pct = line.split('\t').nth(1).unwrap_or(line);
    let percent = pct.split('%').next()?.split_whitespace().last()?.parse::<u8>().ok()?;
    let state = pct.split(';').nth(1).unwrap_or("").trim();
    let charging = state == "charging" || state.starts_with("finishing");
    Some(Power { percent, charging, plugged: plugged_in })
}

fn power() -> Option<Power> {
    exec("/usr/bin/pmset", &["-g", "batt"]).and_then(|t| parse_pmset(&t))
}

fn kind_of(name: &str, minor: &str) -> String {
    let n = format!("{name} {minor}").to_lowercase();
    let k = if n.contains("keyboard") {
        "keyboard"
    } else if n.contains("trackpad") {
        "trackpad"
    } else if n.contains("mouse") {
        "mouse"
    } else if n.contains("airpods") || n.contains("headphone") || n.contains("beats") || n.contains("headset") {
        "headphones"
    } else {
        ""
    };
    k.into()
}

/// Magic Keyboard / Mouse / Trackpad from `ioreg -r -k BatteryPercent -l`: each matching
/// object (a `+-o` line) with its "Product" and "BatteryPercent".
pub fn parse_ioreg_batteries(text: &str) -> Vec<Battery> {
    let mut out: Vec<Battery> = Vec::new();
    let mut cur: (Option<String>, Option<u8>) = (None, None);
    let flush = |cur: &mut (Option<String>, Option<u8>), out: &mut Vec<Battery>| {
        if let (Some(name), Some(p)) = cur.clone() {
            if !out.iter().any(|b| b.name == name) {
                out.push(Battery { kind: kind_of(&name, ""), name, main: Some(p), ..Default::default() });
            }
        }
        *cur = (None, None);
    };
    for line in text.lines() {
        if line.contains("+-o ") {
            flush(&mut cur, &mut out);
            continue;
        }
        let l = line.trim_start().trim_start_matches('|').trim_start();
        if let Some(v) = l.strip_prefix("\"Product\" = ") {
            cur.0 = Some(v.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty());
        } else if let Some(v) = l.strip_prefix("\"BatteryPercent\" = ") {
            cur.1 = v.trim().parse::<u32>().ok().map(|p| p.min(100) as u8);
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// Connected Bluetooth devices with a battery from `system_profiler SPBluetoothDataType -json`
/// (AirPods report left / right / case).
pub fn parse_bluetooth(json: &str) -> Vec<Battery> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    let level = |p: &serde_json::Value, k: &str| p.get(k).and_then(|x| x.as_str()).and_then(|s| s.trim().trim_end_matches('%').trim().parse::<u32>().ok()).map(|n| n.min(100) as u8);
    let mut out = Vec::new();
    for ctl in v.get("SPBluetoothDataType").and_then(|x| x.as_array()).into_iter().flatten() {
        for dev in ctl.get("device_connected").and_then(|x| x.as_array()).into_iter().flatten() {
            let Some(obj) = dev.as_object() else { continue };
            for (name, p) in obj {
                let b = Battery {
                    kind: kind_of(name, p.get("device_minorType").and_then(|x| x.as_str()).unwrap_or("")),
                    name: name.clone(),
                    main: level(p, "device_batteryLevelMain").or_else(|| level(p, "device_batteryLevel")),
                    left: level(p, "device_batteryLevelLeft"),
                    right: level(p, "device_batteryLevelRight"),
                    case: level(p, "device_batteryLevelCase"),
                };
                if b.main.is_some() || b.left.is_some() || b.right.is_some() || b.case.is_some() {
                    out.push(b);
                }
            }
        }
    }
    out
}

fn peripherals() -> Vec<Battery> {
    let mut out = exec("/usr/sbin/ioreg", &["-r", "-k", "BatteryPercent", "-l", "-d", "1"]).map(|t| parse_ioreg_batteries(&t)).unwrap_or_default();
    let bt = exec("/usr/sbin/system_profiler", &["SPBluetoothDataType", "-json"]).map(|t| parse_bluetooth(&t)).unwrap_or_default();
    for b in bt {
        // ioreg's reading is fresher for the Magic devices; system_profiler adds AirPods etc.
        if !out.iter().any(|o| o.name == b.name) {
            out.push(b);
        }
    }
    out
}

// ---------------------------------------------------------------- preview

/// Sample data for `quaked preview`: this Mac mini and a paired MacBook Pro, with batteries.
pub fn sample() -> Status {
    let wave = |n: usize, base: f32, amp: f32, seed: u32| -> Vec<f32> {
        (0..n).map(|i| (base + amp * ((i as f32 * 0.37 + seed as f32).sin() * 0.6 + ((i as u32 * 7 + seed) % 11) as f32 / 11.0 * 0.8 - 0.4)).max(0.0)).collect()
    };
    let mini = Mac {
        name: "Mac mini".into(),
        chip: "Apple M5 Pro".into(),
        cores: 15,
        ok: true,
        at_ms: 1,
        cpu: 23.4,
        mem_used: 19_150_000_000,
        mem_total: 25_769_803_776,
        swap_used: 1_515_000_000,
        pressure: 1,
        gpu: Some(41.0),
        net_in: 2_480_000.0,
        net_out: 318_000.0,
        disk_free: 412_000_000_000,
        disk_total: 994_662_584_320,
        uptime_s: 3 * 86_400 + 6 * 3600 + 12 * 60,
        power: None,
        batteries: vec![
            Battery { name: "Magic Keyboard".into(), kind: "keyboard".into(), main: Some(84), ..Default::default() },
            Battery { name: "Magic Trackpad".into(), kind: "trackpad".into(), main: Some(14), ..Default::default() },
            Battery { name: "Austin's AirPods Pro".into(), kind: "headphones".into(), left: Some(92), right: Some(88), case: Some(41), ..Default::default() },
        ],
        hist: Hist {
            cpu: wave(60, 20.0, 14.0, 1),
            mem: wave(60, 0.72, 0.05, 3).into_iter().map(|v| v.min(1.0)).collect(),
            gpu: wave(60, 30.0, 22.0, 5),
            net_in: wave(60, 1_600_000.0, 1_200_000.0, 2),
            net_out: wave(60, 260_000.0, 160_000.0, 4),
        },
        ..Default::default()
    };
    let mbp = Mac {
        name: "AustinM5Max".into(),
        chip: "Apple M5 Max".into(),
        cores: 18,
        ok: true,
        at_ms: 1,
        cpu: 71.8,
        mem_used: 58_400_000_000,
        mem_total: 68_719_476_736,
        swap_used: 4_200_000_000,
        pressure: 2,
        gpu: Some(88.0),
        net_in: 84_000.0,
        net_out: 12_400_000.0,
        disk_free: 96_000_000_000,
        disk_total: 1_994_662_584_320,
        uptime_s: 11 * 86_400 + 2 * 3600,
        power: Some(Power { percent: 63, charging: true, plugged: true }),
        batteries: vec![Battery { name: "Magic Mouse".into(), kind: "mouse".into(), main: Some(7), ..Default::default() }],
        hist: Hist {
            cpu: wave(60, 62.0, 22.0, 7),
            mem: wave(60, 0.84, 0.04, 9).into_iter().map(|v| v.min(1.0)).collect(),
            gpu: wave(60, 75.0, 20.0, 11),
            net_in: wave(60, 90_000.0, 60_000.0, 6),
            net_out: wave(60, 9_000_000.0, 5_000_000.0, 8),
        },
        ..Default::default()
    };
    Status { macs: vec![mini, mbp], off: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VM_STAT: &str = "Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                    21053.
Pages active:                                 448212.
Pages inactive:                               440772.
Pages speculative:                              5445.
Pages throttled:                                   0.
Pages wired down:                             227158.
Pages purgeable:                                2156.
\"Translation faults\":                     2721602334.
Pages copy-on-write:                        94987069.
File-backed pages:                            343916.
Anonymous pages:                              550513.
Pages stored in compressor:                   839372.
Pages occupied by compressor:                 393275.
Swapouts:                                     108868.
";

    #[test]
    fn vm_stat_counts_like_activity_monitor() {
        let used = parse_vm_stat(VM_STAT).unwrap();
        assert_eq!(used, (550_513 - 2156 + 227_158 + 393_275) * 16384);
        // Old format without "Anonymous pages": active + wired + compressed, 4K pages.
        let old = "Mach Virtual Memory Statistics: (page size of 4096 bytes)\nPages active: 100.\nPages wired down: 50.\nPages occupied by compressor: 10.\n";
        assert_eq!(parse_vm_stat(old), Some(160 * 4096));
        assert_eq!(parse_vm_stat("garbage"), None);
    }

    #[test]
    fn netstat_sums_physical_link_rows() {
        let text = "Name       Mtu   Network       Address            Ipkts Ierrs     Ibytes    Opkts Oerrs     Obytes  Coll
lo0        16384 <Link#1>                      365555649     0 140555503897 365555649     0 140555503897     0
lo0        16384 127           127.0.0.1       365555649     - 140555503897 365555649     - 140555503897     -
gif0*      1280  <Link#2>                             0     0          0        0     0          0     0
en0        1500  <Link#7>    bc:74:ea:e0:ea:be 16662449     0 12410669327 12786992     0 7391053751     0
en0        1500  192.168.68/22 192.168.68.71   16662449     - 12410669327 12786992     - 7391053751     -
en1        1500  <Link#15>   bc:74:ea:d3:72:a0   760709     0  167076019   131602     0   20413169     0
en9        1500  <Link#30>                          10     0       1000        5     0        500     0
llw0       1500  <Link#17>   ae:8d:77:cb:3c:e5 14443077     0 21551697642   482503     0   53357648     0
utun4      1380  <Link#26>                           44     0       7476       27     0       4151     0
";
        assert_eq!(parse_netstat(text), (12_410_669_327 + 167_076_019 + 1000, 7_391_053_751 + 20_413_169 + 500));
        assert_eq!(parse_netstat(""), (0, 0));
    }

    #[test]
    fn gpu_utilization_from_ioreg() {
        let text = r#"+-o AGXAcceleratorG17X  <class AGXAcceleratorG17X, id 0x1000003c2>
    {
      "PerformanceStatistics" = {"In use system memory (driver)"=0,"Tiler Utilization %"=17,"Renderer Utilization %"=64,"Device Utilization %"=67,"SplitSceneCount"=0}
      "model" = "Apple M5 Pro"
    }"#;
        assert_eq!(parse_gpu(text), Some(67.0));
        let two = format!("{text}\n{}", text.replace("=67", "=12"));
        assert_eq!(parse_gpu(&two), Some(67.0));
        assert_eq!(parse_gpu("nothing here"), None);
    }

    #[test]
    fn cpu_percent_from_tick_deltas() {
        assert_eq!(cpu_percent(&[100, 100, 100, 0], &[130, 110, 160, 0]), 40.0);
        assert_eq!(cpu_percent(&[1, 1, 1, 1], &[1, 1, 1, 1]), 0.0);
        // Counters wrap.
        assert_eq!(cpu_percent(&[u32::MAX, 0, 0, 0], &[9, 0, 10, 0]), 50.0);
    }

    #[test]
    fn ioreg_peripheral_batteries() {
        let text = r#"+-o AppleDeviceManagementHIDEventService  <class AppleDeviceManagementHIDEventService, id 0x100000c2e, registered>
    {
      "LowBatteryNotificationPercentage" = 2
      "ProductID" = 613
      "Product" = "Magic Trackpad"
      "Transport" = "Bluetooth"
      "BatteryPercent" = 64
      "BatteryStatusFlags" = 0
    }

+-o AppleDeviceManagementHIDEventService  <class AppleDeviceManagementHIDEventService, id 0x100000c40, registered>
    {
      "Product" = "Magic Keyboard with Touch ID"
      "BatteryPercent" = 100
    }
+-o AppleDeviceManagementHIDEventService  <class AppleDeviceManagementHIDEventService, id 0x100000c41, registered>
    {
      "Product" = "Magic Trackpad"
      "BatteryPercent" = 64
    }
+-o Nameless  <class X>
    {
      "BatteryPercent" = 5
    }
"#;
        let b = parse_ioreg_batteries(text);
        assert_eq!(b.len(), 2);
        assert_eq!((b[0].name.as_str(), b[0].kind.as_str(), b[0].main), ("Magic Trackpad", "trackpad", Some(64)));
        assert_eq!((b[1].name.as_str(), b[1].kind.as_str(), b[1].main), ("Magic Keyboard with Touch ID", "keyboard", Some(100)));
        assert!(parse_ioreg_batteries("").is_empty());
    }

    #[test]
    fn bluetooth_airpods_and_others() {
        let json = r#"{"SPBluetoothDataType":[{"controller_properties":{},
          "device_connected":[
            {"Austin's AirPods Pro":{"device_address":"x","device_batteryLevelCase":"41%","device_batteryLevelLeft":"92%","device_batteryLevelRight":"88%","device_minorType":"Headphones"}},
            {"Magic Mouse":{"device_batteryLevelMain":"7%","device_minorType":"Mouse"}},
            {"Austin's iPhone":{"device_minorType":"Smartphone"}}
          ],
          "device_not_connected":[{"Old AirPods":{"device_batteryLevelLeft":"50%"}}]}]}"#;
        let b = parse_bluetooth(json);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].name, "Austin's AirPods Pro");
        assert_eq!(b[0].kind, "headphones");
        assert_eq!((b[0].left, b[0].right, b[0].case, b[0].main), (Some(92), Some(88), Some(41), None));
        assert_eq!(b[0].lowest(), Some(88));
        assert_eq!((b[1].kind.as_str(), b[1].main), ("mouse", Some(7)));
        assert!(parse_bluetooth("not json").is_empty());
        assert!(parse_bluetooth(r#"{"SPBluetoothDataType":[{"device_not_connected":[]}]}"#).is_empty());
    }

    #[test]
    fn pmset_battery() {
        let laptop = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=24117347)\t85%; discharging; 4:12 remaining present: true\n";
        assert_eq!(parse_pmset(laptop), Some(Power { percent: 85, charging: false, plugged: false }));
        let charging = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=24117347)\t63%; charging; 1:02 remaining present: true\n";
        assert_eq!(parse_pmset(charging), Some(Power { percent: 63, charging: true, plugged: true }));
        let held = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=24117347)\t80%; AC attached; not charging present: true\n";
        assert_eq!(parse_pmset(held), Some(Power { percent: 80, charging: false, plugged: true }));
        assert_eq!(parse_pmset("Now drawing from 'AC Power'\n"), None);
    }

    #[test]
    fn peer_replies() {
        let m = sample().macs.remove(0);
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"memUsed\""));
        assert_eq!(parse_peer(format!("{json}\n").as_bytes()), Ok(m));
        assert_eq!(parse_peer(b"error: unknown command \"health\"\n"), Err("its quaked is older; update it".into()));
        assert_eq!(parse_peer(b"error: health is off there\n"), Err("health is off there".into()));
        assert!(parse_peer(b"{}").is_err());
        assert!(parse_peer(b"<html>").is_err());
    }

    #[test]
    fn history_is_capped() {
        let mut q = Vec::new();
        for i in 0..100 {
            push(&mut q, i as f32);
        }
        assert_eq!(q.len(), HISTORY);
        assert_eq!(q[0], 40.0);
    }

    #[test]
    fn batteries_once_across_macs() {
        let mut st = sample();
        let dup = st.macs[0].batteries[0].clone();
        st.macs[1].batteries.push(dup);
        let names: Vec<&str> = st.batteries().iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["Magic Keyboard", "Magic Trackpad", "Austin's AirPods Pro", "Magic Mouse"]);
    }

    #[test]
    fn live_sample_of_this_mac() {
        // Two read-only samples of this Mac (vm_stat, netstat, ioreg, sysctl, statfs).
        let mut m = Mac::default();
        let mut s = Sampler::default();
        s.prime();
        std::thread::sleep(Duration::from_millis(300));
        s.sample(&mut m);
        assert!((0.0..=100.0).contains(&m.cpu), "{}", m.cpu);
        assert!(m.mem_total > 0 && m.mem_used > 0 && m.mem_used <= m.mem_total, "{} / {}", m.mem_used, m.mem_total);
        assert!(m.net_in >= 0.0 && m.net_out >= 0.0);
        assert!(m.disk_total > 0 && m.uptime_s > 0 && m.at_ms > 0);
        assert!(m.pressure >= 1);
        assert_eq!(m.hist.cpu.len(), 1);
        eprintln!("cpu {:.1}%  mem {}/{}  swap {}  pressure {}  gpu {:?}  net {:.0}/{:.0} B/s  disk {}/{}  up {}s  power {:?}", m.cpu, m.mem_used, m.mem_total, m.swap_used, m.pressure, m.gpu, m.net_in, m.net_out, m.disk_free, m.disk_total, m.uptime_s, power());
    }

    #[test]
    fn thread_samples_and_reports_unreachable_peers() {
        let (tx, rx) = std::sync::mpsc::channel();
        let cmd = spawn(Cfg::default(), move |u| {
            let _ = tx.send(u);
        });
        cmd.send(Cmd::PageShown(true)).unwrap();
        // A paired Mac nobody answers for (port 1 on this Mac refuses).
        cmd.send(Cmd::Peers(vec![("Ghost".into(), "127.0.0.1".into(), 1)], "k".into())).unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut seen: Option<Status> = None;
        while Instant::now() < deadline {
            let Ok(Update::Status(st)) = rx.recv_timeout(Duration::from_millis(500)) else { continue };
            let ghost_failed = st.macs.get(1).is_some_and(|g| g.name == "Ghost" && !g.ok && g.error.as_deref().is_some_and(|e| !e.ends_with('…')));
            if st.macs[0].hist.cpu.len() >= 2 && ghost_failed {
                seen = Some(st);
                break;
            }
        }
        let st = seen.expect("two samples and the peer's failure within 8 s");
        assert!(st.macs[0].ok && st.macs[0].at_ms > 0 && !st.macs[0].name.is_empty());
        // Dropping the peer removes its column.
        cmd.send(Cmd::Peers(vec![], "k".into())).unwrap();
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut gone = false;
        while Instant::now() < deadline && !gone {
            if let Ok(Update::Status(st)) = rx.recv_timeout(Duration::from_millis(500)) {
                gone = st.macs.len() == 1;
            }
        }
        assert!(gone);
    }

    #[test]
    fn live_readers_answer() {
        // Read-only calls against this Mac: they should all produce something sane.
        assert!(cpu_ticks().is_some());
        assert!(sysctl_num::<u64>("hw.memsize").unwrap_or(0) > 1 << 30);
        assert!(uptime().unwrap_or(0) > 0);
        let (free, total) = disk("/").unwrap();
        assert!(free > 0 && total >= free);
        assert!(swap_used().is_some());
    }
}
