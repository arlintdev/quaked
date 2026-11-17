//! Call mode: notices you're on a call and puts the call screen over the board.
//!
//! A call is on while an app listens to the soundboard's virtual mic (`audio::Status::
//! listener`), or while a known call app (Zoom, Teams, FaceTime, Webex, Slack,
//! Discord...) holds a microphone. The thread watches CoreAudio's process list for the
//! second; the app side (app/call.rs) folds in the first, decides whether the screen
//! shows, and owns the rest of `Status` (selection, dismissal, timer).

use crate::config::CallSettings as Cfg;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// How a knob press mutes you.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Mute {
    /// Only the call app can: a press brings it forward.
    #[default]
    App,
    /// The soundboard carries your mic into the call, so we mute it ourselves
    /// (`audio::Status::mic_muted` is the truth).
    Soundboard,
    /// A press brings the call app forward and types its own shortcut ("cmd+shift+a");
    /// whether you're muted is the app's business.
    Keys(String),
}

/// What the board shows. Everything here is set on the app side.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// A call is on and the screen isn't hidden: it shows over everything.
    pub active: bool,
    /// The call app ("Zoom") and its bundle id.
    pub app: String,
    pub bundle: String,
    /// The calendar event going on now, if any ("Design review").
    pub meeting: String,
    /// When the call started (unix ms).
    pub started_ms: u64,
    pub mute: Mute,
    /// The soundboard mixes your mic in (off: callers hear only the clips).
    pub passthrough: bool,
    /// The clip the knob is on.
    pub sel: usize,
    /// Feedback after a press: (text, unix ms).
    pub note: Option<(String, u64)>,
    /// Call apps holding a microphone, from the watcher thread (bundle ids).
    pub capturing: Vec<String>,
    /// A call is going on (even with the screen hidden or call mode off).
    pub on_call: bool,
    /// Hidden with a double-click until this call ends.
    pub dismissed: bool,
}

/// How many clips the call screen shows.
pub const CLIPS: usize = 8;
/// Tap indices on the call screen (clips are TAP_CLIP + i).
pub const TAP_MUTE: usize = 0;
pub const TAP_HIDE: usize = 1;
pub const TAP_EVENT: usize = 2;
pub const TAP_CLIP: usize = 10;

// ---------------------------------------------------------------- call apps

/// (bundle id prefix, name, its own mute shortcut, is a browser). A prefix also matches
/// its helpers ("com.google.Chrome" matches "com.google.Chrome.helper").
const APPS: &[(&str, &str, Option<&str>, bool)] = &[
    ("us.zoom.xos", "Zoom", Some("cmd+shift+a"), false),
    ("com.microsoft.teams2", "Teams", Some("cmd+shift+m"), false),
    ("com.microsoft.teams", "Teams", Some("cmd+shift+m"), false),
    ("com.apple.FaceTime", "FaceTime", None, false),
    ("com.apple.avconferenced", "FaceTime", None, false),
    ("com.cisco.webexmeetingsapp", "Webex", Some("cmd+shift+m"), false),
    ("com.webex.meetingmanager", "Webex", Some("cmd+shift+m"), false),
    ("Cisco-Systems.Spark", "Webex", Some("cmd+shift+m"), false),
    ("com.tinyspeck.slackmacgap", "Slack", Some("cmd+shift+space"), false),
    ("com.hnc.Discord", "Discord", Some("cmd+shift+m"), false),
    ("co.teamport.around", "Around", None, false),
    ("com.skype.skype", "Skype", None, false),
    ("com.amazon.Amazon-Chime", "Chime", None, false),
    ("com.logmein.GoToMeeting", "GoTo Meeting", None, false),
    ("com.ringcentral.glip", "RingCentral", None, false),
    ("net.whatsapp.WhatsApp", "WhatsApp", None, false),
    ("ru.keepcoder.Telegram", "Telegram", None, false),
    ("org.whispersystems.signal-desktop", "Signal", None, false),
    ("app.tuple.app", "Tuple", None, false),
    ("com.google.Chrome", "Chrome", None, true),
    ("com.apple.Safari", "Safari", None, true),
    ("com.apple.WebKit.GPU", "Safari", None, true),
    ("company.thebrowser.Browser", "Arc", None, true),
    ("org.mozilla.firefox", "Firefox", None, true),
    ("com.microsoft.edgemac", "Edge", None, true),
    ("com.brave.Browser", "Brave", None, true),
    ("com.vivaldi.Vivaldi", "Vivaldi", None, true),
];

fn matches(bundle: &str, prefix: &str) -> bool {
    let (b, p) = (bundle.to_ascii_lowercase(), prefix.to_ascii_lowercase());
    b == p || b.strip_prefix(&p).is_some_and(|rest| rest.starts_with('.'))
}

