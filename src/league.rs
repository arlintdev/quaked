//! League of Legends client (LCU API on 127.0.0.1, self-signed TLS, Basic auth with the
//! client's per-launch token). Shows the game phase, and accepts the ready check when
//! auto-accept is on. Works with live and PBE installs.
//!
//! It also keeps your record with the people you play with: every finished matched game
//! from the client's match history is saved to league-history.json (so the tally keeps
//! growing past the client's short history), and during champ select and the game it
//! shows your wins and losses with the teammates you have.
//!
//! During a game it reads Riot's Live Client Data API (https://127.0.0.1:2999, served by
//! the game itself) for the clock, score, your stats, your respawn timer and the events,
//! all of which the game shows you anyway.

use crate::config::League as LeagueCfg;
use crate::device::log;
use crate::http;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
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

/// What kind of game: the pane changes its layout and colors to match.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    None,
    Aram,
    /// ARAM: Mayhem (gameMode "KIWI")
    Mayhem,
    /// Summoner's Rift: normal, ranked, quickplay, URF...
    Rift,
    Arena,
    Other,
}

impl Mode {
    pub fn of(game_mode: &str) -> Mode {
        match game_mode {
            "" => Mode::None,
            "ARAM" => Mode::Aram,
            "KIWI" => Mode::Mayhem,
            "CLASSIC" | "URF" | "ARURF" | "ONEFORALL" | "NEXUSBLITZ" | "ULTBOOK" | "SWIFTPLAY" => Mode::Rift,
            "CHERRY" => Mode::Arena,
            _ => Mode::Other,
        }
    }
    /// ARAM and Mayhem count together for your ARAM record.
    pub fn family(self) -> Mode {
        if self == Mode::Mayhem { Mode::Aram } else { self }
    }
}

/// One player on the in-game scoreboard (what the game's Tab screen shows).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LivePlayer {
    pub champion: String,
    pub name: String,
    pub mine: bool,
    pub me: bool,
    pub level: u32,
    pub kills: u32,
    pub deaths: u32,
    pub assists: u32,
    pub cs: u32,
    pub dead: bool,
    pub respawn: f64,
}

/// One line of the in-game event feed: (game time, text, 1 your team / -1 theirs / 0 neither).
pub type GameEvent = (f64, String, i8);

/// The game in progress, from the Live Client Data API.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveGame {
    /// Seconds since the game started.
    pub time: f64,
    pub mode: String,
    /// 11 Summoner's Rift, 12 Howling Abyss...
    pub map: i64,
    pub champion: String,
    pub level: u32,
    pub gold: f64,
    pub kills: u32,
    pub deaths: u32,
    pub assists: u32,
    pub cs: u32,
    pub dead: bool,
    pub respawn: f64,
    pub killed_by: String,
    pub ally_kills: u32,
    pub enemy_kills: u32,
    /// Newest last.
    pub events: Vec<GameEvent>,
    pub result: Option<String>,
    pub ward_score: f64,
    /// Both teams, yours first.
    pub players: Vec<LivePlayer>,
    /// Turrets each side has taken (yours, theirs), from the events.
    pub turrets: (u32, u32),
    pub inhibs: (u32, u32),
    /// Dragon types each side has taken, in order.
    pub dragons: (Vec<String>, Vec<String>),
    pub barons: (u32, u32),
    pub heralds: (u32, u32),
    pub grubs: (u32, u32),
}

