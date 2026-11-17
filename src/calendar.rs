//! Calendar: your next meetings from macOS Calendar (EventKit), with their join links.
//!
//! The thread asks for calendar access once, then reads events from two hours ago to a
//! day ahead every minute, and right away when the calendar database changes. Join
//! links (Zoom, Meet, Teams, FaceTime, Webex...) are found in the event's URL, location
//! and notes. The app side (app/calendar.rs) keeps the knob's pick and the
//! "starts in 2 minutes" alerts.

use crate::config::CalendarSettings as Cfg;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// One calendar event. Other features (call mode, night mode) read these fields: keep
/// them, add more as needed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Event {
    /// Stable for one occurrence (EventKit's id plus the start, since repeats share an id).
    pub id: String,
    pub title: String,
    /// Unix ms
    pub start_ms: u64,
    pub end_ms: u64,
    pub all_day: bool,
    pub location: String,
    /// A Zoom / Meet / Teams / FaceTime link found in the event, if any.
    pub join_url: String,
    pub calendar: String,
    /// "#RRGGBB" of its calendar.
    pub color: String,
    /// The paired Mac it came from ("" = this Mac). Events both Macs have (iCloud) show once,
    /// as this Mac's.
    pub machine: String,
}

/// What the board shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// The first read finished (until then the page says "Loading").
    pub loaded: bool,
    /// Calendar access granted.
    pub authorized: bool,
    /// From two hours ago to a day ahead, soonest first (some may be over already:
    /// use `upcoming` / `next` for what's still to come).
    pub events: Vec<Event>,
    pub error: Option<String>,
    /// A paired Mac whose calendar couldn't be read ("AustinM5Max: ...").
    pub peer_note: Option<String>,
    /// App side: the event picked with the knob (its `id`); "" = the featured one.
    pub selected: String,
    /// App side: events already announced before they start (ids).
    pub announced: Vec<String>,
}

/// Tap index of the join / open button on the big card (rows are 0..).
pub const TAP_JOIN: usize = 1000;

const MIN: u64 = 60_000;
const HOUR: u64 = 60 * MIN;

impl Status {
    /// Timed events that aren't over yet, soonest first: what the knob walks through.
    pub fn upcoming(&self, now: u64) -> Vec<&Event> {
        self.events.iter().filter(|e| !e.all_day && e.end_ms > now).collect()
    }

    /// All-day events going on now.
    pub fn all_day(&self, now: u64) -> Vec<&Event> {
        self.events.iter().filter(|e| e.all_day && e.start_ms <= now && e.end_ms > now).collect()
    }

    /// The next timed event that hasn't started yet.
    pub fn next(&self, now: u64) -> Option<&Event> {
        self.events.iter().find(|e| !e.all_day && e.start_ms > now)
    }

    /// The event the page leads with (an index into `upcoming`).
    pub fn featured(&self, now: u64) -> Option<usize> {
        featured(&self.upcoming(now), now)
    }

    /// The knob's pick (an index into `upcoming`), else the featured event.
    pub fn selection(&self, now: u64) -> Option<usize> {
        let picked = (!self.selected.is_empty()).then(|| self.upcoming(now).iter().position(|e| e.id == self.selected)).flatten();
        picked.or_else(|| self.featured(now))
    }
}

/// Which upcoming event to lead with: one starting within 15 minutes, else a meeting
/// going on now (not an all-afternoon block), else the next one to start.
pub fn featured(up: &[&Event], now: u64) -> Option<usize> {
    let soon = up.iter().position(|e| e.start_ms > now && e.start_ms - now <= 15 * MIN);
    let running = up.iter().position(|e| e.start_ms <= now && e.end_ms.saturating_sub(e.start_ms) <= 3 * HOUR);
    let next = up.iter().position(|e| e.start_ms > now);
    soon.or(running).or(next).or((!up.is_empty()).then_some(0))
}

// ---------------------------------------------------------------- time

fn local(ms: u64) -> libc::tm {
    let t = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    tm
}

fn h12(h: i32) -> i32 {
    match h % 12 {
        0 => 12,
        h => h,
    }
}

/// "9:30" (12-hour, like the board's clock).
pub fn hm(ms: u64) -> String {
    let tm = local(ms);
    format!("{}:{:02}", h12(tm.tm_hour), tm.tm_min)
}

/// "9:30 AM"
pub fn hm_ampm(ms: u64) -> String {
    let tm = local(ms);
    format!("{}:{:02} {}", h12(tm.tm_hour), tm.tm_min, if tm.tm_hour < 12 { "AM" } else { "PM" })
}

/// "9:30 – 10:15 AM" (the AM / PM once when both ends share it).
pub fn span(start: u64, end: u64) -> String {
    let (a, b) = (local(start), local(end));
    if (a.tm_hour < 12) == (b.tm_hour < 12) {
        format!("{} – {}", hm(start), hm_ampm(end))
    } else {
        format!("{} – {}", hm_ampm(start), hm_ampm(end))
    }
}

/// Ruler labels: "10 AM", "11", "12 PM", "1"... (AM / PM at the first one and at 12).
pub fn hour_label(ms: u64, first: bool) -> String {
    let h = local(ms).tm_hour;
    if first || h % 12 == 0 {
        format!("{} {}", h12(h), if h < 12 { "AM" } else { "PM" })
    } else {
        h12(h).to_string()
    }
}