fn known(bundle: &str) -> Option<&'static (&'static str, &'static str, Option<&'static str>, bool)> {
    // The longest prefix wins ("com.microsoft.teams2" over "com.microsoft.teams").
    APPS.iter().filter(|a| matches(bundle, a.0)).max_by_key(|a| a.0.len())
}

/// Is `bundle` a call app under these settings?
pub fn is_call_app(bundle: &str, cfg: &Cfg) -> bool {
    match known(bundle) {
        Some(a) => !a.3 || cfg.browsers,
        None => cfg.apps.iter().any(|x| !x.trim().is_empty() && matches(bundle, x.trim())),
    }
}

/// A call app's name ("us.zoom.xos" -> "Zoom"); None if we don't know it.
pub fn app_name(bundle: &str) -> Option<&'static str> {
    known(bundle).map(|a| a.1)
}

/// The app's own mute shortcut, if it has a dependable one.
pub fn mute_keys(bundle: &str) -> Option<&'static str> {
    known(bundle).and_then(|a| a.2)
}

/// Who the call is in and how a press mutes, from the soundboard's listener and the call
/// apps holding a mic. None = not on a call.
pub fn decide(listener: Option<&str>, capturing: &[String], cfg: &Cfg) -> Option<(String, Mute)> {
    if let Some(l) = listener.filter(|l| !l.is_empty()) {
        return Some((l.to_string(), Mute::Soundboard));
    }
    let b = capturing.first()?;
    let mute = match mute_keys(b) {
        Some(k) if cfg.mute_shortcut => Mute::Keys(k.to_string()),
        _ => Mute::App,
    };
    Some((b.clone(), mute))
}

