//! League of Legends client (LCU API on 127.0.0.1, self-signed TLS, Basic auth with the
//! client's per-launch token). Shows the game phase, and accepts the ready check when
//! auto-accept is on. Works with live and PBE installs.

use crate::config::League as LeagueCfg;
use crate::device::log;
use crate::http;
use serde_json::Value;
use std::collections::HashMap;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReadyCheck {
    /// "InProgress", "EveryoneReady", "StrangerNotReady", "PartyNotReady"...
    pub state: String,
    /// "None" (not answered yet), "Accepted" or "Declined".
    pub response: String,
    pub timer: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChampSelect {
    pub phase: String,
    pub time_left_ms: f64,
    pub position: String,
    pub champion: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub running: bool,
    /// LCU gameflow phase: None, Lobby, Matchmaking, ReadyCheck, ChampSelect, InProgress...
    pub phase: String,
    pub summoner: Option<(String, u32)>,
    pub queue: Option<String>,
    /// (seconds in queue, estimated seconds)
    pub search: Option<(f64, f64)>,
    pub ready: Option<ReadyCheck>,
    pub champ: Option<ChampSelect>,
    pub accepted_by_quaked: bool,
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum Cmd {
    Accept,
    Decline,
    FindMatch,
    CancelSearch,
    Configure(LeagueCfg),
}

pub enum Event {
    Status(Status),
    /// A ready check just appeared (the queue popped).
    QueuePop,
}

#[derive(Clone)]
struct Client {
    port: u16,
    auth: String,
}

/// Port and token from the running client's command line, or from its lockfile.
fn find_client() -> Option<Client> {
    if let Ok(out) = std::process::Command::new("/bin/ps").args(["-A", "-ww", "-o", "args="]).output() {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if !line.contains("LeagueClientUx") || !line.contains("--app-port=") {
                continue;
            }
            let arg = |name: &str| line.split_whitespace().find_map(|a| a.trim_matches('"').strip_prefix(name).map(str::to_string));
            if let (Some(port), Some(token)) = (arg("--app-port=").and_then(|p| p.parse().ok()), arg("--remoting-auth-token=")) {
                return Some(Client { port, auth: http::base64(format!("riot:{token}").as_bytes()) });
            }
        }
    }
    for app in ["League of Legends.app", "League of Legends (PBE).app"] {
        let path = format!("/Applications/{app}/Contents/LoL/lockfile");
        if let Ok(text) = std::fs::read_to_string(&path) {
            // name:pid:port:password:protocol
            let f: Vec<&str> = text.trim().split(':').collect();
            if f.len() >= 4 {
                if let Ok(port) = f[2].parse() {
                    return Some(Client { port, auth: http::base64(format!("riot:{}", f[3]).as_bytes()) });
                }
            }
        }
    }
    None
}

impl Client {
    fn call(&self, method: &str, path: &str, body: Option<&[u8]>) -> Result<http::Response, String> {
        let addr = ("127.0.0.1", self.port).to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("no address")?;
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|e| e.to_string())?;
        let _ = tcp.set_read_timeout(Some(Duration::from_secs(4)));
        let _ = tcp.set_write_timeout(Some(Duration::from_secs(4)));
        // The client's certificate is self-signed for 127.0.0.1; we only ever talk to localhost.
        let tls = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .build()
            .map_err(|e| e.to_string())?;
        let mut stream = tls.connect("127.0.0.1", tcp).map_err(|e| e.to_string())?;
        http::request(&mut stream, method, &format!("127.0.0.1:{}", self.port), path, Some(&self.auth), body).map_err(|e| e.to_string())
    }

    fn get(&self, path: &str) -> Option<Value> {
        self.call("GET", path, None).ok().filter(|r| r.ok()).and_then(|r| r.json())
    }
}

pub fn spawn(cfg: LeagueCfg, emit: impl Fn(Event) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("league".into()).spawn(move || run(cfg, rx, emit)).unwrap();
    tx
}