/// The start of the local hour holding `ms`.
pub fn hour_start(ms: u64) -> u64 {
    let tm = local(ms);
    ms - ms % 1000 - (tm.tm_min as u64 * 60 + tm.tm_sec as u64) * 1000
}

/// "Monday, Nov 16"
pub fn day_label(ms: u64) -> String {
    const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let tm = local(ms);
    format!("{}, {} {}", DAYS[tm.tm_wday.clamp(0, 6) as usize], MONTHS[tm.tm_mon.clamp(0, 11) as usize], tm.tm_mday)
}

/// "TUE" when `ms` falls on another local day than `now` (rows from tomorrow say so).
pub fn other_day(now: u64, ms: u64) -> Option<&'static str> {
    const DAYS: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
    (days_apart(now, ms) != 0).then(|| DAYS[local(ms).tm_wday.clamp(0, 6) as usize])
}

fn days_apart(a: u64, b: u64) -> i32 {
    let (x, y) = (local(a), local(b));
    if x.tm_year == y.tm_year {
        y.tm_yday - x.tm_yday
    } else {
        (y.tm_year - x.tm_year).signum() * 400 // far enough: "Thursday 9:30"
    }
}

fn ceil_min(ms: u64) -> u64 {
    ms.div_ceil(MIN)
}

