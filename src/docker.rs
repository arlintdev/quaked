//! Containers through the Docker Engine API on a Unix socket. Kiri serves that API at
//! ~/.kiri/run/docker.sock (it has no other API), so it's the first choice; other
//! Docker-compatible engines work the same way.

use crate::config::{self, Docker as DockerCfg};
use crate::device::log;
use crate::http;
use serde_json::Value;
use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SOCKETS: [&str; 4] = ["~/.kiri/run/docker.sock", "/var/run/docker.sock", "~/.docker/run/docker.sock", "~/.rd/docker.sock"];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    /// running, paused, exited, restarting, created, dead
    pub state: String,
    /// "Up 2 days", "Exited (0) 3 hours ago"
    pub status: String,
    pub ports: Vec<String>,
    pub cpu: Option<f32>,
    pub mem: Option<u64>,
    pub mem_limit: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub ok: bool,
    pub error: Option<String>,
    pub socket: String,
    pub engine: String,
    pub ncpu: u64,
    pub mem_total: u64,
    pub images: usize,
    pub volumes: usize,
    pub containers: Vec<Container>,
    /// (container id, last log lines)
    pub logs: Option<(String, Vec<String>)>,
}

#[derive(Debug)]
pub enum Cmd {
    Logs(String),
    /// (container id, "start" | "stop" | "restart" | "pause" | "unpause")
    Action(String, &'static str),
    Configure(DockerCfg),
}

fn socket_path(cfg: &DockerCfg) -> Option<PathBuf> {
    if !cfg.socket.is_empty() {
        return Some(config::expand(&cfg.socket));
    }
    SOCKETS.iter().map(|s| config::expand(s)).find(|p| p.exists())
}

fn call(sock: &PathBuf, method: &str, path: &str) -> Result<http::Response, String> {
    let mut s = UnixStream::connect(sock).map_err(|e| e.to_string())?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(6)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(3)));
    http::request(&mut s, method, "docker", path, None, None).map_err(|e| e.to_string())
}

fn get(sock: &PathBuf, path: &str) -> Result<Value, String> {
    let r = call(sock, "GET", path)?;
    if !r.ok() {
        return Err(format!("{path}: HTTP {}", r.status));
    }
    r.json().ok_or_else(|| format!("{path}: not JSON"))
}

/// Docker multiplexes stdout/stderr as [stream, 0, 0, 0, len_be32] frames unless the
/// container has a TTY.
fn demux(raw: &[u8]) -> String {
    let framed = raw.len() >= 8 && raw[0] <= 2 && raw[1..4] == [0, 0, 0];
    if !framed {
        return String::from_utf8_lossy(raw).into_owned();
    }
    let mut out = Vec::new();
    let mut b = raw;
    while b.len() >= 8 {
        let n = u32::from_be_bytes([b[4], b[5], b[6], b[7]]) as usize;
        let end = (8 + n).min(b.len());
        out.extend_from_slice(&b[8..end]);
        b = &b[end..];
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if c != '\r' {
            out.push(c);
        }
    }
    out
}

pub fn spawn(cfg: DockerCfg, fast: Arc<AtomicBool>, emit: impl Fn(Status) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("docker".into()).spawn(move || run(cfg, fast, rx, emit)).unwrap();
    tx
}

