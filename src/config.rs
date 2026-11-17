//! User settings: ~/Library/Application Support/Quaked/config.json (camelCase JSON),
//! edited on the board, in the settings page quaked serves, or by hand. Paired Macs keep
//! the same settings: the newest `updatedAt` wins (each keeps its own `sync` section).
//! The daemon writes defaults if the file is missing and reloads it when it changes.
//! Unknown fields are ignored; missing fields take the defaults below.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::SystemTime;

pub fn dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join("Library/Application Support/Quaked")
}

pub fn path() -> PathBuf {
    dir().join("config.json")
}

/// Expands a leading "~/" to the home folder.
pub fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest),
        None => PathBuf::from(p),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "lowercase")]
pub enum PageId {
    Dash,
    Ai,
    Music,
    Shortcuts,
    Soundboard,
    League,
    Docker,
    Ops,
    Code,
    Digin,
    Focus,
    Deploy,
    Paste,
    Windows,
    Calendar,
    Health,
    Settings,
}

impl PageId {
    /// The name in menus and lists ("Dashboard", "Sounds").
    pub fn title(self) -> &'static str {
        match self {
            PageId::Dash => "Dashboard",
            PageId::Ai => "AI",
            PageId::Music => "Music",
            PageId::Shortcuts => "Shortcuts",
            PageId::Soundboard => "Sounds",
            PageId::League => "League",
            PageId::Docker => "Docker",
            PageId::Ops => "Ops",
            PageId::Code => "Code",
            PageId::Digin => "Digin",
            PageId::Focus => "Focus",
            PageId::Deploy => "Deploy",
            PageId::Paste => "Clipboard",
            PageId::Windows => "Windows",
            PageId::Calendar => "Calendar",
            PageId::Health => "Health",
            PageId::Settings => "Settings",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PageId::Dash => "HOME",
            PageId::Ai => "AI",
            PageId::Music => "MUSIC",
            PageId::Shortcuts => "SHORTCUTS",
            PageId::Soundboard => "SOUNDS",
            PageId::League => "LEAGUE",
            PageId::Docker => "DOCKER",
            PageId::Ops => "OPS",
            PageId::Code => "CODE",
            PageId::Digin => "DIGIN",
            PageId::Focus => "FOCUS",
            PageId::Deploy => "SHIP",
            PageId::Paste => "PASTE",
            PageId::Windows => "WINDOWS",
            PageId::Calendar => "CAL",
            PageId::Health => "HEALTH",
            PageId::Settings => "SETTINGS",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub version: u32,
    /// When these settings last changed (unix ms); paired Macs take the newest.
    pub updated_at: u64,
    /// Chirp when a Claude session starts waiting on you.
    pub buzzer: bool,
    /// Seconds without input before the board returns to the top level.
    pub idle_seconds: u64,
    /// After `idle_seconds` without input, go back to the dashboard (when it's shown);
    /// otherwise just back to the top level of the current page.
    pub idle_home: bool,
    /// Pages in knob order. Leave one out to hide it.
    pub pages: Vec<PageId>,
    /// Show quaked's icon in the menu bar.
    pub menu_bar: bool,
    /// Drop a Claude session from the board once it has been done / idle this long
    /// (0 = keep it). It comes back as soon as you send it a prompt.
    pub session_idle_minutes: u64,
    pub ring: Ring,
    pub touch: Touch,
    pub shortcuts: Vec<Shortcut>,
    pub soundboard: Soundboard,
    pub league: League,
    pub docker: Docker,
    pub ops: Ops,
    pub code: Code,
    pub alerts: Alerts,
    pub ask: AskSettings,
    pub digin: Digin,
    pub call: CallSettings,
    pub focus: FocusSettings,
    pub deploy: DeploySettings,
    pub paste: PasteSettings,
    pub windows: WindowsSettings,
    pub calendar: CalendarSettings,
    pub health: HealthSettings,
    pub night: NightSettings,
    pub viz: VizSettings,
    pub sync: SyncSettings,
    pub device: DeviceSettings,
}

/// Servers, sites and network for the OPS page.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Ops {
    /// SSH host aliases (from ~/.ssh/config). Checked for reachability; with `sshStats`
    /// also load, memory, disk and GPU over one shared SSH connection per host.
    pub hosts: Vec<String>,
    pub ssh_stats: bool,
    /// URLs checked for HTTP status, response time and TLS expiry.
    pub sites: Vec<String>,
}