/// Your record with one person (games on the same team).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Duo {
    pub puuid: String,
    pub name: String,
    pub games: u32,
    pub wins: u32,
    /// On your friends list.
    pub friend: bool,
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
    /// The people you've played with most (friends, or 3+ games together), most games first.
    pub duos: Vec<Duo>,
    /// Your teammates right now (lobby, champ select, in game) with your record together.
    pub with_now: Vec<Duo>,
    /// Wins and losses across the saved games.
    pub record: (u32, u32),
    /// The game you're in, while it runs.
    pub live: Option<LiveGame>,
    /// This session (games less than 90 minutes apart): wins, losses.
    pub session: (u32, u32),
    /// Positive: wins in a row; negative: losses in a row.
    pub streak: i32,
    /// What you're playing (or about to): lobby, champ select, game.
    pub mode: Mode,
    /// The queue's name for it ("ARAM: Mayhem", "Ranked Solo/Duo").
    pub mode_name: String,
    /// Your record in this kind of game (ARAM and Mayhem together).
    pub mode_record: (u32, u32),
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
    let mut store = load_store();
    let mut me = String::new();
    let mut friends: HashSet<String> = HashSet::new();
    let mut duos: HashMap<String, Duo> = HashMap::new();
    let mut next_history = Instant::now();
    let mut next_mates = Instant::now();
    let mut last_phase = String::new();
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
                let puuid = s.get("puuid").and_then(Value::as_str).unwrap_or("").to_string();
                if puuid != me {
                    me = puuid;
                    next_history = Instant::now();
                }
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
        // A game just ended: its result shows up in the history a little later.
        let ended = matches!(last_phase.as_str(), "InProgress" | "GameStart" | "Reconnect" | "WaitingForStats" | "PreEndOfGame") && matches!(phase.as_str(), "EndOfGame" | "Lobby" | "None" | "");
        if ended {
            next_history = Instant::now() + Duration::from_secs(20);
        }
        if !me.is_empty() && Instant::now() >= next_history {
            friends = friend_puuids(&c);
            let added = sync_history(&c, &me, &mut store);
            if added > 0 {
                log(format!("league: saved {added} game{} to your history", if added == 1 { "" } else { "s" }));
            }
            duos = tally(&store, &me, &friends);
            let mine: Vec<&GameRec> = store.games.iter().filter(|g| g.me == me).collect();
            let wins = mine.iter().filter(|g| g.win).count() as u32;
            status.record = (wins, mine.len() as u32 - wins);
            (status.session, status.streak) = session_of(&store, &me);
            if status.mode != Mode::None {
                status.mode_record = record_in(&store, &me, status.mode);
            }
            let mut top: Vec<Duo> = duos.values().filter(|d| d.friend || d.games >= 3).cloned().collect();
            top.sort_by(|a, b| b.games.cmp(&a.games).then(b.wins.cmp(&a.wins)));
            top.truncate(8);
            status.duos = top;
            // Look again soon after a game (its result can be late), otherwise now and then.
            next_history = Instant::now() + Duration::from_secs(if ended { 45 } else { 600 });
        }
        if Instant::now() >= next_mates || phase != last_phase {
            next_mates = Instant::now() + Duration::from_secs(4);
            // What you're playing: the queue in the lobby, champ select or game.
            let (mode, name) = match c.get("/lol-gameflow/v1/session").and_then(|s| s.pointer("/gameData/queue").cloned()) {
                Some(q) if !matches!(phase.as_str(), "None" | "") => (
                    Mode::of(q.get("gameMode").and_then(Value::as_str).unwrap_or("")),
                    q.get("description").and_then(Value::as_str).filter(|d| !d.is_empty()).or_else(|| q.get("shortName").and_then(Value::as_str)).unwrap_or("").to_string(),
                ),
                _ => (Mode::None, String::new()),
            };
            if mode != status.mode || status.mode_record == (0, 0) {
                status.mode_record = if me.is_empty() || mode == Mode::None { (0, 0) } else { record_in(&store, &me, mode) };
            }
            status.mode = mode;
            status.mode_name = name;
            status.with_now = if me.is_empty() {
                Vec::new()
            } else {
                current_mates(&c, &phase, &me)
                    .into_iter()
                    .map(|(puuid, name)| match duos.get(&puuid) {
                        Some(d) => Duo { name: if name.is_empty() { d.name.clone() } else { name }, ..d.clone() },
                        None => Duo { friend: friends.contains(&puuid), puuid, name, ..Default::default() },
                    })
                    .collect()
            };
        }
        // The game itself, about once a second while it runs.
        status.live = if matches!(phase.as_str(), "InProgress" | "GameStart" | "Reconnect" | "WaitingForStats" | "PreEndOfGame") {
            live_get("/liveclientdata/allgamedata").as_ref().and_then(parse_live)
        } else {
            None
        };
        if let Some(g) = &status.live {
            let m = Mode::of(&g.mode);
            if m != Mode::None && m != status.mode {
                status.mode = m;
                status.mode_record = record_in(&store, &me, m);
            }
        }
        last_phase = phase.clone();
        status.phase = phase;
        publish(&mut published, &status, &emit);
    }
}