fn run(mut cfg: DockerCfg, fast: Arc<AtomicBool>, rx: Receiver<Cmd>, emit: impl Fn(Status)) {
    let mut status = Status::default();
    let mut published = Status { ok: true, ..Default::default() };
    let mut prev_cpu: HashMap<String, (u64, u64)> = HashMap::new();
    let mut next_counts = Instant::now();
    let mut last_error_logged = String::new();
    loop {
        let Some(sock) = socket_path(&cfg) else {
            status = Status { error: Some("No Docker socket found (is Kiri running?)".into()), ..Default::default() };
            publish(&mut published, &status, &emit);
            if wait(&rx, &mut cfg, Duration::from_secs(5), &mut status).is_err() {
                return;
            }
            continue;
        };
        status.socket = sock.display().to_string().replace(&std::env::var("HOME").unwrap_or_default(), "~");
        match poll(&sock, &mut status, &mut prev_cpu, &mut next_counts) {
            Ok(()) => {
                status.ok = true;
                status.error = None;
                last_error_logged.clear();
            }
            Err(e) => {
                if e != last_error_logged {
                    log(format!("docker: {e}"));
                    last_error_logged = e.clone();
                }
                status.ok = false;
                status.error = Some(e);
                status.containers.clear();
            }
        }
        publish(&mut published, &status, &emit);
        let period = if fast.load(Ordering::Relaxed) { Duration::from_secs(2) } else { Duration::from_secs(15) };
        loop {
            let started = Instant::now();
            match wait(&rx, &mut cfg, period, &mut status) {
                Err(()) => return,
                Ok(Some((cmd, sock_cmd))) => {
                    handle(&sock, cmd, sock_cmd, &mut status);
                    publish(&mut published, &status, &emit);
                    // An action changes container state: refresh soon.
                    break;
                }
                Ok(None) if started.elapsed() >= period || fast.load(Ordering::Relaxed) => break,
                Ok(None) => {}
            }
        }
    }
}