impl Default for Ops {
    fn default() -> Self {
        Ops {
            hosts: ["workbench", "shebash-gpu", "spark.d3d.dev", "pi"].map(String::from).to_vec(),
            ssh_stats: true,
            sites: ["https://arlint.dev", "https://digin.work", "https://ibtg.notdone.dev", "https://plcplayground.notdone.dev"].map(String::from).to_vec(),
        }
    }
}

/// GitHub pull requests for the CODE page (through the `gh` CLI's login).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Code {
    pub refresh_seconds: u64,
    /// Include GitLab merge requests (through glab).
    pub gitlab: bool,
    /// GitLab hosts to ask; empty = every host in glab's config.
    pub gitlab_hosts: Vec<String>,
}

impl Default for Code {
    fn default() -> Self {
        Code { refresh_seconds: 60, gitlab: true, gitlab_hosts: Vec::new() }
    }
}

// ---------------------------------------------------------------- feature settings
// One section per feature; each feature's agent may add fields to its own section.

/// Call mode: the call screen while you're on a call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct CallSettings {
    pub enabled: bool,
    /// Count a web browser holding the microphone as a call (Meet, Teams or Zoom in a tab).
    pub browsers: bool,
    /// More call apps, by bundle id ("com.example.Meetings").
    pub apps: Vec<String>,
    /// When the soundboard isn't carrying your mic, a press brings the call app forward
    /// and types its own mute shortcut (Zoom ⌘⇧A, Teams / Webex / Discord ⌘⇧M, Slack
    /// ⌘⇧Space). Off: a press only brings the call app forward so you can mute there.
    pub mute_shortcut: bool,
}
impl Default for CallSettings {
    fn default() -> Self {
        CallSettings { enabled: true, browsers: true, apps: Vec::new(), mute_shortcut: false }
    }
}

/// Focus timer on the knob and ring.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct FocusSettings {
    pub minutes: u64,
    /// macOS Shortcuts run when a focus block starts / ends (e.g. turning Do Not Disturb on / off).
    pub start_shortcut: String,
    pub end_shortcut: String,
}
impl Default for FocusSettings {
    fn default() -> Self {
        FocusSettings { minutes: 25, start_shortcut: String::new(), end_shortcut: String::new() }
    }
}

/// Deploy and CI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DeploySettings {
    pub projects: Vec<DeployProject>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DeployProject {
    pub name: String,
    /// The project folder ("~/ArlintDev/plcplayground").
    pub path: String,
    /// What deploys it, run in `path` ("./deploy.sh").
    pub command: String,
    /// GitHub repo for CI status ("arlintdev/plcplayground"); empty = from the folder's git remote.
    pub repo: String,
}

/// Clipboard picker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct PasteSettings {
    pub enabled: bool,
    /// How many recent copies to keep (in memory only).
    pub history: usize,
}
impl Default for PasteSettings {
    fn default() -> Self {
        PasteSettings { enabled: true, history: 20 }
    }
}

/// Window switcher.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct WindowsSettings {
    pub enabled: bool,
}
impl Default for WindowsSettings {
    fn default() -> Self {
        WindowsSettings { enabled: true }
    }
}

/// Calendar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct CalendarSettings {
    pub enabled: bool,
    /// Calendar names to show; empty = all.
    pub calendars: Vec<String>,
    /// Minutes before a meeting with a join link to blink the ring and show the
    /// Calendar page (0 = never).
    pub alert_minutes: u64,
}
impl Default for CalendarSettings {
    fn default() -> Self {
        CalendarSettings { enabled: true, calendars: Vec::new(), alert_minutes: 2 }
    }
}

/// Mac health (this Mac and paired ones).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct HealthSettings {
    pub enabled: bool,
}
impl Default for HealthSettings {
    fn default() -> Self {
        HealthSettings { enabled: true }
    }
}

/// Night mode: the ambient screen when idle at night.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct NightSettings {
    pub enabled: bool,
    /// Night runs from start_hour to end_hour (local time, 0-23).
    pub start_hour: u32,
    pub end_hour: u32,
    /// Minutes without input before it comes on.
    pub idle_minutes: u64,
    /// For the weather; unset = estimated from the network.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}
impl Default for NightSettings {
    fn default() -> Self {
        NightSettings { enabled: true, start_hour: 22, end_hour: 7, idle_minutes: 10, latitude: None, longitude: None }
    }
}

/// Music visualizer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct VizSettings {
    pub enabled: bool,
    /// Pulse the ring on the beat.
    pub ring: bool,
}
impl Default for VizSettings {
    fn default() -> Self {
        VizSettings { enabled: true, ring: true }
    }
}

/// Digin servers you're signed in to (the sign-in tokens stay in each Mac's Keychain).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Digin {
    pub accounts: Vec<DiginAccount>,
    pub refresh_seconds: u64,
}