fn run(mut cfg: LeagueCfg, rx: Receiver<Cmd>, emit: impl Fn(Event)) {
    let mut client: Option<Client> = None;
    let mut status = Status::default();
    let mut published = Status { running: true, ..Default::default() };
    let mut champions: HashMap<i64, String> = HashMap::new();
    let mut queues: HashMap<i64, String> = HashMap::new();
    let mut ready_seen: Option<Instant> = None;
    let mut accepted_this_check = false;
    let mut next_summoner = Instant::now();
    loop {
        let wait = if client.is_some() { Duration::from_millis(700) } else { Duration::from_secs(3) };
        match rx.recv_timeout(wait) {
            Ok(cmd) => {
                if let Cmd::Configure(c) = cmd {
                    cfg = c;
                } else if let Some(c) = &client {
                    let (method, path) = match cmd {
                        Cmd::Accept => ("POST", "/lol-matchmaking/v1/ready-check/accept"),
                        Cmd::Decline => ("POST", "/lol-matchmaking/v1/ready-check/decline"),
                        Cmd::FindMatch => ("POST", "/lol-lobby/v2/lobby/matchmaking/search"),
                        Cmd::CancelSearch => ("DELETE", "/lol-lobby/v2/lobby/matchmaking/search"),
                        Cmd::Configure(_) => unreachable!(),
                    };
                    match c.call(method, path, None) {
                        Ok(r) if r.ok() => log(format!("league: {method} {path}")),
                        Ok(r) => log(format!("league: {path} -> {} {}", r.status, String::from_utf8_lossy(&r.body).chars().take(120).collect::<String>())),
                        Err(e) => log(format!("league: {path} failed: {e}")),
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        if client.is_none() {
            client = find_client();
            if let Some(c) = &client {
                log(format!("league: client on port {}", c.port));
                next_summoner = Instant::now();
            }
        }
        let Some(c) = client.clone() else {
            status = Status::default();
            publish(&mut published, &status, &emit);
            continue;
        };
        let phase = match c.call("GET", "/lol-gameflow/v1/gameflow-phase", None) {
            Ok(r) if r.ok() => r.json().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default(),
            Ok(_) => String::new(),
            Err(_) => {
                // Client closed (or restarted with a new port/token).
                client = None;
                status = Status::default();
                publish(&mut published, &status, &emit);
                continue;
            }
        };
        status.running = true;
        status.error = None;
        if Instant::now() >= next_summoner {
            next_summoner = Instant::now() + Duration::from_secs(30);
            if let Some(s) = c.get("/lol-summoner/v1/current-summoner") {
                let name = s.get("gameName").and_then(Value::as_str).filter(|n| !n.is_empty()).or_else(|| s.get("displayName").and_then(Value::as_str)).unwrap_or("").to_string();
                let level = s.get("summonerLevel").and_then(Value::as_u64).unwrap_or(0) as u32;
                status.summoner = (!name.is_empty()).then_some((name, level));
            }
            if champions.is_empty() {
                if let Some(Value::Array(list)) = c.get("/lol-game-data/assets/v1/champion-summary.json") {
                    for ch in list {
                        if let (Some(id), Some(name)) = (ch.get("id").and_then(Value::as_i64), ch.get("name").and_then(Value::as_str)) {
                            champions.insert(id, name.to_string());
                        }
                    }
                }
            }
        }

        // Queue name (lobby / matchmaking / ready check / champ select)
        status.queue = None;
        if matches!(phase.as_str(), "Lobby" | "Matchmaking" | "ReadyCheck" | "ChampSelect") {
            let queue_id = c.get("/lol-lobby/v2/lobby").and_then(|l| l.pointer("/gameConfig/queueId").and_then(Value::as_i64));
            if let Some(id) = queue_id {
                if !queues.contains_key(&id) {
                    if let Some(q) = c.get(&format!("/lol-game-queues/v1/queues/{id}")) {
                        let name = q.get("description").and_then(Value::as_str).filter(|s| !s.is_empty()).or_else(|| q.get("shortName").and_then(Value::as_str)).unwrap_or("").to_string();
                        queues.insert(id, name);
                    }
                }
                status.queue = queues.get(&id).cloned().filter(|s| !s.is_empty());
            }
        }

        status.search = None;
        if phase == "Matchmaking" {
            if let Some(s) = c.get("/lol-matchmaking/v1/search") {
                let t = s.get("timeInQueue").and_then(Value::as_f64).unwrap_or(0.0);
                let est = s.get("estimatedQueueTime").and_then(Value::as_f64).unwrap_or(0.0);
                status.search = Some((t, est));
            }
        }

        status.ready = None;
        if phase == "ReadyCheck" {
            if let Some(r) = c.get("/lol-matchmaking/v1/ready-check") {
                let rc = ReadyCheck {
                    state: r.get("state").and_then(Value::as_str).unwrap_or("").to_string(),
                    response: r.get("playerResponse").and_then(Value::as_str).unwrap_or("").to_string(),
                    timer: r.get("timer").and_then(Value::as_f64).unwrap_or(0.0),
                };
                if ready_seen.is_none() {
                    ready_seen = Some(Instant::now());
                    accepted_this_check = false;
                    log("league: queue popped");
                    emit(Event::QueuePop);
                }
                let waited = ready_seen.map(|t| t.elapsed()).unwrap_or_default();
                if cfg.auto_accept && rc.response == "None" && !accepted_this_check && waited >= Duration::from_millis(cfg.accept_delay_ms) {
                    accepted_this_check = true;
                    match c.call("POST", "/lol-matchmaking/v1/ready-check/accept", None) {
                        Ok(r) if r.ok() => {
                            status.accepted_by_quaked = true;
                            log("league: auto-accepted");
                        }
                        Ok(r) => log(format!("league: accept -> {}", r.status)),
                        Err(e) => log(format!("league: accept failed: {e}")),
                    }
                }
                status.ready = Some(rc);
            }
        } else {
            ready_seen = None;
            if phase != "ChampSelect" {
                status.accepted_by_quaked = false;
            }
        }

        status.champ = None;
        if phase == "ChampSelect" {
            if let Some(s) = c.get("/lol-champ-select/v1/session") {
                let me = s.get("localPlayerCellId").and_then(Value::as_i64).unwrap_or(-1);
                let mine = s.get("myTeam").and_then(Value::as_array).and_then(|t| t.iter().find(|p| p.get("cellId").and_then(Value::as_i64) == Some(me)));
                let champ_id = mine.and_then(|p| p.get("championId").and_then(Value::as_i64)).filter(|&id| id > 0).or_else(|| mine.and_then(|p| p.get("championPickIntent").and_then(Value::as_i64)).filter(|&id| id > 0));
                status.champ = Some(ChampSelect {
                    phase: s.pointer("/timer/phase").and_then(Value::as_str).unwrap_or("").to_string(),
                    time_left_ms: s.pointer("/timer/adjustedTimeLeftInPhase").and_then(Value::as_f64).unwrap_or(0.0),
                    position: mine.and_then(|p| p.get("assignedPosition").and_then(Value::as_str)).unwrap_or("").to_string(),
                    champion: champ_id.and_then(|id| champions.get(&id).cloned()).unwrap_or_default(),
                });
            }
        }
        status.phase = phase;
        publish(&mut published, &status, &emit);
    }
}

fn publish(published: &mut Status, status: &Status, emit: &impl Fn(Event)) {
    if status != published {
        *published = status.clone();
        emit(Event::Status(status.clone()));
    }
}