fn h_m(m: u64) -> String {
    match (m / 60, m % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// "in 12 min", "in 1 h 5 min", "now · 23 min left", "ended".
pub fn until(now: u64, start: u64, end: u64) -> String {
    if now < start {
        format!("in {}", h_m(ceil_min(start - now)))
    } else if now < end {
        format!("now · {} left", h_m(ceil_min(end - now)))
    } else {
        "ended".into()
    }
}

/// Compact, for list rows: "12m", "1h05", "now", "done".
pub fn until_short(now: u64, start: u64, end: u64) -> String {
    if now < start {
        let m = ceil_min(start - now);
        if m < 60 {
            format!("{m}m")
        } else {
            format!("{}h{:02}", m / 60, m % 60)
        }
    } else if now < end {
        "now".into()
    } else {
        "done".into()
    }
}

/// `until`, but "at 9:30 AM" / "tomorrow 9:30 AM" once it's ten hours or more away.
pub fn when(now: u64, start: u64, end: u64) -> String {
    if start <= now || start - now < 10 * HOUR {
        return until(now, start, end);
    }
    match days_apart(now, start) {
        0 => format!("at {}", hm_ampm(start)),
        1 => format!("tomorrow {}", hm_ampm(start)),
        _ => format!("{} {}", day_label(start).split(',').next().unwrap_or(""), hm_ampm(start)),
    }
}

// ---------------------------------------------------------------- join links

/// The first meeting link in `texts` (an event's URL, location and notes, in that order).
pub fn join_link<'a>(texts: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let texts: Vec<&str> = texts.into_iter().collect();
    for t in &texts {
        if let Some(u) = urls(t).into_iter().find(|u| is_join(u)) {
            return Some(u);
        }
    }
    // Links written without "https://" ("meet.google.com/abc-defg-hij").
    for t in &texts {
        if let Some(u) = bare_links(t).into_iter().find(|u| is_join(u)) {
            return Some(u);
        }
    }
    None
}

fn url_end(c: char) -> bool {
    c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\' | '^' | '`')
}

fn trim_url(u: &str) -> String {
    u.trim_end_matches(['.', ',', ';', ':', '!', '?']).to_string()
}

/// Every http(s) URL in `text`.
fn urls(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = lower[from..].find("http").map(|i| i + from) {
        let rest = &lower[i..];
        if rest.starts_with("https://") || rest.starts_with("http://") {
            let end = text[i..].find(url_end).map(|e| i + e).unwrap_or(text.len());
            out.push(trim_url(&text[i..end]));
            from = end.max(i + 4);
        } else {
            from = i + 4;
        }
    }
    out
}

/// Known meeting hosts written without a scheme, made into https links.
fn bare_links(text: &str) -> Vec<String> {
    const HOSTS: [&str; 5] = ["meet.google.com/", "zoom.us/", "teams.microsoft.com/", "facetime.apple.com/", "webex.com/"];
    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    for h in HOSTS {
        let mut from = 0;
        while let Some(i) = lower[from..].find(h).map(|i| i + from) {
            // Back up over a subdomain ("us02web.zoom.us").
            let start = lower[..i].char_indices().rev().find(|&(_, c)| !(c.is_ascii_alphanumeric() || c == '.' || c == '-')).map(|(s, c)| s + c.len_utf8()).unwrap_or(0);
            let end = text[i..].find(url_end).map(|e| i + e).unwrap_or(text.len());
            if !lower[..start].ends_with("://") {
                out.push(format!("https://{}", trim_url(&text[start..end])));
            }
            from = end.max(i + h.len());
        }
    }
    out
}

/// (lowercase host, path) of an http(s) URL.
fn host_path(url: &str) -> Option<(String, &str)> {
    let rest = url.split_once("://").map(|(s, r)| (s.eq_ignore_ascii_case("https") || s.eq_ignore_ascii_case("http")).then_some(r)).flatten()?;
    let (auth, path) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host = auth.rsplit('@').next().unwrap_or(auth).split(':').next().unwrap_or("").to_ascii_lowercase();
    Some((host, path))
}

fn on(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

/// Is this a link that joins a meeting (not just any page on the provider's site)?
pub fn is_join(url: &str) -> bool {
    let Some((host, path)) = host_path(url) else { return false };
    let p = path.to_ascii_lowercase();
    if on(&host, "zoom.us") || on(&host, "zoomgov.com") {
        return p.starts_with("/j/") || p.starts_with("/my/") || p.starts_with("/w/") || p.starts_with("/s/");
    }
    if host == "meet.google.com" {
        let code = p.trim_start_matches('/').split(['/', '?', '#']).next().unwrap_or("");
        return code.len() >= 8 && code.contains('-') && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    }
    if on(&host, "teams.microsoft.com") || on(&host, "teams.live.com") {
        return p.starts_with("/l/meetup-join") || p.starts_with("/meet/");
    }
    if host == "facetime.apple.com" {
        return p.starts_with("/join");
    }
    if on(&host, "webex.com") {
        return p.contains("/meet/") || p.contains("/join") || p.contains("j.php") || p.contains("joinservice") || p.contains("/m/");
    }
    if host == "chime.aws" || on(&host, "whereby.com") || on(&host, "around.co") {
        return p.len() > 1;
    }
    false
}

/// "Zoom", "Google Meet", "Teams"... for a join link.
pub fn provider(url: &str) -> &'static str {
    let Some((host, _)) = host_path(url) else { return "Meeting" };
    if on(&host, "zoom.us") || on(&host, "zoomgov.com") {
        "Zoom"
    } else if host == "meet.google.com" {
        "Google Meet"
    } else if on(&host, "teams.microsoft.com") || on(&host, "teams.live.com") {
        "Teams"
    } else if host == "facetime.apple.com" {
        "FaceTime"
    } else if on(&host, "webex.com") {
        "Webex"
    } else if host == "chime.aws" {
        "Chime"
    } else if on(&host, "whereby.com") {
        "Whereby"
    } else if on(&host, "around.co") {
        "Around"
    } else {
        "Meeting"
    }
}

/// The same meeting as a link its app opens directly (no browser tab, no "Open zoom.us?"
/// prompt): zoommtg:// for Zoom meeting ids, msteams: for Teams. None = use the link as is.
pub fn app_link(url: &str) -> Option<String> {
    let (host, path) = host_path(url)?;
    if on(&host, "zoom.us") || on(&host, "zoomgov.com") {
        let rest = path.strip_prefix("/j/")?;
        let (id, query) = rest.split_once('?').unwrap_or((rest, ""));
        let id: String = id.chars().take_while(|c| c.is_ascii_digit()).collect();
        if id.is_empty() {
            return None;
        }
        let pwd = query.split('&').find_map(|kv| kv.strip_prefix("pwd=")).map(|p| p.split('#').next().unwrap_or(p));
        return Some(match pwd {
            Some(p) if !p.is_empty() => format!("zoommtg://{host}/join?action=join&confno={id}&pwd={p}"),
            _ => format!("zoommtg://{host}/join?action=join&confno={id}"),
        });
    }
    if host == "teams.microsoft.com" && path.starts_with("/l/meetup-join") {
        return Some(format!("msteams:{path}"));
    }
    None
}

/// Opens the event in Calendar.
pub fn event_link(id: &str) -> String {
    let ek = id.rsplit_once('@').map(|(e, _)| e).unwrap_or(id);
    let enc: String = ek.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    format!("ical://ekevent/{enc}?method=show&options=more")
}

// ---------------------------------------------------------------- the thread

#[derive(Debug)]
pub enum Cmd {
    Configure(Cfg),
    /// Read the calendar again now (the page was shown, or the database changed).
    Refresh,
    /// The paired Macs to ask for their events (name, host, port) and the shared key.
    Peers(Vec<(String, String, u16)>, String),
    /// (internal) A paired Mac answered `ctl calendar`, or didn't.
    PeerReply(String, Result<Vec<Event>, String>),
}

/// What the module's thread sends the app (handled in app/calendar.rs on the main thread).
#[derive(Debug)]
pub enum Update {
    Status(Status),
}

const REFRESH: Duration = Duration::from_secs(60);
const BACK: u64 = 2 * HOUR;
const AHEAD: u64 = 24 * HOUR;
const MAX_EVENTS: usize = 80;

/// Starts the module's background work; `emit` hands updates to the app.
pub fn spawn(cfg: Cfg, emit: impl Fn(Update) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    let me = tx.clone();
    std::thread::Builder::new().name("calendar".into()).spawn(move || run(cfg, me, rx, emit)).unwrap();
    tx
}

/// A paired Mac's events stay up for this long after it stops answering.
const PEER_STALE: Duration = Duration::from_secs(10 * 60);

fn run(mut cfg: Cfg, me: Sender<Cmd>, rx: Receiver<Cmd>, emit: impl Fn(Update)) {
    let mut store: Option<ek::Store> = None;
    let mut last: Option<Status> = None;
    let mut local = Status::default();
    let mut peers: Vec<(String, String, u16)> = Vec::new();
    let mut key = String::new();
    // Each paired Mac's last good answer (and when), and its latest error.
    let mut good: HashMap<String, (Vec<Event>, Instant)> = HashMap::new();
    let mut errors: HashMap<String, String> = HashMap::new();
    let mut asking: HashSet<String> = HashSet::new();
    let mut next_read = Instant::now();
    let mut next_peers = Instant::now();
    loop {
        let now = Instant::now();
        if now >= next_read {
            local = if cfg.enabled { read(&cfg, &mut store, &me) } else { Status { loaded: true, error: Some("Calendar is off in Quaked Settings".into()), ..Default::default() } };
            next_read = now + if cfg.enabled { REFRESH } else { Duration::from_secs(3600) };
        }
        if cfg.enabled && now >= next_peers {
            for (name, host, port) in &peers {
                if key.is_empty() || !asking.insert(name.clone()) {
                    continue;
                }
                let (name, host, port, key, back) = (name.clone(), host.clone(), *port, key.clone(), me.clone());
                std::thread::Builder::new()
                    .name("calendar-peer".into())
                    .spawn(move || {
                        let reply = crate::sync::request(&host, port, &key, "ctl calendar").and_then(|r| parse_peer(&r));
                        let _ = back.send(Cmd::PeerReply(name, reply));
                    })
                    .ok();
            }
            next_peers = now + REFRESH;
        }
        let shared: Vec<(&str, &[Event])> = if cfg.enabled {
            peers.iter().filter_map(|(n, _, _)| good.get(n).filter(|(_, at)| at.elapsed() < PEER_STALE).map(|(ev, _)| (n.as_str(), ev.as_slice()))).collect()
        } else {
            Vec::new()
        };
        let mut st = local.clone();
        st.events = merge(&local.events, &shared);
        // This Mac without calendar access still shows the other Mac's events.
        if !st.authorized && !shared.is_empty() {
            st.authorized = true;
        }
        st.peer_note = if cfg.enabled { peers.iter().find_map(|(n, _, _)| errors.get(n).map(|e| format!("{n}: {e}"))) } else { None };
        if last.as_ref() != Some(&st) {
            emit(Update::Status(st.clone()));
            last = Some(st);
        }
        let wake = if cfg.enabled { next_read.min(next_peers) } else { next_read };
        let first = match rx.recv_timeout(wake.saturating_duration_since(Instant::now())) {
            Ok(cmd) => cmd,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let mut cmds = vec![first];
        while let Ok(cmd) = rx.try_recv() {
            cmds.push(cmd);
        }
        for cmd in cmds {
            match cmd {
                Cmd::Configure(c) => {
                    cfg = c;
                    next_read = Instant::now();
                    next_peers = Instant::now();
                }
                // Changes come in bursts (a sync touches many events): read once after them.
                Cmd::Refresh => next_read = next_read.min(Instant::now() + Duration::from_millis(300)),
                Cmd::Peers(p, k) => {
                    if p != peers || k != key {
                        good.retain(|n, _| p.iter().any(|(m, _, _)| m == n));
                        errors.retain(|n, _| p.iter().any(|(m, _, _)| m == n));
                        peers = p;
                        key = k;
                        next_peers = Instant::now();
                    }
                }
                Cmd::PeerReply(name, reply) => {
                    asking.remove(&name);
                    if !peers.iter().any(|(n, _, _)| *n == name) {
                        continue;
                    }
                    match reply {
                        Ok(events) => {
                            errors.remove(&name);
                            good.insert(name, (events, Instant::now()));
                        }
                        Err(e) => {
                            if errors.get(&name) != Some(&e) {
                                crate::device::log(format!("calendar: {name}: {e}"));
                            }
                            errors.insert(name, e);
                        }
                    }
                }
            }
        }
    }
}

/// This Mac's events plus the paired Macs', soonest first. An event a paired Mac also has
/// (same title, start and end: a shared or iCloud calendar on both) shows once, as this
/// Mac's; the others get the Mac's name in `machine` and an id of their own.
pub fn merge(local: &[Event], peers: &[(&str, &[Event])]) -> Vec<Event> {
    let key = |e: &Event| (e.title.trim().to_lowercase(), e.start_ms, e.end_ms, e.all_day);
    let mut seen: HashSet<(String, u64, u64, bool)> = local.iter().map(key).collect();
    let mut out: Vec<Event> = local.to_vec();
    for (name, events) in peers {
        for e in events.iter().filter(|e| e.machine.is_empty()) {
            if seen.insert(key(e)) {
                out.push(Event { id: format!("{name}/{}", e.id), machine: name.to_string(), ..e.clone() });
            }
        }
    }
    out.sort_by(|a, b| a.start_ms.cmp(&b.start_ms).then(a.end_ms.cmp(&b.end_ms)).then(a.title.cmp(&b.title)));
    out.truncate(MAX_EVENTS);
    out
}

/// What this Mac answers a paired Mac's `ctl calendar`: its own events only (never the
/// ones it got from other Macs, so nothing echoes back and forth).
pub fn peer_json(st: &Status, enabled: bool) -> String {
    if !enabled {
        return "error: Calendar is off there".into();
    }
    if !st.loaded {
        return "error: not read yet".into();
    }
    if !st.authorized || (st.events.iter().all(|e| !e.machine.is_empty()) && st.error.is_some()) {
        return format!("error: {}", st.error.as_deref().unwrap_or("no calendar access there"));
    }
    let own: Vec<&Event> = st.events.iter().filter(|e| e.machine.is_empty()).collect();
    serde_json::json!({ "events": own }).to_string()
}

/// A paired Mac's answer to `ctl calendar`.
pub fn parse_peer(reply: &[u8]) -> Result<Vec<Event>, String> {
    let text = String::from_utf8_lossy(reply);
    let text = text.trim();
    if let Some(e) = text.strip_prefix("error:") {
        return Err(if e.contains("unknown command") { "its quaked is older; update it".into() } else { e.trim().to_string() });
    }
    #[derive(Deserialize)]
    struct Reply {
        events: Vec<Event>,
    }
    serde_json::from_str::<Reply>(text).map(|r| r.events).map_err(|_| "unexpected reply".to_string())
}

fn read(cfg: &Cfg, store: &mut Option<ek::Store>, me: &Sender<Cmd>) -> Status {
    let mut access = ek::access();
    if access == ek::NOT_DETERMINED {
        // The system asks once; this waits for the answer.
        ek::request();
        access = ek::access();
    }
    if access != ek::FULL_ACCESS {
        *store = None;
        let why = match access {
            ek::WRITE_ONLY => "Quaked can only add events: allow full access in System Settings › Privacy & Security › Calendars",
            ek::RESTRICTED => "Calendar access is restricted on this Mac",
            _ => "Allow Quaked in System Settings › Privacy & Security › Calendars",
        };
        return Status { loaded: true, error: Some(why.into()), ..Default::default() };
    }
    if store.is_none() {
        *store = ek::Store::open(me.clone());
    }
    let Some(s) = store.as_ref() else {
        return Status { loaded: true, authorized: true, error: Some("Couldn't open the calendar".into()), ..Default::default() };
    };
    let now = crate::status::now_ms();
    match s.events(now.saturating_sub(BACK), now + AHEAD, &cfg.calendars) {
        Ok(mut events) => {
            events.sort_by(|a, b| a.start_ms.cmp(&b.start_ms).then(a.end_ms.cmp(&b.end_ms)).then(a.title.cmp(&b.title)));
            events.truncate(MAX_EVENTS);
            Status { loaded: true, authorized: true, events, ..Default::default() }
        }
        Err(e) => Status { loaded: true, authorized: true, error: Some(e), ..Default::default() },
    }
}

/// EventKit through objc2 (calendar thread only).
mod ek {
    use super::{join_link, Cmd, Event};
    use block2::RcBlock;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send, sel};
    use std::ffi::{c_char, CStr};
    use std::sync::mpsc::Sender;
    use std::time::Duration;

    #[link(name = "EventKit", kind = "framework")]
    extern "C" {
        static EKEventStoreChangedNotification: *const AnyObject;
    }

    const ENTITY_EVENT: usize = 0; // EKEntityTypeEvent
    // EKAuthorizationStatus
    pub const NOT_DETERMINED: isize = 0;
    pub const RESTRICTED: isize = 1;
    pub const FULL_ACCESS: isize = 3; // also the old "authorized"
    pub const WRITE_ONLY: isize = 4;
    const CANCELED: isize = 3; // EKEventStatusCanceled
    const DECLINED: isize = 3; // EKParticipantStatusDeclined

    unsafe fn string(s: *mut AnyObject) -> String {
        if s.is_null() {
            return String::new();
        }
        let p: *const c_char = msg_send![s, UTF8String];
        if p.is_null() {
            String::new()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }

    unsafe fn millis(date: *mut AnyObject) -> u64 {
        if date.is_null() {
            return 0;
        }
        let t: f64 = msg_send![date, timeIntervalSince1970];
        (t * 1000.0).max(0.0) as u64
    }

    unsafe fn count(a: *mut AnyObject) -> usize {
        if a.is_null() {
            0
        } else {
            msg_send![a, count]
        }
    }

    unsafe fn at(a: *mut AnyObject, i: usize) -> *mut AnyObject {
        msg_send![a, objectAtIndex: i]
    }

    pub fn access() -> isize {
        unsafe { msg_send![class!(EKEventStore), authorizationStatusForEntityType: ENTITY_EVENT] }
    }

    /// Shows the system's calendar prompt (only the first time) and waits for the answer.
    pub fn request() -> bool {
        autoreleasepool(|_| unsafe {
            let store: *mut AnyObject = msg_send![class!(EKEventStore), new];
            if store.is_null() {
                return false;
            }
            let (tx, rx) = std::sync::mpsc::channel::<bool>();
            let done: RcBlock<dyn Fn(Bool, *mut AnyObject)> = RcBlock::new(move |granted: Bool, _error: *mut AnyObject| {
                let _ = tx.send(granted.as_bool());
            });
            let full: Bool = msg_send![store, respondsToSelector: sel!(requestFullAccessToEventsWithCompletion:)];
            if full.as_bool() {
                let _: () = msg_send![store, requestFullAccessToEventsWithCompletion: &*done];
            } else {
                let _: () = msg_send![store, requestAccessToEntityType: ENTITY_EVENT, completion: &*done];
            }
            match rx.recv_timeout(Duration::from_secs(600)) {
                Ok(granted) => {
                    let _: () = msg_send![store, release];
                    granted
                }
                Err(_) => false, // still asking: leave the store alone
            }
        })
    }

    /// An event store, alive for the thread's life.
    pub struct Store {
        store: *mut AnyObject,
    }

    impl Store {
        /// Opens the store; changes to the calendar database send `Cmd::Refresh` to `refresh`.
        pub fn open(refresh: Sender<Cmd>) -> Option<Store> {
            autoreleasepool(|_| unsafe {
                let store: *mut AnyObject = msg_send![class!(EKEventStore), new];
                if store.is_null() {
                    return None;
                }
                let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
                let changed: RcBlock<dyn Fn(*mut AnyObject)> = RcBlock::new(move |_note: *mut AnyObject| {
                    let _ = refresh.send(Cmd::Refresh);
                });
                let _token: *mut AnyObject = msg_send![center,
                    addObserverForName: EKEventStoreChangedNotification,
                    object: store,
                    queue: std::ptr::null_mut::<AnyObject>(),
                    usingBlock: &*changed];
                Some(Store { store })
            })
        }

        /// Events overlapping `from..to` in the calendars named `only` (empty = all).
        pub fn events(&self, from: u64, to: u64, only: &[String]) -> Result<Vec<Event>, String> {
            autoreleasepool(|_| unsafe {
                let cals: *mut AnyObject = msg_send![self.store, calendarsForEntityType: ENTITY_EVENT];
                let total = count(cals);
                let picked: Vec<*mut AnyObject> = (0..total)
                    .map(|i| at(cals, i))
                    .filter(|&c| only.is_empty() || {
                        let title = string(msg_send![c, title]);
                        only.iter().any(|o| o.trim().eq_ignore_ascii_case(title.trim()))
                    })
                    .collect();
                if picked.is_empty() {
                    return if total == 0 || only.is_empty() { Ok(Vec::new()) } else { Err(format!("No calendar named {}", only.join(", "))) };
                }
                let list: *mut AnyObject = msg_send![class!(NSArray), arrayWithObjects: picked.as_ptr(), count: picked.len()];
                let start: *mut AnyObject = msg_send![class!(NSDate), dateWithTimeIntervalSince1970: from as f64 / 1000.0];
                let end: *mut AnyObject = msg_send![class!(NSDate), dateWithTimeIntervalSince1970: to as f64 / 1000.0];
                let pred: *mut AnyObject = msg_send![self.store, predicateForEventsWithStartDate: start, endDate: end, calendars: list];
                if pred.is_null() {
                    return Err("Calendar query failed".into());
                }
                let found: *mut AnyObject = msg_send![self.store, eventsMatchingPredicate: pred];
                Ok((0..count(found)).filter_map(|i| event(at(found, i))).collect())
            })
        }
    }

    unsafe fn declined(e: *mut AnyObject) -> bool {
        let people: *mut AnyObject = msg_send![e, attendees];
        for i in 0..count(people) {
            let p = at(people, i);
            let me: Bool = msg_send![p, isCurrentUser];
            if me.as_bool() {
                let st: isize = msg_send![p, participantStatus];
                return st == DECLINED;
            }
        }
        false
    }

    /// The calendar's color as "#RRGGBB" ("" if it has none).
    unsafe fn color(cal: *mut AnyObject) -> String {
        if cal.is_null() {
            return String::new();
        }
        let c: *mut AnyObject = msg_send![cal, color];
        if c.is_null() {
            return String::new();
        }
        let space: *mut AnyObject = msg_send![class!(NSColorSpace), sRGBColorSpace];
        let c: *mut AnyObject = msg_send![c, colorUsingColorSpace: space];
        if c.is_null() {
            return String::new();
        }
        let (r, g, b): (f64, f64, f64) = (msg_send![c, redComponent], msg_send![c, greenComponent], msg_send![c, blueComponent]);
        let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("#{:02X}{:02X}{:02X}", byte(r), byte(g), byte(b))
    }

    unsafe fn event(e: *mut AnyObject) -> Option<Event> {
        if e.is_null() {
            return None;
        }
        let status: isize = msg_send![e, status];
        if status == CANCELED || declined(e) {
            return None;
        }
        let title = string(msg_send![e, title]);
        let start_ms = millis(msg_send![e, startDate]);
        let end_ms = millis(msg_send![e, endDate]);
        let all_day: Bool = msg_send![e, isAllDay];
        let location = string(msg_send![e, location]);
        let url: *mut AnyObject = msg_send![e, URL];
        let url = if url.is_null() { String::new() } else { string(msg_send![url, absoluteString]) };
        let notes = string(msg_send![e, notes]);
        let cal: *mut AnyObject = msg_send![e, calendar];
        let calendar = if cal.is_null() { String::new() } else { string(msg_send![cal, title]) };
        let ident = string(msg_send![e, eventIdentifier]);
        Some(Event {
            id: format!("{ident}@{start_ms}"),
            title: if title.trim().is_empty() { "(No title)".into() } else { title.trim().to_string() },
            start_ms,
            end_ms: end_ms.max(start_ms),
            all_day: all_day.as_bool(),
            join_url: join_link([url.as_str(), location.as_str(), notes.as_str()]).unwrap_or_default(),
            location: location.trim().to_string(),
            calendar,
            color: color(cal),
            machine: String::new(),
        })
    }
}

/// Sample data for `quaked preview`: a real-looking afternoon around now.
pub fn sample() -> Status {
    let now = crate::status::now_ms();
    // Meetings start on five-minute marks, the next one 10-15 minutes out.
    let mark = now - now % (5 * MIN) + 15 * MIN;
    let ev = |id: &str, title: &str, start: u64, mins: u64, cal: &str, color: &str, location: &str, join: &str| Event {
        id: id.into(),
        title: title.into(),
        start_ms: start,
        end_ms: start + mins * MIN,
        all_day: false,
        location: location.into(),
        join_url: join.into(),
        calendar: cal.into(),
        color: color.into(),
        machine: String::new(),
    };
    let day = now - now % (24 * HOUR);
    let events = vec![
        Event { id: "bday".into(), title: "Sam's birthday".into(), start_ms: day, end_ms: day + 24 * HOUR, all_day: true, calendar: "Family".into(), color: "#FF9F0A".into(), ..Default::default() },
        ev("standup", "Standup", mark - 105 * MIN, 15, "Work", "#4C8DFF", "", "https://meet.google.com/abc-defg-hij"),
        ev("priya", "1:1 with Priya", mark - 40 * MIN, 30, "Work", "#4C8DFF", "", "https://meet.google.com/kqr-mnop-wxy"),
        ev("review", "Design review: soundboard v2", mark, 45, "Work", "#4C8DFF", "https://us02web.zoom.us/j/84512345678?pwd=Zm9vYmFy", "https://us02web.zoom.us/j/84512345678?pwd=Zm9vYmFy"),
        ev("lunch", "Lunch with Dana", mark + 75 * MIN, 60, "Personal", "#2F9E5B", "Pita Jungle", ""),
        ev("planning", "Sprint planning", mark + 165 * MIN, 60, "Work", "#4C8DFF", "Microsoft Teams Meeting", "https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc%40thread.v2/0"),
        ev("dentist", "Dentist", mark + 285 * MIN, 45, "Personal", "#2F9E5B", "Smile Dental, 4th Ave", ""),
        ev("deploy", "Deploy window: platform", mark + 360 * MIN, 30, "On call", "#B36BFF", "", ""),
        ev("gym", "Climbing", mark + 465 * MIN, 90, "Personal", "#2F9E5B", "Momentum", ""),
    ];
    Status { loaded: true, authorized: true, events, error: None, ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(title: &str, start: u64, machine: &str) -> Event {
        Event { id: format!("id-{title}"), title: title.into(), start_ms: start, end_ms: start + 1_800_000, machine: machine.into(), ..Default::default() }
    }

    #[test]
    fn merges_both_macs_once() {
        let local = vec![ev("Standup", 1_000, ""), ev("Piano lesson", 9_000, "")];
        let theirs = vec![ev("standup ", 1_000, ""), ev("Design review", 5_000, ""), ev("Echo", 7_000, "Mac mini")];
        let all = merge(&local, &[("AustinM5Max", &theirs)]);
        let titles: Vec<&str> = all.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["Standup", "Design review", "Piano lesson"]);
        assert_eq!(all[0].machine, "");
        assert_eq!(all[1].machine, "AustinM5Max");
        assert_eq!(all[1].id, "AustinM5Max/id-Design review");
    }

    #[test]
    fn answers_peers_with_own_events_only() {
        let st = Status { loaded: true, authorized: true, events: vec![ev("Mine", 1, ""), ev("Theirs", 2, "AustinM5Max")], ..Default::default() };
        let back = parse_peer(peer_json(&st, true).as_bytes()).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].title, "Mine");
        assert_eq!(parse_peer(b"error: unknown command \"calendar\"\n"), Err("its quaked is older; update it".into()));
        assert!(peer_json(&st, false).starts_with("error:"));
        let denied = Status { loaded: true, error: Some("Allow Quaked".into()), ..Default::default() };
        assert_eq!(peer_json(&denied, true), "error: Allow Quaked");
    }


    #[test]
    fn finds_join_links() {
        let zoom = "https://us02web.zoom.us/j/84512345678?pwd=Zm9vYmFy";
        assert_eq!(join_link([zoom, "", ""]).as_deref(), Some(zoom));
        // In notes, after other links, inside angle brackets (Outlook) and with trailing punctuation.
        let notes = "Agenda: https://docs.google.com/doc/123\nJoin: <https://teams.microsoft.com/l/meetup-join/19%3ameeting_x%40thread.v2/0?context=%7b%22Tid%22%7d>.";
        assert_eq!(join_link(["", "Room 4B", notes]).as_deref(), Some("https://teams.microsoft.com/l/meetup-join/19%3ameeting_x%40thread.v2/0?context=%7b%22Tid%22%7d"));
        assert_eq!(join_link(["", "https://meet.google.com/abc-defg-hij.", ""]).as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        assert_eq!(join_link(["", "", "FaceTime: https://facetime.apple.com/join#v=1&p=AbC"]).as_deref(), Some("https://facetime.apple.com/join#v=1&p=AbC"));
        assert_eq!(join_link(["", "", "Webex (https://acme.webex.com/acme/j.php?MTID=m123)"]).as_deref(), Some("https://acme.webex.com/acme/j.php?MTID=m123"));
        // The event URL wins over the notes.
        assert_eq!(join_link(["https://meet.google.com/aaa-bbbb-ccc", "", zoom]).as_deref(), Some("https://meet.google.com/aaa-bbbb-ccc"));
        // Written without a scheme.
        assert_eq!(join_link(["", "meet.google.com/xyz-abcd-efg", ""]).as_deref(), Some("https://meet.google.com/xyz-abcd-efg"));
        assert_eq!(join_link(["", "", "dial in or go to us06web.zoom.us/j/123456789"]).as_deref(), Some("https://us06web.zoom.us/j/123456789"));
        // Not meeting links.
        assert_eq!(join_link(["https://zoom.us/pricing", "https://meet.google.com/", "https://teams.microsoft.com/v2/"]), None);
        assert_eq!(join_link(["", "Conference room", "no links here"]), None);
        assert_eq!(join_link(["", "", "https://evilzoom.us/j/123"]), None);
    }

    #[test]
    fn providers_and_app_links() {
        assert_eq!(provider("https://us02web.zoom.us/j/1"), "Zoom");
        assert_eq!(provider("https://meet.google.com/abc-defg-hij"), "Google Meet");
        assert_eq!(provider("https://teams.microsoft.com/l/meetup-join/x"), "Teams");
        assert_eq!(provider("https://facetime.apple.com/join#v=1"), "FaceTime");
        assert_eq!(provider("https://acme.webex.com/meet/bob"), "Webex");
        assert_eq!(provider("https://example.com/x"), "Meeting");
        assert_eq!(app_link("https://us02web.zoom.us/j/84512345678?pwd=Zm9vYmFy").as_deref(), Some("zoommtg://us02web.zoom.us/join?action=join&confno=84512345678&pwd=Zm9vYmFy"));
        assert_eq!(app_link("https://zoom.us/j/123456789").as_deref(), Some("zoommtg://zoom.us/join?action=join&confno=123456789"));
        assert_eq!(app_link("https://zoom.us/my/austin"), None);
        assert_eq!(app_link("https://teams.microsoft.com/l/meetup-join/19%3a/0?context=x").as_deref(), Some("msteams:/l/meetup-join/19%3a/0?context=x"));
        assert_eq!(app_link("https://meet.google.com/abc-defg-hij"), None);
        assert_eq!(event_link("ABC:12/3@1700000000000"), "ical://ekevent/ABC%3A12%2F3?method=show&options=more");
    }

    #[test]
    fn countdowns() {
        let t = 1_800_000_000_000u64;
        assert_eq!(until(t, t + 12 * MIN, t + 40 * MIN), "in 12 min");
        assert_eq!(until(t, t + 11 * MIN + 1, t + 40 * MIN), "in 12 min"); // rounds up
        assert_eq!(until(t, t + 30_000, t + 40 * MIN), "in 1 min");
        assert_eq!(until(t, t + 65 * MIN, t + 90 * MIN), "in 1 h 5 min");
        assert_eq!(until(t, t + 3 * HOUR, t + 4 * HOUR), "in 3 h");
        assert_eq!(until(t, t - 7 * MIN, t + 23 * MIN), "now · 23 min left");
        assert_eq!(until(t, t - 7 * MIN, t + 83 * MIN), "now · 1 h 23 min left");
        assert_eq!(until(t, t - 60 * MIN, t), "ended");
        assert_eq!(until_short(t, t + 12 * MIN, t + 40 * MIN), "12m");
        assert_eq!(until_short(t, t + 65 * MIN, t + 90 * MIN), "1h05");
        assert_eq!(until_short(t, t - MIN, t + MIN), "now");
        assert_eq!(when(t, t + 12 * MIN, t + 40 * MIN), "in 12 min");
        assert!(when(t, t + 11 * HOUR, t + 12 * HOUR).contains(':'));
        assert_eq!(hour_start(t + 17 * MIN + 4_321) % MIN, 0);
    }

    fn at(title: &str, start: u64, mins: u64) -> Event {
        Event { id: title.into(), title: title.into(), start_ms: start, end_ms: start + mins * MIN, ..Default::default() }
    }

    #[test]
    fn leads_with_the_right_event() {
        let t = 1_800_000_000_000u64;
        let st = |events: Vec<Event>| Status { events, ..Default::default() };
        // A meeting going on now beats the next one an hour out.
        let s = st(vec![at("now", t - 10 * MIN, 30), at("later", t + 60 * MIN, 30)]);
        assert_eq!(s.featured(t), Some(0));
        // ...but one about to start wins.
        let s = st(vec![at("now", t - 10 * MIN, 30), at("soon", t + 5 * MIN, 30)]);
        assert_eq!(s.featured(t), Some(1));
        // An all-afternoon block doesn't hide the next meeting.
        let s = st(vec![at("focus block", t - 60 * MIN, 300), at("1:1", t + 90 * MIN, 30)]);
        assert_eq!(s.featured(t), Some(1));
        // Over and all-day events aren't upcoming.
        let mut allday = at("holiday", t - HOUR, 24 * 60);
        allday.all_day = true;
        let s = st(vec![at("over", t - 60 * MIN, 30), allday, at("next", t + 20 * MIN, 30)]);
        assert_eq!(s.upcoming(t).len(), 1);
        assert_eq!(s.all_day(t).len(), 1);
        assert_eq!(s.next(t).map(|e| e.title.as_str()), Some("next"));
        assert_eq!(st(vec![]).featured(t), None);
        // The knob's pick, while it's still upcoming.
        let mut s = st(vec![at("a", t + 5 * MIN, 30), at("b", t + 60 * MIN, 30)]);
        s.selected = "b".into();
        assert_eq!(s.selection(t), Some(1));
        s.selected = "gone".into();
        assert_eq!(s.selection(t), Some(0));
    }
}