impl Default for Digin {
    fn default() -> Self {
        Digin { accounts: Vec::new(), refresh_seconds: 60 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DiginAccount {
    /// "https://digin.work"
    pub url: String,
    /// What the board calls it; empty = the host name.
    pub label: String,
    pub email: String,
}

/// Claude's multiple-choice questions on the Quake: spin to an option, press to answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AskSettings {
    pub enabled: bool,
    /// Permission prompts too (Allow / Always allow / Deny on the Quake).
    pub permissions: bool,
    /// Unanswered this long, the question goes back to Claude's own prompt.
    pub timeout_seconds: u64,
}

impl Default for AskSettings {
    fn default() -> Self {
        AskSettings { enabled: true, permissions: true, timeout_seconds: 120 }
    }
}

/// Bonjour sync between your Macs: each quaked advertises itself on the local network
/// and the one with the Quake attached shows the others' Claude sessions too. Both Macs
/// need the same key; the first run makes one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncSettings {
    pub enabled: bool,
    pub key: String,
    /// The name other Macs see; empty = this Mac's computer name.
    pub name: String,
}

impl Default for SyncSettings {
    fn default() -> Self {
        SyncSettings { enabled: true, key: String::new(), name: String::new() }
    }
}

/// When something new shows up (a session waiting, a review request, failing CI, a host
/// or site going down, a container stopping): blink the ring and jump to its page.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Alerts {
    pub blink: bool,
    /// Jump to the page, unless you touched the board in the last 15 seconds.
    pub jump: bool,
    /// Stop blinking after this long without input.
    pub seconds: u64,
    pub ai: bool,
    pub code: bool,
    pub ops: bool,
    pub docker: bool,
    /// New Digin mentions, DMs and assignments.
    pub digin: bool,
}

impl Default for Alerts {
    fn default() -> Self {
        Alerts { blink: true, jump: true, seconds: 300, ai: true, code: true, ops: true, docker: true, digin: true }
    }
}

/// Hardware settings applied whenever the Quake connects.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DeviceSettings {
    /// The Quake's own microphone. Off also turns off its (bright) indicator light;
    /// your call mic is chosen separately under soundboard.input.
    pub mic: bool,
    /// Screen brightness (luminance, 10-255) set on every connect; unset leaves whatever
    /// the device has. The board's Settings page and the settings page write it.
    pub screen_brightness: Option<u8>,
}

/// League of Legends client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct League {
    /// Accept the ready check automatically.
    pub auto_accept: bool,
    /// Wait this long before accepting (time to decline on the screen instead).
    pub accept_delay_ms: u64,
    /// Jump to the League page (and chirp) when the queue pops.
    pub snap: bool,
}

impl Default for League {
    fn default() -> Self {
        League { auto_accept: true, accept_delay_ms: 1500, snap: true }
    }
}

/// Docker-compatible engine (Kiri, Docker Desktop, Rancher Desktop...).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Docker {
    /// Engine socket; "" = find one (Kiri's ~/.kiri/run/docker.sock first).
    pub socket: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Ring {
    /// Drive the knob's light ring (off = leave it as the device has it).
    pub enabled: bool,
    pub brightness: u8,
    /// "#RRGGBB" per page.
    pub ai: String,
    pub music: String,
    pub shortcuts: String,
    pub soundboard: String,
    pub league: String,
    pub docker: String,
    pub ops: String,
    pub code: String,
    pub settings: String,
    pub dash: String,
    pub digin: String,
    pub focus: String,
    pub deploy: String,
    pub paste: String,
    pub windows: String,
    pub calendar: String,
    pub health: String,
    /// Breathe in this color while a Claude session needs you.
    pub alert: bool,
    pub alert_color: String,
}

/// How touch-panel coordinates map onto the picture. The Quake's panel reports y from
/// the bottom, so `flipY` is on by default.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Touch {
    pub flip_x: bool,
    pub flip_y: bool,
}