// ---------------------------------------------------------------- your record with people

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct GameRec {
    id: i64,
    /// Unix ms
    at: u64,
    /// Your puuid in that game (the history can hold more than one account).
    me: String,
    win: bool,
    queue: i64,
    /// Teammates: (puuid, name)
    mates: Vec<(String, String)>,
    /// gameMode ("ARAM", "KIWI", "CLASSIC", "CHERRY"...)
    #[serde(default)]
    mode: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    games: Vec<GameRec>,
}

fn store_path() -> std::path::PathBuf {
    crate::config::dir().join("league-history.json")
}

fn load_store() -> Store {
    std::fs::read_to_string(store_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_store(s: &Store) {
    let path = store_path();
    let tmp = path.with_extension("json.tmp");
    if let Ok(text) = serde_json::to_string(s) {
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn player_name(p: &Value) -> String {
    let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let n = s("gameName");
    if n.is_empty() { s("summonerName") } else { n }
}

/// Adds finished matched games from the client's history that the store doesn't have yet.
fn sync_history(c: &Client, me: &str, store: &mut Store) -> usize {
    let Some(list) = c.get("/lol-match-history/v1/products/lol/current-summoner/matches?begIndex=0&endIndex=50") else { return 0 };
    let known: HashSet<i64> = store.games.iter().map(|g| g.id).collect();
    let mut added = 0;
    // Fill in the mode for games saved before it was recorded.
    let mut filled = false;
    for g in list.pointer("/games/games").and_then(Value::as_array).into_iter().flatten() {
        let (Some(id), Some(mode)) = (g.get("gameId").and_then(Value::as_i64), g.get("gameMode").and_then(Value::as_str)) else { continue };
        if let Some(rec) = store.games.iter_mut().find(|r| r.id == id && r.mode.is_empty()) {
            rec.mode = mode.to_string();
            filled = true;
        }
    }
    for g in list.pointer("/games/games").and_then(Value::as_array).into_iter().flatten() {
        let Some(id) = g.get("gameId").and_then(Value::as_i64) else { continue };
        if known.contains(&id) || g.get("gameType").and_then(Value::as_str) != Some("MATCHED_GAME") {
            continue;
        }
        // The list only names you; the game itself names everyone.
        let Some(full) = c.get(&format!("/lol-match-history/v1/games/{id}")) else { continue };
        let ids = full.get("participantIdentities").and_then(Value::as_array).cloned().unwrap_or_default();
        let parts = full.get("participants").and_then(Value::as_array).cloned().unwrap_or_default();
        let pid_of = |puuid: &str| ids.iter().find(|i| i.pointer("/player/puuid").and_then(Value::as_str) == Some(puuid)).and_then(|i| i.get("participantId").and_then(Value::as_i64));
        let Some(my_pid) = pid_of(me) else { continue };
        let team_of = |pid: i64| parts.iter().find(|p| p.get("participantId").and_then(Value::as_i64) == Some(pid)).and_then(|p| p.get("teamId").and_then(Value::as_i64));
        let Some(my_team) = team_of(my_pid) else { continue };
        let win = parts
            .iter()
            .find(|p| p.get("participantId").and_then(Value::as_i64) == Some(my_pid))
            .and_then(|p| p.pointer("/stats/win").and_then(Value::as_bool))
            .unwrap_or(false);
        let mates = ids
            .iter()
            .filter_map(|i| {
                let pid = i.get("participantId").and_then(Value::as_i64)?;
                let puuid = i.pointer("/player/puuid").and_then(Value::as_str)?.to_string();
                (pid != my_pid && team_of(pid) == Some(my_team) && !puuid.is_empty()).then(|| (puuid, player_name(i.get("player").unwrap_or(&Value::Null))))
            })
            .collect();
        let mode = full.get("gameMode").and_then(Value::as_str).unwrap_or("").to_string();
        store.games.push(GameRec { id, at: full.get("gameCreation").and_then(Value::as_u64).unwrap_or(0), me: me.to_string(), win, queue: full.get("queueId").and_then(Value::as_i64).unwrap_or(0), mates, mode });
        added += 1;
    }
    if added > 0 || filled {
        store.games.sort_by_key(|g| g.at);
        save_store(store);
    }
    added
}

/// Your record with everyone you've played with, from the saved games.
fn tally(store: &Store, me: &str, friends: &HashSet<String>) -> HashMap<String, Duo> {
    let mut out: HashMap<String, Duo> = HashMap::new();
    for g in store.games.iter().filter(|g| g.me == me) {
        for (puuid, name) in &g.mates {
            let d = out.entry(puuid.clone()).or_insert_with(|| Duo { puuid: puuid.clone(), friend: friends.contains(puuid), ..Default::default() });
            d.games += 1;
            d.wins += u32::from(g.win);
            if !name.is_empty() {
                d.name = name.clone(); // the newest name wins (games are oldest first)
            }
        }
    }
    out
}

/// Who's on your team right now: (puuid, name).
fn current_mates(c: &Client, phase: &str, me: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    match phase {
        "Lobby" | "Matchmaking" | "ReadyCheck" => {
            for m in c.get("/lol-lobby/v2/lobby").and_then(|l| l.get("members").cloned()).and_then(|m| m.as_array().cloned()).unwrap_or_default() {
                if let Some(p) = m.get("puuid").and_then(Value::as_str).filter(|p| *p != me) {
                    out.push((p.to_string(), player_name(&m)));
                }
            }
        }
        "ChampSelect" => {
            for m in c.get("/lol-champ-select/v1/session").and_then(|s| s.get("myTeam").cloned()).and_then(|m| m.as_array().cloned()).unwrap_or_default() {
                if let Some(p) = m.get("puuid").and_then(Value::as_str).filter(|p| !p.is_empty() && *p != me) {
                    out.push((p.to_string(), player_name(&m)));
                }
            }
        }
        "InProgress" | "GameStart" | "Reconnect" => {
            if let Some(gd) = c.get("/lol-gameflow/v1/session").and_then(|s| s.get("gameData").cloned()) {
                for side in ["teamOne", "teamTwo"] {
                    let team = gd.get(side).and_then(Value::as_array).cloned().unwrap_or_default();
                    if team.iter().any(|p| p.get("puuid").and_then(Value::as_str) == Some(me)) {
                        for p in team {
                            if let Some(id) = p.get("puuid").and_then(Value::as_str).filter(|p| !p.is_empty() && *p != me) {
                                out.push((id.to_string(), player_name(&p)));
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out
}

fn friend_puuids(c: &Client) -> HashSet<String> {
    c.get("/lol-chat/v1/friends")
        .and_then(|f| f.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|f| f.get("puuid").and_then(Value::as_str).filter(|p| !p.is_empty()).map(str::to_string))
        .collect()
}

// ---------------------------------------------------------------- the game in progress

/// GET on the game's own Live Client Data API (self-signed, localhost only, no auth).
fn live_get(path: &str) -> Option<Value> {
    let addr = ("127.0.0.1", 2999).to_socket_addrs().ok()?.next()?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_millis(400)).ok()?;
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(2)));
    let tls = native_tls::TlsConnector::builder().danger_accept_invalid_certs(true).danger_accept_invalid_hostnames(true).build().ok()?;
    let mut stream = tls.connect("127.0.0.1", tcp).ok()?;
    let r = http::request(&mut stream, "GET", "127.0.0.1:2999", path, None, None).ok()?;
    r.ok().then(|| r.json()).flatten()
}

fn dragon_name(kind: &str) -> &str {
    match kind {
        "Fire" => "Infernal",
        "Water" => "Ocean",
        "Earth" => "Mountain",
        "Air" => "Cloud",
        other => other,
    }
}

/// The live game from `allgamedata`, with events told from your side.
fn parse_live(d: &Value) -> Option<LiveGame> {
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let n = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let active = d.get("activePlayer")?;
    let players = d.get("allPlayers").and_then(Value::as_array)?;
    let my_ids = [s(active, "riotId"), s(active, "summonerName"), s(active, "riotIdGameName")];
    let is_me = |p: &Value| [s(p, "riotId"), s(p, "summonerName"), s(p, "riotIdGameName")].iter().any(|x| !x.is_empty() && my_ids.contains(x));
    let me = players.iter().find(|p| is_me(p))?;
    let my_team = s(me, "team");
    // Event names use player names; show champions instead, as the game's kill feed does.
    let champ_of = |name: &str| -> Option<(String, String)> {
        players
            .iter()
            .find(|p| [s(p, "riotId"), s(p, "summonerName"), s(p, "riotIdGameName")].iter().any(|x| !x.is_empty() && x == name))
            .map(|p| (s(p, "championName"), s(p, "team")))
    };
    let side = |team: &str| if team.is_empty() { 0 } else if team == my_team { 1 } else { -1 };
    let team_word = |team: &str| if team == my_team { "Your team" } else { "Enemy team" };
    let my_name = |x: &str| my_ids.iter().any(|m| !m.is_empty() && m == x);
    let scores = me.get("scores").cloned().unwrap_or_default();
    let mut live = LiveGame {
        time: n(d.get("gameData")?, "gameTime"),
        mode: s(d.get("gameData")?, "gameMode"),
        map: d.pointer("/gameData/mapNumber").and_then(Value::as_i64).unwrap_or(0),
        champion: s(me, "championName"),
        level: active.get("level").and_then(Value::as_u64).unwrap_or(0) as u32,
        gold: n(active, "currentGold"),
        kills: scores.get("kills").and_then(Value::as_u64).unwrap_or(0) as u32,
        deaths: scores.get("deaths").and_then(Value::as_u64).unwrap_or(0) as u32,
        assists: scores.get("assists").and_then(Value::as_u64).unwrap_or(0) as u32,
        cs: scores.get("creepScore").and_then(Value::as_u64).unwrap_or(0) as u32,
        dead: me.get("isDead").and_then(Value::as_bool).unwrap_or(false),
        respawn: n(me, "respawnTimer"),
        ..Default::default()
    };
    for p in players {
        let k = p.pointer("/scores/kills").and_then(Value::as_u64).unwrap_or(0) as u32;
        if s(p, "team") == my_team {
            live.ally_kills += k;
        } else {
            live.enemy_kills += k;
        }
    }
    let events = d.pointer("/events/Events").and_then(Value::as_array).cloned().unwrap_or_default();
    for e in &events {
        let t = n(e, "EventTime");
        let killer = s(e, "KillerName");
        let (kchamp, kteam) = champ_of(&killer).unwrap_or_default();
        let line: Option<(String, i8)> = match s(e, "EventName").as_str() {
            "FirstBlood" => {
                let (c, team) = champ_of(&s(e, "Recipient")).unwrap_or_default();
                Some((format!("First blood: {}", if c.is_empty() { "?".into() } else { c }), side(&team)))
            }
            "ChampionKill" => {
                let victim = s(e, "VictimName");
                let (vchamp, _) = champ_of(&victim).unwrap_or_default();
                let assisted = e.get("Assisters").and_then(Value::as_array).is_some_and(|a| a.iter().any(|x| x.as_str().is_some_and(my_name)));
                if my_name(&victim) {
                    live.killed_by = if kchamp.is_empty() { "the enemy".into() } else { kchamp.clone() };
                    Some((format!("{} killed you", live.killed_by), -1))
                } else if my_name(&killer) {
                    Some((format!("You killed {vchamp}"), 1))
                } else if assisted {
                    Some((format!("You helped kill {vchamp}"), 1))
                } else {
                    None
                }
            }
            "Multikill" => {
                let what = match e.get("KillStreak").and_then(Value::as_u64).unwrap_or(0) {
                    2 => "double kill",
                    3 => "triple kill",
                    4 => "quadra kill",
                    5 => "PENTAKILL",
                    _ => "multikill",
                };
                Some((format!("{kchamp}: {what}"), side(&kteam)))
            }
            "Ace" => {
                let team = s(e, "AcingTeam");
                Some((format!("{} aced", team_word(&team)), side(&team)))
            }
            "DragonKill" => {
                let stolen = if s(e, "Stolen") == "True" { " (stolen)" } else { "" };
                Some((format!("{} took the {} drake{stolen}", team_word(&kteam), dragon_name(&s(e, "DragonType"))), side(&kteam)))
            }
            "BaronKill" => Some((format!("{} took Baron", team_word(&kteam)), side(&kteam))),
            "HeraldKill" => Some((format!("{} took the Herald", team_word(&kteam)), side(&kteam))),
            "HordeKill" => Some((format!("{} took Voidgrubs", team_word(&kteam)), side(&kteam))),
            "AtakhanKill" => Some((format!("{} took Atakhan", team_word(&kteam)), side(&kteam))),
            "TurretKilled" | "InhibKilled" => {
                // "Turret_T1_..." belongs to ORDER (T1), "..._T2_..." to CHAOS: whoever owns it lost it.
                let what = s(e, if s(e, "EventName") == "TurretKilled" { "TurretKilled" } else { "InhibKilled" });
                let owner = if what.contains("_T1_") { "ORDER" } else if what.contains("_T2_") { "CHAOS" } else { "" };
                let taker = match owner {
                    "ORDER" => "CHAOS",
                    "CHAOS" => "ORDER",
                    _ => "",
                };
                let thing = if s(e, "EventName") == "TurretKilled" { "a turret" } else { "an inhibitor" };
                (!taker.is_empty()).then(|| (format!("{} took {thing}", team_word(taker)), side(taker)))
            }
            "GameEnd" => {
                let r = s(e, "Result");
                live.result = Some(r.clone());
                Some((if r == "Win" { "Victory".to_string() } else { "Defeat".to_string() }, if r == "Win" { 1 } else { -1 }))
            }
            _ => None,
        };
        if let Some((text, sd)) = line {
            live.events.push((t, text, sd));
        }
    }
    if live.events.len() > 12 {
        live.events.drain(..live.events.len() - 12);
    }
    // Objectives and towers, counted per side.
    for e in &events {
        let name = s(e, "EventName");
        let kteam = champ_of(&s(e, "KillerName")).map(|(_, t)| t).unwrap_or_default();
        let ours = !kteam.is_empty() && kteam == my_team;
        let theirs = !kteam.is_empty() && kteam != my_team;
        let bump = |pair: &mut (u32, u32)| {
            if ours {
                pair.0 += 1
            } else if theirs {
                pair.1 += 1
            }
        };
        match name.as_str() {
            "DragonKill" => {
                let kind = dragon_name(&s(e, "DragonType")).to_string();
                if ours {
                    live.dragons.0.push(kind)
                } else if theirs {
                    live.dragons.1.push(kind)
                }
            }
            "BaronKill" => bump(&mut live.barons),
            "HeraldKill" => bump(&mut live.heralds),
            "HordeKill" => bump(&mut live.grubs),
            "TurretKilled" | "InhibKilled" => {
                let what = s(e, &name);
                let lost_by_them = (what.contains("_T2_") && my_team == "ORDER") || (what.contains("_T1_") && my_team == "CHAOS");
                let lost_by_us = (what.contains("_T1_") && my_team == "ORDER") || (what.contains("_T2_") && my_team == "CHAOS");
                let pair = if name == "TurretKilled" { &mut live.turrets } else { &mut live.inhibs };
                if lost_by_them {
                    pair.0 += 1
                } else if lost_by_us {
                    pair.1 += 1
                }
            }
            _ => {}
        }
    }
    // The scoreboard: your team first, you at the top of it.
    live.ward_score = scores.get("wardScore").and_then(Value::as_f64).unwrap_or(0.0);
    let short = |p: &Value| {
        let n = s(p, "riotIdGameName");
        let n = if n.is_empty() { s(p, "summonerName") } else { n };
        n.split('#').next().unwrap_or("").to_string()
    };
    let mut board: Vec<LivePlayer> = players
        .iter()
        .map(|p| {
            let sc = p.get("scores").cloned().unwrap_or_default();
            let u = |k: &str| sc.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
            LivePlayer {
                champion: s(p, "championName"),
                name: short(p),
                mine: s(p, "team") == my_team,
                me: is_me(p),
                level: p.get("level").and_then(Value::as_u64).unwrap_or(0) as u32,
                kills: u("kills"),
                deaths: u("deaths"),
                assists: u("assists"),
                cs: u("creepScore"),
                dead: p.get("isDead").and_then(Value::as_bool).unwrap_or(false),
                respawn: n(p, "respawnTimer"),
            }
        })
        .collect();
    board.sort_by_key(|p| (!p.mine, !p.me));
    live.players = board;
    Some(live)
}

/// The kind of game a saved record was (older records only have the queue id).
fn rec_mode(r: &GameRec) -> Mode {
    if !r.mode.is_empty() {
        return Mode::of(&r.mode);
    }
    match r.queue {
        450 | 100 | 720 | 3270 => Mode::Aram,
        2400 => Mode::Mayhem,
        1700 | 1710 => Mode::Arena,
        400 | 420 | 430 | 440 | 480 | 490 | 700 | 900 | 1900 | 1020 => Mode::Rift,
        _ => Mode::Other,
    }
}

/// Your wins and losses in one kind of game.
fn record_in(store: &Store, me: &str, mode: Mode) -> (u32, u32) {
    let games: Vec<&GameRec> = store.games.iter().filter(|g| g.me == me && rec_mode(g).family() == mode.family()).collect();
    let wins = games.iter().filter(|g| g.win).count() as u32;
    (wins, games.len() as u32 - wins)
}

/// This session's record (games < 90 minutes apart, newest less than 3 hours ago) and the
/// current streak, from the saved games.
fn session_of(store: &Store, me: &str) -> ((u32, u32), i32) {
    let mine: Vec<&GameRec> = store.games.iter().filter(|g| g.me == me).collect();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let mut session = (0, 0);
    if mine.last().is_some_and(|g| now.saturating_sub(g.at) < 3 * 3_600_000) {
        let mut prev = mine.last().map(|g| g.at).unwrap_or(0);
        for g in mine.iter().rev() {
            if prev.saturating_sub(g.at) > 90 * 60_000 {
                break;
            }
            if g.win { session.0 += 1 } else { session.1 += 1 }
            prev = g.at;
        }
    }
    let mut streak = 0i32;
    if let Some(last) = mine.last() {
        for g in mine.iter().rev() {
            if g.win != last.win {
                break;
            }
            streak += if g.win { 1 } else { -1 };
        }
    }
    (session, streak)
}

fn publish(published: &mut Status, status: &Status, emit: &impl Fn(Event)) {
    if status != published {
        *published = status.clone();
        emit(Event::Status(status.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_game_reads_from_your_side() {
        let d: Value = serde_json::from_str(r#"{
          "activePlayer": {"riotId": "Stoic Otter#PBE", "summonerName": "Stoic Otter#PBE", "level": 13, "currentGold": 1350.5},
          "allPlayers": [
            {"riotId": "Stoic Otter#PBE", "summonerName": "Stoic Otter#PBE", "championName": "Ahri", "team": "ORDER", "isDead": true, "respawnTimer": 23.4, "scores": {"kills": 4, "deaths": 2, "assists": 7, "creepScore": 142}},
            {"riotId": "Ally#1", "summonerName": "Ally#1", "championName": "Leona", "team": "ORDER", "isDead": false, "respawnTimer": 0, "scores": {"kills": 8, "deaths": 1, "assists": 3, "creepScore": 20}},
            {"riotId": "Foe#1", "summonerName": "Foe#1", "championName": "Jinx", "team": "CHAOS", "isDead": false, "respawnTimer": 0, "scores": {"kills": 9, "deaths": 5, "assists": 2, "creepScore": 180}}
          ],
          "events": {"Events": [
            {"EventName": "GameStart", "EventTime": 0.0},
            {"EventName": "FirstBlood", "EventTime": 190.0, "Recipient": "Ally#1"},
            {"EventName": "DragonKill", "EventTime": 400.0, "KillerName": "Foe#1", "DragonType": "Fire", "Stolen": "False"},
            {"EventName": "TurretKilled", "EventTime": 600.0, "KillerName": "Ally#1", "TurretKilled": "Turret_T2_R_03_A"},
            {"EventName": "ChampionKill", "EventTime": 700.0, "KillerName": "Foe#1", "VictimName": "Stoic Otter#PBE", "Assisters": []},
            {"EventName": "Multikill", "EventTime": 701.0, "KillerName": "Foe#1", "KillStreak": 3}
          ]},
          "gameData": {"gameMode": "CLASSIC", "gameTime": 712.0, "mapNumber": 11}
        }"#).unwrap();
        let g = parse_live(&d).unwrap();
        assert_eq!((g.kills, g.deaths, g.assists, g.cs, g.level), (4, 2, 7, 142, 13));
        assert_eq!((g.ally_kills, g.enemy_kills), (12, 9));
        assert!(g.dead && g.respawn > 23.0);
        assert_eq!(g.killed_by, "Jinx");
        let texts: Vec<(&str, i8)> = g.events.iter().map(|e| (e.1.as_str(), e.2)).collect();
        assert_eq!(texts, vec![
            ("First blood: Leona", 1),
            ("Enemy team took the Infernal drake", -1),
            ("Your team took a turret", 1),
            ("Jinx killed you", -1),
            ("Jinx: triple kill", -1),
        ]);
    }
}