enum Pending {
    Logs,
    Action(&'static str),
}

/// Waits for a command; Configure is applied here. Err = channel closed.
fn wait(rx: &Receiver<Cmd>, cfg: &mut DockerCfg, period: Duration, _status: &mut Status) -> Result<Option<(String, Pending)>, ()> {
    match rx.recv_timeout(period) {
        Ok(Cmd::Configure(c)) => {
            *cfg = c;
            Ok(None)
        }
        Ok(Cmd::Logs(id)) => Ok(Some((id, Pending::Logs))),
        Ok(Cmd::Action(id, a)) => Ok(Some((id, Pending::Action(a)))),
        Err(RecvTimeoutError::Timeout) => Ok(None),
        Err(RecvTimeoutError::Disconnected) => Err(()),
    }
}

fn handle(sock: &PathBuf, id: String, what: Pending, status: &mut Status) {
    match what {
        Pending::Logs => match call(sock, "GET", &format!("/containers/{id}/logs?stdout=1&stderr=1&tail=200")) {
            Ok(r) if r.ok() => {
                let text = strip_ansi(&demux(&r.body));
                let lines: Vec<String> = text.lines().map(|l| l.to_string()).filter(|l| !l.trim().is_empty()).collect();
                status.logs = Some((id, lines));
            }
            Ok(r) => status.logs = Some((id, vec![format!("(logs: HTTP {})", r.status)])),
            Err(e) => status.logs = Some((id, vec![format!("(logs: {e})")])),
        },
        Pending::Action(a) => {
            let q = if a == "stop" || a == "restart" { "?t=10" } else { "" };
            match call(sock, "POST", &format!("/containers/{id}/{a}{q}")) {
                Ok(r) if r.ok() || r.status == 304 => log(format!("docker: {a} {}", &id[..12.min(id.len())])),
                Ok(r) => log(format!("docker: {a} -> HTTP {} {}", r.status, String::from_utf8_lossy(&r.body).chars().take(120).collect::<String>())),
                Err(e) => log(format!("docker: {a} failed: {e}")),
            }
        }
    }
}

fn poll(sock: &PathBuf, status: &mut Status, prev_cpu: &mut HashMap<String, (u64, u64)>, next_counts: &mut Instant) -> Result<(), String> {
    let info = get(sock, "/info")?;
    let os = info.get("OperatingSystem").and_then(Value::as_str).unwrap_or("Docker");
    let ver = info.get("ServerVersion").and_then(Value::as_str).unwrap_or("");
    status.engine = format!("{os} {ver}").trim().to_string();
    status.ncpu = info.get("NCPU").and_then(Value::as_u64).unwrap_or(0);
    status.mem_total = info.get("MemTotal").and_then(Value::as_u64).unwrap_or(0);
    if Instant::now() >= *next_counts {
        *next_counts = Instant::now() + Duration::from_secs(30);
        status.images = get(sock, "/images/json").ok().and_then(|v| v.as_array().map(Vec::len)).unwrap_or(0);
        status.volumes = get(sock, "/volumes").ok().and_then(|v| v.get("Volumes").and_then(Value::as_array).map(Vec::len)).unwrap_or(0);
    }
    let list = get(sock, "/containers/json?all=1")?;
    let mut containers = Vec::new();
    for c in list.as_array().cloned().unwrap_or_default() {
        let id = c.get("Id").and_then(Value::as_str).unwrap_or("").to_string();
        let name = c.get("Names").and_then(|n| n.get(0)).and_then(Value::as_str).unwrap_or("").trim_start_matches('/').to_string();
        let mut ports: Vec<String> = c
            .get("Ports")
            .and_then(Value::as_array)
            .map(|ps| {
                ps.iter()
                    .filter_map(|p| {
                        let private = p.get("PrivatePort").and_then(Value::as_u64)?;
                        Some(match p.get("PublicPort").and_then(Value::as_u64) {
                            Some(public) => format!("{public}→{private}"),
                            None => format!("{private}"),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        ports.sort();
        ports.dedup();
        let mut ct = Container {
            name,
            image: c.get("Image").and_then(Value::as_str).unwrap_or("").split('@').next().unwrap_or("").to_string(),
            state: c.get("State").and_then(Value::as_str).unwrap_or("").to_string(),
            status: c.get("Status").and_then(Value::as_str).unwrap_or("").to_string(),
            ports,
            id,
            ..Default::default()
        };
        if ct.state == "running" {
            if let Ok(s) = get(sock, &format!("/containers/{}/stats?stream=false&one-shot=true", ct.id)) {
                let total = s.pointer("/cpu_stats/cpu_usage/total_usage").and_then(Value::as_u64).unwrap_or(0);
                let system = s.pointer("/cpu_stats/system_cpu_usage").and_then(Value::as_u64).unwrap_or(0);
                let cpus = s.pointer("/cpu_stats/online_cpus").and_then(Value::as_u64).unwrap_or(1).max(1);
                if let Some((pt, ps)) = prev_cpu.get(&ct.id) {
                    let (dt, ds) = (total.saturating_sub(*pt), system.saturating_sub(*ps));
                    if ds > 0 {
                        ct.cpu = Some(dt as f32 / ds as f32 * cpus as f32 * 100.0);
                    }
                }
                prev_cpu.insert(ct.id.clone(), (total, system));
                let usage = s.pointer("/memory_stats/usage").and_then(Value::as_u64);
                let inactive = s.pointer("/memory_stats/stats/inactive_file").and_then(Value::as_u64).or_else(|| s.pointer("/memory_stats/stats/cache").and_then(Value::as_u64)).unwrap_or(0);
                ct.mem = usage.map(|u| u.saturating_sub(inactive));
                ct.mem_limit = s.pointer("/memory_stats/limit").and_then(Value::as_u64);
            }
        }
        containers.push(ct);
    }
    // Running first, then by name.
    containers.sort_by(|a, b| (a.state != "running").cmp(&(b.state != "running")).then(a.name.cmp(&b.name)));
    prev_cpu.retain(|id, _| containers.iter().any(|c| &c.id == id));
    status.containers = containers;
    Ok(())
}

fn publish(published: &mut Status, status: &Status, emit: &impl Fn(Status)) {
    if status != published {
        *published = status.clone();
        emit(status.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demuxes_docker_log_frames() {
        let mut raw = vec![1, 0, 0, 0, 0, 0, 0, 6];
        raw.extend(b"hello\n");
        raw.extend([2, 0, 0, 0, 0, 0, 0, 4]);
        raw.extend(b"err\n");
        assert_eq!(demux(&raw), "hello\nerr\n");
        assert_eq!(demux(b"plain tty output\n"), "plain tty output\n");
        assert_eq!(strip_ansi("\u{1b}[32mok\u{1b}[0m\r"), "ok");
    }
}