impl Default for Touch {
    fn default() -> Self {
        Touch { flip_x: false, flip_y: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Shortcut {
    pub title: String,
    /// SF Symbol name, e.g. "camera.viewfinder".
    pub symbol: String,
    #[serde(default)]
    pub color: String,
    pub action: Action,
}

/// What a shortcut tile does.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Action {
    /// A key combo typed by the Quake as a USB keyboard, e.g. "cmd+shift+4".
    Keys { keys: String },
    /// Opens an app, file, folder or URL (like `open`).
    Open { target: String },
    /// Runs a macOS Shortcut by name.
    Shortcut { name: String },
    /// Runs a shell command (/bin/sh -c).
    Shell { command: String },
    /// Mutes / unmutes your mic in the soundboard mix.
    MicMute,
    /// "playpause", "next" or "previous" for Spotify / Music.
    Media { command: String },
    /// Plays a soundboard clip by title.
    Clip { title: String },
    /// Switches the board to a page.
    Page { page: PageId },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Soundboard {
    /// Where clips live. Every audio file in it is a clip unless `clips` lists them.
    pub folder: String,
    /// Your real mic (device name); "" = the system default input.
    pub input: String,
    /// Where the mix goes: point your call app's microphone at this device.
    pub output: String,
    /// When your mic is routed through: "auto" (while an app such as Zoom is listening to
    /// `output`), "on" (always) or "off".
    pub live: String,
    /// Mix your mic into the output (off = only clips go out).
    pub passthrough: bool,
    /// Also play clips on your own speakers / headphones.
    pub monitor: bool,
    /// "" = the system default output.
    pub monitor_device: String,
    /// Clip volume, 0.0 to 1.0.
    pub gain: f32,
    /// Optional order and styling; files not listed are added after these.
    pub clips: Vec<Clip>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    /// File name inside `folder`, or an absolute path.
    pub file: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub symbol: String,
    #[serde(default = "one")]
    pub gain: f32,
}

fn one() -> f32 {
    1.0
}

fn sc(title: &str, symbol: &str, color: &str, action: Action) -> Shortcut {
    Shortcut { title: title.into(), symbol: symbol.into(), color: color.into(), action }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: 1,
            updated_at: 0,
            buzzer: true,
            idle_seconds: 45,
            idle_home: true,
            menu_bar: true,
            session_idle_minutes: 5,
            pages: vec![PageId::Dash, PageId::Ai, PageId::Music, PageId::Digin, PageId::Calendar, PageId::Focus, PageId::Shortcuts, PageId::Soundboard, PageId::League, PageId::Docker, PageId::Ops, PageId::Code, PageId::Settings],
            ring: Ring::default(),
            touch: Touch::default(),
            shortcuts: vec![
                sc("Mute mic", "mic.slash.fill", "#FF6B6B", Action::MicMute),
                sc("Screenshot", "camera.viewfinder", "#8AB4F8", Action::Keys { keys: "cmd+shift+4".into() }),
                sc("Lock screen", "lock.fill", "#C9C9D1", Action::Keys { keys: "ctrl+cmd+q".into() }),
                sc("Mission Control", "rectangle.3.group.fill", "#7FD1B9", Action::Keys { keys: "ctrl+up".into() }),
                sc("Claude", "sparkles", "#D97757", Action::Open { target: "claude://".into() }),
                sc("Terminal", "terminal.fill", "#C9C9D1", Action::Open { target: "/System/Applications/Utilities/Terminal.app".into() }),
                sc("Spotify", "music.note", "#1DB954", Action::Open { target: "/Applications/Spotify.app".into() }),
                sc("Play / pause", "playpause.fill", "#1DB954", Action::Media { command: "playpause".into() }),
            ],
            soundboard: Soundboard::default(),
            league: League::default(),
            docker: Docker::default(),
            ops: Ops::default(),
            code: Code::default(),
            alerts: Alerts::default(),
            ask: AskSettings::default(),
            digin: Digin::default(),
            call: CallSettings::default(),
            focus: FocusSettings::default(),
            deploy: DeploySettings::default(),
            paste: PasteSettings::default(),
            windows: WindowsSettings::default(),
            calendar: CalendarSettings::default(),
            health: HealthSettings::default(),
            night: NightSettings::default(),
            viz: VizSettings::default(),
            sync: SyncSettings::default(),
            device: DeviceSettings::default(),
        }
    }
}

impl Default for Ring {
    fn default() -> Self {
        Ring {
            enabled: true,
            brightness: 200,
            ai: "#D97757".into(),
            music: "#1DB954".into(),
            shortcuts: "#4C8DFF".into(),
            soundboard: "#B36BFF".into(),
            league: "#C89B3C".into(),
            docker: "#2496ED".into(),
            ops: "#7FD1B9".into(),
            code: "#A371F7".into(),
            settings: "#D0D0D8".into(),
            dash: "#64D2FF".into(),
            digin: "#E879F9".into(),
            focus: "#FF9F0A".into(),
            deploy: "#30D158".into(),
            paste: "#5AC8FA".into(),
            windows: "#BF5AF2".into(),
            calendar: "#FF453A".into(),
            health: "#66D4CF".into(),
            alert: true,
            alert_color: "#F2B705".into(),
        }
    }
}

impl Default for Soundboard {
    fn default() -> Self {
        Soundboard {
            folder: "~/Music/Quaked Soundboard".into(),
            input: String::new(),
            output: "BlackHole 2ch".into(),
            live: "auto".into(),
            passthrough: true,
            monitor: true,
            monitor_device: String::new(),
            gain: 0.8,
            clips: Vec::new(),
        }
    }
}

impl Ring {
    pub fn color_for(&self, page: PageId) -> &str {
        match page {
            PageId::Ai => &self.ai,
            PageId::Music => &self.music,
            PageId::Shortcuts => &self.shortcuts,
            PageId::Soundboard => &self.soundboard,
            PageId::League => &self.league,
            PageId::Docker => &self.docker,
            PageId::Ops => &self.ops,
            PageId::Code => &self.code,
            PageId::Settings => &self.settings,
            PageId::Dash => &self.dash,
            PageId::Digin => &self.digin,
            PageId::Focus => &self.focus,
            PageId::Deploy => &self.deploy,
            PageId::Paste => &self.paste,
            PageId::Windows => &self.windows,
            PageId::Calendar => &self.calendar,
            PageId::Health => &self.health,
        }
    }
}

/// "#RRGGBB" -> 0xRRGGBB (falls back to `default`).
pub fn hex(s: &str, default: u32) -> u32 {
    u32::from_str_radix(s.trim().trim_start_matches('#'), 16).ok().filter(|_| s.trim().trim_start_matches('#').len() == 6).unwrap_or(default)
}

/// 0xRRGGBB -> (hue, saturation) on QMK's 0..=255 scales.
pub fn hue_sat(rgb: u32) -> (u8, u8) {
    let (r, g, b) = (((rgb >> 16) & 255) as f32 / 255.0, ((rgb >> 8) & 255) as f32 / 255.0, (rgb & 255) as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let mut h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    if h < 0.0 {
        h += 360.0;
    }
    let s = if max == 0.0 { 0.0 } else { d / max };
    ((h / 360.0 * 255.0).round() as u8, (s * 255.0).round() as u8)
}

/// Loads the config, writing the defaults first if there is none.
/// The saved settings; Err if the file exists but can't be read as settings.
pub fn try_load() -> Result<Config, String> {
    let p = path();
    match std::fs::read_to_string(&p) {
        Ok(text) => serde_json::from_str::<Config>(&text).map_err(|e| format!("{} is invalid ({e})", p.display())),
        Err(_) => Ok(load()),
    }
}

pub fn load() -> Config {
    let p = path();
    match std::fs::read_to_string(&p) {
        Ok(text) => match serde_json::from_str::<Config>(&text) {
            Ok(c) => c,
            Err(e) => {
                crate::device::log(format!("config: {} is invalid ({e}); using defaults", p.display()));
                Config::default()
            }
        },
        Err(_) => {
            let c = Config::default();
            let _ = std::fs::create_dir_all(dir());
            if let Ok(text) = serde_json::to_string_pretty(&c) {
                let _ = std::fs::write(&p, text);
            }
            c
        }
    }
}

/// Writes the config atomically (temp file + rename).
pub fn save(cfg: &Config) -> bool {
    let path = path();
    let tmp = path.with_extension("json.tmp");
    let Ok(text) = serde_json::to_string_pretty(cfg) else { return false };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_ok()
}

pub fn modified() -> Option<SystemTime> {
    std::fs::metadata(path()).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_partial_files() {
        let c = Config::default();
        let text = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Config>(&text).unwrap(), c);
        let partial: Config = serde_json::from_str(r##"{"buzzer": false, "ring": {"ai": "#112233"}}"##).unwrap();
        assert!(!partial.buzzer);
        assert_eq!(partial.ring.ai, "#112233");
        assert_eq!(partial.ring.music, Ring::default().music);
        assert_eq!(partial.pages.len(), 13);
    }

    #[test]
    fn actions_are_tagged() {
        let a: Action = serde_json::from_str(r#"{"type":"keys","keys":"cmd+shift+4"}"#).unwrap();
        assert_eq!(a, Action::Keys { keys: "cmd+shift+4".into() });
        let m: Action = serde_json::from_str(r#"{"type":"micMute"}"#).unwrap();
        assert_eq!(m, Action::MicMute);
    }

    #[test]
    fn colors() {
        assert_eq!(hex("#D97757", 0), 0xD97757);
        assert_eq!(hex("nope", 7), 7);
        assert_eq!(hue_sat(0xFF0000), (0, 255));
        assert_eq!(hue_sat(0x00FF00).0, 85);
    }
}
