//! OPS page data: network (Cloudflare WARP, AppGate, public IP / colo), your servers
//! (reachability, and load / memory / disk / GPU over SSH), and your sites (HTTP status,
//! response time, TLS expiry).

use crate::config::Ops as OpsCfg;
use crate::device::log;
use std::net::{TcpStream, ToSocketAddrs};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

const WARP_CLI: &str = "/usr/local/bin/warp-cli";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Network {
    /// Some(true/false) when warp-cli answers.
    pub warp: Option<bool>,
    pub appgate: bool,
    pub ip: String,
    pub colo: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Host {
    pub name: String,
    pub address: String,
    pub up: bool,
    pub latency_ms: Option<u32>,
    pub load: Option<f32>,
    pub cpus: Option<u32>,
    /// (used, total) bytes
    pub mem: Option<(u64, u64)>,
    pub disk: Option<(u64, u64)>,
    pub uptime: String,
    /// (utilization %, memory used MiB, memory total MiB, temperature C)
    pub gpu: Option<(f32, f32, f32, f32)>,
    pub stats_error: Option<String>,
    /// Why the host counts as down ("No route to host", "needs cloudflared").
    pub reason: Option<String>,
    /// Reached through a ProxyCommand / ProxyJump, so checked over SSH instead of TCP.
    pub proxied: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Site {
    pub url: String,
    pub code: Option<u16>,
    pub ms: Option<u32>,
    pub tls_days: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub net: Network,
    pub hosts: Vec<Host>,
    pub sites: Vec<Site>,
}

#[derive(Debug)]
pub enum Cmd {
    Configure(OpsCfg),
    SetWarp(bool),
}

fn run_cmd(prog: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(prog).args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(text)
}

fn network() -> Network {
    let warp = run_cmd(WARP_CLI, &["--accept-tos", "status"], Duration::from_secs(4)).map(|s| s.contains("Connected") && !s.contains("Disconnected"));
    let appgate = run_cmd("/usr/bin/pgrep", &["-f", "AppGate SDP"], Duration::from_secs(2)).is_some_and(|s| !s.trim().is_empty());
    let mut net = Network { warp, appgate, ..Default::default() };
    if let Some(trace) = run_cmd("/usr/bin/curl", &["-s", "--max-time", "4", "https://1.1.1.1/cdn-cgi/trace"], Duration::from_secs(5)) {
        for line in trace.lines() {
            if let Some(v) = line.strip_prefix("ip=") {
                net.ip = v.to_string();
            } else if let Some(v) = line.strip_prefix("colo=") {
                net.colo = v.to_string();
            } else if let Some(v) = line.strip_prefix("warp=") {
                if net.warp.is_none() {
                    net.warp = Some(v != "off");
                }
            }
        }
    }
    net
}

/// Where ssh would go for an alias.
#[derive(Debug, Clone, PartialEq)]
struct Target {
    alias: String,
    host: String,
    port: u16,
    /// The ProxyCommand's program (or "ssh" for ProxyJump), if any.
    proxy: Option<String>,
}

fn resolve(alias: &str) -> Target {
    let out = run_cmd("/usr/bin/ssh", &["-G", alias], Duration::from_secs(3)).unwrap_or_default();
    let mut t = Target { alias: alias.to_string(), host: alias.to_string(), port: 22, proxy: None };
    for line in out.lines() {
        if let Some(h) = line.strip_prefix("hostname ") {
            t.host = h.trim().to_string();
        } else if let Some(p) = line.strip_prefix("port ") {
            t.port = p.trim().parse().unwrap_or(22);
        } else if let Some(cmd) = line.strip_prefix("proxycommand ") {
            t.proxy = cmd.split_whitespace().next().map(str::to_string);
        } else if line.starts_with("proxyjump ") {
            t.proxy = Some("ssh".into());
        }
    }
    t
}

/// Is a ProxyCommand's program installed? Checks the places launchd's PATH misses too.
fn program_exists(prog: &str) -> bool {
    if prog.contains('/') {
        return std::path::Path::new(prog).exists();
    }
    let path = std::env::var("PATH").unwrap_or_default();
    let found = path.split(':').chain(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"]).any(|d| std::path::Path::new(d).join(prog).exists());
    found
}

fn tcp_latency(host: &str, port: u16) -> Result<u32, String> {
    let addr = (host, port).to_socket_addrs().map_err(|_| "can't resolve".to_string())?.next().ok_or("can't resolve")?;
    let t = Instant::now();
    match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
        Ok(_) => Ok(t.elapsed().as_millis().max(1) as u32),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Err("no answer".into()),
        Err(e) => {
            // Drop the " (os error 65)" suffix.
            let msg = e.to_string();
            Err(msg.split(" (os error").next().unwrap_or(&msg).to_string())
        }
    }
}

const STATS_SCRIPT: &str = r#"cat /proc/loadavg 2>/dev/null | cut -d' ' -f1; nproc 2>/dev/null; free -b 2>/dev/null | awk '/Mem:/{print $3, $2}'; df -B1 / 2>/dev/null | awk 'NR==2{print $3, $2}'; uptime -p 2>/dev/null; nvidia-smi --query-gpu=utilization.gpu,memory.used,memory.total,temperature.gpu --format=csv,noheader,nounits 2>/dev/null | head -1"#;

/// Reads load / memory / disk / GPU over SSH. Returns the round trip on success.
fn ssh_stats(alias: &str, h: &mut Host) -> Option<u32> {
    let started = Instant::now();
    let control = format!("ControlPath={}/ssh-%C", crate::status::dir().parent().map(|p| p.display().to_string()).unwrap_or_else(|| "/tmp".into()));
    let args = [
        "-o", "BatchMode=yes", "-o", "ConnectTimeout=4", "-o", "ControlMaster=auto", "-o", &control, "-o", "ControlPersist=10m", "-o", "LogLevel=ERROR", alias, STATS_SCRIPT,
    ];
    let Some(out) = run_cmd("/usr/bin/ssh", &args, Duration::from_secs(8)) else {
        h.stats_error = Some("ssh timed out".into());
        return None;
    };
    let lines: Vec<&str> = out.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("Pseudo-terminal")).collect();
    if let Some(bad) = lines.iter().find(|l| l.contains("Permission denied") || l.contains("Host key") || l.starts_with("ssh:") || l.contains("command not found") || l.contains("Connection closed") || l.contains("kex_exchange")) {
        h.stats_error = Some(crate::status::squash(bad, 60));
        return None;
    }
    if lines.is_empty() {
        h.stats_error = Some("no output".into());
        return None;
    }
    let rtt = started.elapsed().as_millis().max(1) as u32;
    h.stats_error = None;
    let pair = |l: &str| -> Option<(u64, u64)> {
        let mut it = l.split_whitespace().map(|x| x.parse::<u64>().ok());
        Some((it.next()??, it.next()??))
    };
    let mut i = 0;
    let next = |i: &mut usize| -> Option<&str> {
        let v = lines.get(*i).copied();
        *i += 1;
        v
    };
    h.load = next(&mut i).and_then(|l| l.parse().ok());
    h.cpus = next(&mut i).and_then(|l| l.parse().ok());
    h.mem = next(&mut i).and_then(pair);
    h.disk = next(&mut i).and_then(pair);
    h.uptime = next(&mut i).map(|l| l.trim_start_matches("up ").to_string()).unwrap_or_default();
    h.gpu = next(&mut i).and_then(|l| {
        let v: Vec<f32> = l.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        (v.len() == 4).then(|| (v[0], v[1], v[2], v[3]))
    });
    Some(rtt)
}

/// "Feb  9 23:59:59 2027 GMT" -> days from now.
fn days_until(expire: &str) -> Option<i64> {
    let f: Vec<&str> = expire.split_whitespace().collect();
    if f.len() < 4 {
        return None;
    }
    let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].iter().position(|m| *m == f[0])? as i64 + 1;
    let day: i64 = f[1].parse().ok()?;
    let year: i64 = f[3].parse().ok()?;
    // days since 1970-01-01 (civil from days, inverse)
    let (y, m) = if month <= 2 { (year - 1, month + 9) } else { (year, month - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64 / 86_400;
    Some(days - now)
}

fn check_site(url: &str) -> Site {
    let mut site = Site { url: url.to_string(), ..Default::default() };
    let Some(out) = run_cmd("/usr/bin/curl", &["-s", "-v", "-o", "/dev/null", "-L", "--max-time", "8", "-w", "\nQUAKED %{http_code} %{time_total}\n", url], Duration::from_secs(10)) else {
        site.error = Some("timed out".into());
        return site;
    };
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("QUAKED ") {
            let mut it = rest.split_whitespace();
            site.code = it.next().and_then(|c| c.parse().ok()).filter(|&c| c > 0);
            site.ms = it.next().and_then(|t| t.parse::<f64>().ok()).map(|t| (t * 1000.0) as u32);
        } else if let Some(exp) = line.trim_start_matches(['*', ' ']).strip_prefix("expire date: ") {
            site.tls_days = days_until(exp);
        } else if line.contains("Could not resolve host") || line.contains("Failed to connect") || line.contains("SSL certificate problem") {
            site.error = Some(crate::status::squash(line.trim_start_matches(['*', ' ']), 60));
        }
    }
    if site.code.is_none() && site.error.is_none() {
        site.error = Some("no response".into());
    }
    site
}

pub fn spawn(cfg: OpsCfg, fast: Arc<AtomicBool>, emit: impl Fn(Status) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("ops".into()).spawn(move || run(cfg, fast, rx, emit)).unwrap();
    tx
}

fn run(mut cfg: OpsCfg, fast: Arc<AtomicBool>, rx: Receiver<Cmd>, emit: impl Fn(Status)) {
    let mut status = Status::default();
    let mut published = Status { net: Network { ip: "-".into(), ..Default::default() }, ..Default::default() };
    let (mut next_net, mut next_hosts, mut next_stats, mut next_sites) = (Instant::now(), Instant::now(), Instant::now(), Instant::now());
    let mut resolved: Vec<Target> = Vec::new();
    // Proxied hosts are only checked over SSH; back off while that keeps failing, so a
    // broken proxy (or one that wants a browser login) isn't retried every minute.
    let mut proxy_retry: Vec<(Instant, u32)> = Vec::new();
    loop {
        let now = Instant::now();
        // Slow down when the page isn't on screen.
        let k = if fast.load(Ordering::Relaxed) { 1 } else { 3 };
        if now >= next_net {
            status.net = network();
            next_net = now + Duration::from_secs(15 * k);
        }
        if resolved.len() != cfg.hosts.len() || resolved.iter().zip(&cfg.hosts).any(|(r, h)| &r.alias != h) {
            resolved = cfg.hosts.iter().map(|h| resolve(h)).collect();
            status.hosts = resolved
                .iter()
                .map(|t| Host { name: t.alias.clone(), address: t.host.clone(), proxied: t.proxy.is_some(), ..Default::default() })
                .collect();
            proxy_retry = vec![(now, 0); resolved.len()];
            next_hosts = now;
            next_stats = now;
        }
        if now >= next_hosts {
            for (h, t) in status.hosts.iter_mut().zip(&resolved) {
                match &t.proxy {
                    Some(prog) if !program_exists(prog) => {
                        h.up = false;
                        h.latency_ms = None;
                        h.reason = Some(format!("needs {prog} (not installed)"));
                    }
                    Some(_) => {} // decided by the SSH check below
                    None => match tcp_latency(&t.host, t.port) {
                        Ok(ms) => {
                            h.up = true;
                            h.latency_ms = Some(ms);
                            h.reason = None;
                        }
                        Err(e) => {
                            h.up = false;
                            h.latency_ms = None;
                            h.reason = Some(e);
                        }
                    },
                }
            }
            next_hosts = now + Duration::from_secs(20 * k);
        }
        let stats_due = now >= next_stats;
        for (i, (h, t)) in status.hosts.iter_mut().zip(&resolved).enumerate() {
            let proxied = t.proxy.as_deref().is_some_and(program_exists);
            if proxied {
                let (at, fails) = proxy_retry[i];
                if now < at {
                    continue;
                }
                let rtt = ssh_stats(&t.alias, h);
                h.up = rtt.is_some();
                h.latency_ms = rtt;
                h.reason = h.stats_error.clone();
                let fails = if rtt.is_some() { 0 } else { fails + 1 };
                let wait = if fails == 0 { 60 * k } else { (120u64 << fails.min(4)).min(1800) };
                proxy_retry[i] = (Instant::now() + Duration::from_secs(wait), fails);
            } else if stats_due && cfg.ssh_stats && h.up {
                ssh_stats(&t.alias, h);
            }
        }
        if stats_due {
            next_stats = now + Duration::from_secs(60 * k);
        }
        if now >= next_sites {
            status.sites = cfg.sites.iter().map(|u| check_site(u)).collect();
            next_sites = now + Duration::from_secs(60 * k);
        }
        if status != published {
            published = status.clone();
            emit(status.clone());
        }
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Cmd::Configure(c)) => {
                if c.sites != cfg.sites {
                    next_sites = Instant::now();
                }
                cfg = c;
            }
            Ok(Cmd::SetWarp(on)) => {
                let arg = if on { "connect" } else { "disconnect" };
                match run_cmd(WARP_CLI, &["--accept-tos", arg], Duration::from_secs(10)) {
                    Some(out) => log(format!("ops: warp {arg}: {}", crate::status::squash(&out, 80))),
                    None => log(format!("ops: warp {arg} timed out")),
                }
                next_net = Instant::now() + Duration::from_secs(2);
                // The board shows the new state optimistically; re-send even if nothing changed.
                published = Status::default();
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
    fn tls_expiry_days() {
        let d = days_until("Feb  9 23:59:59 2030 GMT").unwrap();
        assert!(d > 365 && d < 3650, "{d}");
        assert_eq!(days_until("garbage"), None);
    }
}