/// "Cmd+Shift+A" for "cmd+shift+a" (the board's fonts have no ⌘ / ⇧ glyphs).
pub fn keys_label(combo: &str) -> String {
    combo
        .split('+')
        .map(|p| match p.trim().to_ascii_lowercase().as_str() {
            "cmd" | "command" => "Cmd".to_string(),
            "shift" => "Shift".to_string(),
            "alt" | "opt" | "option" => "Opt".to_string(),
            "ctrl" | "control" => "Ctrl".to_string(),
            "space" => "Space".to_string(),
            k => k.to_uppercase(),
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// "4:07", "1:02:33"
pub fn timer(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

// ---------------------------------------------------------------- the watcher thread

#[derive(Debug)]
pub enum Cmd {
    Configure(Cfg),
}

/// What the module's thread sends the app (handled in app/call.rs on the main thread).
#[derive(Debug)]
pub enum Update {
    /// Call apps on a call right now (bundle ids, longest-running first).
    Capturing(Vec<String>),
}

const POLL: Duration = Duration::from_secs(2);
/// Holding a mic this long makes it a call (two polls in a row: a blip isn't a call)...
const START_AFTER: Duration = Duration::from_secs(2);
/// ...and letting go of it this long ends it (some apps drop the mic for a moment).
const END_AFTER: Duration = Duration::from_secs(8);

/// Smooths the raw "who holds a mic" polls into calls.
#[derive(Default)]
struct Watch {
    /// bundle -> (first seen, last seen)
    seen: HashMap<String, (Instant, Instant)>,
}

impl Watch {
    fn step(&mut self, now: Instant, raw: &[String]) -> Vec<String> {
        for b in raw {
            self.seen.entry(b.clone()).or_insert((now, now)).1 = now;
        }
        self.seen.retain(|_, (_, last)| now.duration_since(*last) < END_AFTER);
        let mut on: Vec<(&String, Instant)> = self.seen.iter().filter(|(_, (first, _))| now.duration_since(*first) >= START_AFTER).map(|(b, (first, _))| (b, *first)).collect();
        on.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(b.0)));
        on.into_iter().map(|(b, _)| b.clone()).collect()
    }
}

/// Starts the module's background work; `emit` hands updates to the app.
pub fn spawn(cfg: Cfg, emit: impl Fn(Update) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("call".into()).spawn(move || run(cfg, rx, emit)).unwrap();
    tx
}

fn run(mut cfg: Cfg, rx: Receiver<Cmd>, emit: impl Fn(Update)) {
    let mut watch = Watch::default();
    let mut last: Option<Vec<String>> = None;
    loop {
        let on = if cfg.enabled {
            let raw: Vec<String> = crate::audio::input_apps().into_iter().filter(|b| is_call_app(b, &cfg)).collect();
            watch.step(Instant::now(), &raw)
        } else {
            watch = Watch::default();
            Vec::new()
        };
        if last.as_ref() != Some(&on) {
            if !on.is_empty() || last.as_ref().is_some_and(|l| !l.is_empty()) {
                crate::device::log(format!("call: {}", if on.is_empty() { "ended".to_string() } else { on.join(", ") }));
            }
            emit(Update::Capturing(on.clone()));
            last = Some(on);
        }
        match rx.recv_timeout(if cfg.enabled { POLL } else { Duration::from_secs(3600) }) {
            Ok(Cmd::Configure(c)) => cfg = c,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Sample data for `quaked preview`: 23 minutes into a Zoom call through the soundboard.
pub fn sample() -> Status {
    Status {
        active: true,
        app: "Zoom".into(),
        bundle: "us.zoom.xos".into(),
        meeting: "1:1 with Priya".into(),
        started_ms: crate::status::now_ms() - (23 * 60 + 41) * 1000,
        mute: Mute::Soundboard,
        passthrough: true,
        sel: 2,
        note: None,
        capturing: vec!["us.zoom.xos".into()],
        on_call: true,
        dismissed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Cfg {
        Cfg::default()
    }

    #[test]
    fn recognizes_call_apps() {
        let c = cfg();
        assert!(is_call_app("us.zoom.xos", &c));
        assert!(is_call_app("com.microsoft.teams2", &c));
        assert!(is_call_app("com.google.Chrome.helper", &c));
        assert!(is_call_app("com.hnc.Discord.helper", &c));
        assert!(!is_call_app("com.apple.VoiceMemos", &c));
        assert!(!is_call_app("com.google.Chromebook", &c)); // a prefix needs a '.' after it
        let no_browsers = Cfg { browsers: false, ..cfg() };
        assert!(!is_call_app("com.google.Chrome", &no_browsers));
        assert!(is_call_app("us.zoom.xos", &no_browsers));
        let extra = Cfg { apps: vec!["com.example.Meet".into()], ..cfg() };
        assert!(is_call_app("com.example.Meet", &extra));
        assert!(!is_call_app("com.example.Meet", &c));
        assert_eq!(app_name("com.microsoft.teams2"), Some("Teams"));
        assert_eq!(app_name("com.google.Chrome.helper"), Some("Chrome"));
        assert_eq!(mute_keys("us.zoom.xos"), Some("cmd+shift+a"));
        assert_eq!(mute_keys("com.apple.FaceTime"), None);
    }

    #[test]
    fn decides_who_the_call_is_in() {
        let c = cfg();
        // The soundboard's listener: we control the mic.
        assert_eq!(decide(Some("us.zoom.xos"), &[], &c), Some(("us.zoom.xos".into(), Mute::Soundboard)));
        assert_eq!(decide(Some("us.zoom.xos"), &["com.apple.FaceTime".into()], &c), Some(("us.zoom.xos".into(), Mute::Soundboard)));
        // Only a call app on its own mic: mute is in the app, unless shortcuts are on and it has one.
        assert_eq!(decide(None, &["us.zoom.xos".into()], &c), Some(("us.zoom.xos".into(), Mute::App)));
        let keys = Cfg { mute_shortcut: true, ..cfg() };
        assert_eq!(decide(None, &["us.zoom.xos".into()], &keys), Some(("us.zoom.xos".into(), Mute::Keys("cmd+shift+a".into()))));
        assert_eq!(decide(None, &["com.apple.FaceTime".into()], &keys), Some(("com.apple.FaceTime".into(), Mute::App)));
        assert_eq!(decide(None, &[], &c), None);
        assert_eq!(decide(Some(""), &[], &c), None);
    }

    #[test]
    fn a_blip_is_not_a_call() {
        let t0 = Instant::now();
        let z = vec!["us.zoom.xos".to_string()];
        let mut w = Watch::default();
        assert!(w.step(t0, &z).is_empty());
        assert_eq!(w.step(t0 + POLL, &z), z);
        // A short drop doesn't end it...
        assert_eq!(w.step(t0 + POLL * 3, &[]), z);
        assert_eq!(w.step(t0 + POLL * 4, &z), z);
        // ...letting go for a while does.
        assert!(w.step(t0 + POLL * 4 + END_AFTER, &[]).is_empty());
        // A single sighting never starts one.
        let mut w = Watch::default();
        assert!(w.step(t0, &z).is_empty());
        assert!(w.step(t0 + END_AFTER, &[]).is_empty());
    }

    #[test]
    fn labels() {
        assert_eq!(keys_label("cmd+shift+a"), "Cmd+Shift+A");
        assert_eq!(keys_label("cmd+shift+space"), "Cmd+Shift+Space");
        assert_eq!(timer(247), "4:07");
        assert_eq!(timer(3753), "1:02:33");
    }
}
