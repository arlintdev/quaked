//! Focus timer: a countdown you set and start on the knob, shown on its page and in the
//! ring wherever the board is. The timer itself lives on the main thread (app/focus.rs);
//! this module has its state and rules, and a small thread that keeps the per-day tally
//! (and the block in progress, so a restart doesn't lose it) in
//! ~/Library/Application Support/Quaked/focus.json.

use crate::config::FocusSettings as Cfg;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

/// Lengths you can set, in minutes.
pub const MIN_MIN: u64 = 1;
pub const MAX_MIN: u64 = 120;
/// What the screen's − / + buttons move it by.
pub const STEP_MIN: u64 = 5;
/// Knob clicks closer together than this count as one spin.
pub const SPIN_GAP: Duration = Duration::from_millis(160);
/// A spin moves a minute a click, then 5 after this many quick clicks, then 15.
const SPIN_FIVES_AFTER: u32 = 4;
const SPIN_FIFTEENS_AFTER: u32 = 12;
/// The ring breathes green this long after a block ends (after the alert blink).
pub const DONE_RING_MS: u64 = 30_000;
/// A finished block goes back to "ready" on its own after this long.
pub const DONE_LINGER_MS: u64 = 10 * 60_000;
/// How many color / brightness steps the ring takes over a block.
pub const RING_STEPS: u32 = 24;
/// Days of history kept in focus.json.
const KEEP_DAYS: usize = 120;

pub const RING_GREEN: u32 = 0x30D158;
pub const RING_AMBER: u32 = 0xF2B705;
pub const RING_RED: u32 = 0xFF453A;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    #[default]
    Ready,
    Running,
    Paused,
    Done,
}

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::Ready => "ready",
            Phase::Running => "running",
            Phase::Paused => "paused",
            Phase::Done => "done",
        }
    }
}

/// One day's focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Day {
    /// Blocks run to the end.
    pub blocks: u32,
    /// Minutes focused (finished blocks, plus the part of cancelled ones you did).
    pub minutes: u64,
}

/// The timer and the tally: what the board shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub phase: Phase,
    /// The block's length (being set, or running).
    pub minutes: u64,
    /// Running: when it ends (unix ms, on a whole second).
    pub ends_ms: u64,
    /// Paused: time left (ms).
    pub left_ms: u64,
    /// When the block started (unix ms).
    pub started_ms: u64,
    /// Done: when it ended (unix ms).
    pub done_ms: u64,
    /// Done: the ring breathes green until then (unix ms).
    pub ring_until_ms: u64,
    /// The settings' length last taken (so a change there shows up while ready).
    pub cfg_minutes: u64,
    /// Focus per local day ("2026-11-16").
    pub days: BTreeMap<String, Day>,
    /// focus.json has been read.
    pub loaded: bool,
}

/// What the ring should show: (effect, color, brightness, speed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingLook {
    pub effect: u8,
    pub rgb: u32,
    pub brightness: u8,
    pub speed: u8,
}

pub fn clamp_minutes(m: u64) -> u64 {
    m.clamp(MIN_MIN, MAX_MIN)
}

/// The knob's spin on the timer: slow clicks fine-tune a minute at a time, a fast spin
/// speeds up the longer it goes.
#[derive(Debug, Clone, Copy, Default)]
pub struct Spin {
    last: Option<(Instant, i32)>,
    run: u32,
}

impl Spin {
    /// A click in direction `d` (±1) at `at`: how many minutes it moves.
    pub fn click(&mut self, at: Instant, d: i32) -> u64 {
        let quick = self.last.is_some_and(|(t, dir)| dir == d.signum() && at.saturating_duration_since(t) < SPIN_GAP);
        self.run = if quick { self.run + 1 } else { 1 };
        self.last = Some((at, d.signum()));
        match self.run {
            r if r > SPIN_FIFTEENS_AFTER => 15,
            r if r > SPIN_FIVES_AFTER => 5,
            _ => 1,
        }
    }
}

/// Rounds a unix ms time up to the next whole second.
fn whole_second(ms: u64) -> u64 {
    ms.div_ceil(1000) * 1000
}

impl Status {
    pub fn total_ms(&self) -> u64 {
        self.minutes * 60_000
    }

    /// Time left on the clock.
    pub fn left_ms(&self, now: u64) -> u64 {
        match self.phase {
            Phase::Ready => self.total_ms(),
            Phase::Running => self.ends_ms.saturating_sub(now).min(self.total_ms()),
            Phase::Paused => self.left_ms,
            Phase::Done => 0,
        }
    }

    /// Whole seconds left, as the countdown shows them (25:00 at the start, 0:00 at the end).
    pub fn left_secs(&self, now: u64) -> u64 {
        self.left_ms(now).div_ceil(1000)
    }

    /// How far through the block (0 = just started, 1 = done).
    pub fn elapsed_frac(&self, now: u64) -> f32 {
        let total = self.total_ms();
        if total == 0 {
            return 0.0;
        }
        match self.phase {
            Phase::Ready => 0.0,
            Phase::Done => 1.0,
            _ => (1.0 - self.left_ms(now) as f32 / total as f32).clamp(0.0, 1.0),
        }
    }

    /// Time focused in this block so far.
    pub fn focused_ms(&self, now: u64) -> u64 {
        match self.phase {
            Phase::Running | Phase::Paused => self.total_ms().saturating_sub(self.left_ms(now)),
            Phase::Done => self.total_ms(),
            Phase::Ready => 0,
        }
    }

    /// The day `key`'s tally.
    pub fn day(&self, key: &str) -> Day {
        self.days.get(key).copied().unwrap_or_default()
    }

    /// Today's tally.
    pub fn today(&self, now: u64) -> Day {
        self.day(&day_key(now))
    }

    pub fn active(&self) -> bool {
        matches!(self.phase, Phase::Running | Phase::Paused)
    }

    /// Moves the length one `step` (minutes) in direction `d` (only while not running): a
    /// step over a minute lands on its next round number (27 up by 5 is 30). Done goes back
    /// to ready. Returns whether anything changed.
    pub fn adjust(&mut self, d: i32, step: u64) -> bool {
        if self.active() || d == 0 {
            return false;
        }
        let was = (self.phase, self.minutes);
        self.phase = Phase::Ready;
        let m = clamp_minutes(self.minutes);
        let step = step.max(1);
        let m = if d > 0 { (m / step + 1) * step } else { (m.saturating_sub(1) / step) * step };
        self.minutes = clamp_minutes(m);
        was != (self.phase, self.minutes)
    }

    /// Starts a block of `minutes` (from ready or done).
    pub fn start(&mut self, now: u64) -> bool {
        if self.active() {
            return false;
        }
        self.minutes = clamp_minutes(self.minutes);
        self.phase = Phase::Running;
        self.started_ms = now;
        self.ends_ms = whole_second(now + self.total_ms());
        self.left_ms = 0;
        self.done_ms = 0;
        self.ring_until_ms = 0;
        true
    }

    pub fn pause(&mut self, now: u64) -> bool {
        if self.phase != Phase::Running {
            return false;
        }
        self.left_ms = self.left_ms(now);
        self.phase = Phase::Paused;
        true
    }

    pub fn resume(&mut self, now: u64) -> bool {
        if self.phase != Phase::Paused {
            return false;
        }
        self.ends_ms = whole_second(now + self.left_ms);
        self.left_ms = 0;
        self.phase = Phase::Running;
        true
    }

    /// Stops a running or paused block without counting it; returns how long you
    /// focused in it (ms), or None if nothing was running.
    pub fn cancel(&mut self, now: u64) -> Option<u64> {
        if !self.active() {
            return None;
        }
        let focused = self.focused_ms(now);
        self.phase = Phase::Ready;
        self.ends_ms = 0;
        self.left_ms = 0;
        Some(focused)
    }

    /// A finished block back to ready.
    pub fn dismiss(&mut self) -> bool {
        if self.phase != Phase::Done {
            return false;
        }
        self.phase = Phase::Ready;
        self.done_ms = 0;
        self.ring_until_ms = 0;
        true
    }

    /// The running block has reached zero.
    pub fn due(&self, now: u64) -> bool {
        self.phase == Phase::Running && now >= self.ends_ms
    }

    /// Ends the running block (at `at`, its end time) and counts it on `day`.
    pub fn finish(&mut self, at: u64, day: &str) -> bool {
        if self.phase != Phase::Running {
            return false;
        }
        self.phase = Phase::Done;
        self.done_ms = at;
        self.ring_until_ms = at + DONE_RING_MS;
        self.ends_ms = 0;
        let minutes = self.minutes;
        self.credit(day, 1, minutes);
        true
    }

    /// Adds to a day's tally and drops days older than `KEEP_DAYS`.
    pub fn credit(&mut self, day: &str, blocks: u32, minutes: u64) {
        if blocks == 0 && minutes == 0 {
            return;
        }
        let d = self.days.entry(day.to_string()).or_default();
        d.blocks += blocks;
        d.minutes += minutes;
        prune(&mut self.days);
    }

    /// The ring while a block runs (green to amber to red as time runs out, dimming a
    /// little), is paused (slow amber breathing) or has just ended (green breathing).
    /// It moves in `RING_STEPS` steps so the knob isn't sent a new color every second.
    pub fn ring(&self, now: u64, brightness: u8) -> Option<RingLook> {
        match self.phase {
            Phase::Running => {
                let t = ring_step(self.elapsed_frac(now));
                let dim = 1.0 - 0.55 * t * t;
                let b = ((brightness as f32 * dim).round() as u8).max(brightness.min(24));
                Some(RingLook { effect: crate::proto::EFFECT_SOLID, rgb: ramp(t), brightness: b, speed: 128 })
            }
            Phase::Paused => Some(RingLook { effect: crate::proto::EFFECT_BREATHING, rgb: RING_AMBER, brightness, speed: 60 }),
            Phase::Done if now < self.ring_until_ms => Some(RingLook { effect: crate::proto::EFFECT_BREATHING, rgb: RING_GREEN, brightness: brightness.max(120), speed: 150 }),
            _ => None,
        }
    }

    /// What focus.json keeps.
    pub fn store(&self) -> Store {
        let active = self.active().then_some(Active { phase: self.phase, minutes: self.minutes, ends_ms: self.ends_ms, left_ms: self.left_ms, started_ms: self.started_ms });
        Store { days: self.days.clone(), active }
    }
}

/// `t` (0..1) snapped down to the ring's steps.
pub fn ring_step(t: f32) -> f32 {
    let n = RING_STEPS as f32;
    ((t.clamp(0.0, 1.0) * n).floor() / n).min(1.0)
}

/// The color for how far through a block you are: green, then amber about two thirds
/// in, red at the end (hue sweeps through yellow and orange, never muddy).
pub fn ramp(t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let (g, a, r) = (hsv(RING_GREEN), hsv(RING_AMBER), hsv(RING_RED));
    let (from, to, k) = if t < 0.65 { (g, a, t / 0.65) } else { (a, r, (t - 0.65) / 0.35) };
    let lerp = |x: f32, y: f32| x + (y - x) * k;
    rgb_of(lerp(from.0, to.0), lerp(from.1, to.1), lerp(from.2, to.2))
}

/// (hue degrees, saturation, value) of 0xRRGGBB.
fn hsv(c: u32) -> (f32, f32, f32) {
    let (r, g, b) = (((c >> 16) & 255) as f32 / 255.0, ((c >> 8) & 255) as f32 / 255.0, (c & 255) as f32 / 255.0);
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

fn rgb_of(h: f32, s: f32, v: f32) -> u32 {
    let c = v * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let to = |f: f32| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u32;
    to(r) << 16 | to(g) << 8 | to(b)
}

fn prune(days: &mut BTreeMap<String, Day>) {
    while days.len() > KEEP_DAYS {
        let Some(oldest) = days.keys().next().cloned() else { break };
        days.remove(&oldest);
    }
}

// ---------------------------------------------------------------- local time

fn local(ms: u64) -> libc::tm {
    let t = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    tm
}

/// The local day of a unix ms time: "2026-11-16".
pub fn day_key(ms: u64) -> String {
    let tm = local(ms);
    format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
}

/// Local clock time like the board's: "4:47".
pub fn clock(ms: u64) -> String {
    let tm = local(ms);
    let h = match tm.tm_hour % 12 {
        0 => 12,
        h => h,
    };
    format!("{h}:{:02}", tm.tm_min)
}

/// The last `n` local days, oldest first: (key, weekday letter).
pub fn last_days(now: u64, n: usize) -> Vec<(String, &'static str)> {
    // From local noon, so a 23- or 25-hour day never lands twice or gets skipped.
    let tm = local(now);
    let since_midnight = (tm.tm_hour as u64 * 3600 + tm.tm_min as u64 * 60 + tm.tm_sec as u64) * 1000;
    let noon = now.saturating_sub(since_midnight) + 12 * 3_600_000;
    (0..n as u64)
        .rev()
        .map(|i| {
            let t = noon.saturating_sub(i * 86_400_000);
            let letter = ["S", "M", "T", "W", "T", "F", "S"][local(t).tm_wday.clamp(0, 6) as usize];
            (day_key(t), letter)
        })
        .collect()
}

// ---------------------------------------------------------------- focus.json

/// A block in progress, kept across restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Active {
    pub phase: Phase,
    pub minutes: u64,
    #[serde(default)]
    pub ends_ms: u64,
    #[serde(default)]
    pub left_ms: u64,
    #[serde(default)]
    pub started_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Store {
    pub days: BTreeMap<String, Day>,
    pub active: Option<Active>,
}

pub fn path() -> PathBuf {
    crate::config::dir().join("focus.json")
}

pub fn load(p: &Path) -> Store {
    std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(p: &Path, store: &Store) -> std::io::Result<()> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(store).unwrap_or_default())?;
    std::fs::rename(&tmp, p)
}

// ---------------------------------------------------------------- thread

#[derive(Debug)]
pub enum Cmd {
    Configure(Cfg),
    /// Write focus.json.
    Save(Store),
}

/// What the module's thread sends the app (handled in app/focus.rs on the main thread).
#[derive(Debug)]
pub enum Update {
    /// focus.json, read at startup.
    Loaded(Store),
}

/// Reads focus.json and then writes it whenever the app asks (off the main thread).
pub fn spawn(cfg: Cfg, emit: impl Fn(Update) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("focus".into()).spawn(move || run(cfg, rx, emit)).unwrap();
    tx
}

fn run(mut cfg: Cfg, rx: Receiver<Cmd>, emit: impl Fn(Update)) {
    let p = path();
    emit(Update::Loaded(load(&p)));
    for cmd in rx {
        match cmd {
            // The main thread reads the settings itself; this just notes new Shortcuts.
            Cmd::Configure(c) => {
                if (&c.start_shortcut, &c.end_shortcut) != (&cfg.start_shortcut, &cfg.end_shortcut) {
                    crate::device::log(format!("focus: shortcuts now start {:?}, end {:?}", c.start_shortcut, c.end_shortcut));
                }
                cfg = c;
            }
            Cmd::Save(store) => {
                if let Err(e) = save(&p, &store) {
                    crate::device::log(format!("focus: could not write {}: {e}", p.display()));
                }
            }
        }
    }
}

/// Sample data for `quaked preview`: a block with about 13 minutes left, 3 done today.
pub fn sample() -> Status {
    let now = crate::status::now_ms();
    let mut st = Status { minutes: 25, cfg_minutes: 25, loaded: true, ..Default::default() };
    st.start(now - (12 * 60 + 23) * 1000);
    for (i, (key, _)) in last_days(now, 7).into_iter().enumerate() {
        let (blocks, minutes) = [(4, 110), (2, 55), (0, 0), (5, 135), (3, 80), (1, 25), (3, 75)][i];
        st.days.insert(key, Day { blocks, minutes });
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_763_300_000_000; // a whole second

    fn ready(minutes: u64) -> Status {
        Status { minutes, ..Default::default() }
    }

    #[test]
    fn length_steps_and_clamps() {
        let mut s = ready(25);
        assert!(s.adjust(1, 1));
        assert_eq!(s.minutes, 26);
        // Bigger steps land on round numbers, both ways.
        assert!(s.adjust(1, 5));
        assert_eq!(s.minutes, 30);
        assert!(s.adjust(1, 5));
        assert_eq!(s.minutes, 35);
        s.adjust(-1, 1);
        assert!(s.adjust(-1, 5));
        assert_eq!(s.minutes, 30);
        assert!(s.adjust(1, 15));
        assert_eq!(s.minutes, 45);
        s.adjust(-1, 1);
        s.adjust(-1, 15);
        assert_eq!(s.minutes, 30);
        for _ in 0..40 {
            s.adjust(-1, 1);
        }
        assert_eq!(s.minutes, MIN_MIN);
        assert!(!s.adjust(-1, 1), "already at the shortest");
        assert!(!s.adjust(-1, 5));
        for _ in 0..20 {
            s.adjust(1, 15);
        }
        assert_eq!(s.minutes, MAX_MIN);
        assert_eq!(clamp_minutes(0), 1);
        assert_eq!(clamp_minutes(27), 27);
        assert_eq!(clamp_minutes(500), 120);
        // Not while a block runs or is paused.
        s.start(T0);
        assert!(!s.adjust(1, 1));
        s.pause(T0 + 1000);
        assert!(!s.adjust(1, 1));
        assert_eq!(s.minutes, MAX_MIN);
    }

    #[test]
    fn a_spin_speeds_up_slow_clicks_dont() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        // Slow, deliberate clicks: a minute each, however many.
        let mut sp = Spin::default();
        let slow: Vec<u64> = (0..10).map(|i| sp.click(ms(i * 300), 1)).collect();
        assert!(slow.iter().all(|&m| m == 1), "{slow:?}");
        // A fast spin: a minute for the first few clicks, then 5, then 15.
        let mut sp = Spin::default();
        let fast: Vec<u64> = (0..16).map(|i| sp.click(ms(i * 60), 1)).collect();
        assert_eq!(fast, [1, 1, 1, 1, 5, 5, 5, 5, 5, 5, 5, 5, 15, 15, 15, 15]);
        // Pausing or turning back starts over at a minute.
        assert_eq!(sp.click(ms(15 * 60 + 400), 1), 1);
        let mut sp = Spin::default();
        for i in 0..8 {
            sp.click(ms(i * 60), 1);
        }
        assert_eq!(sp.click(ms(8 * 60), -1), 1);
    }

    #[test]
    fn start_pause_resume_finish() {
        let mut s = ready(25);
        assert_eq!(s.left_secs(T0), 25 * 60);
        assert!(s.start(T0 + 300)); // ends on a whole second
        assert_eq!(s.phase, Phase::Running);
        assert_eq!(s.ends_ms, T0 + 25 * 60_000 + 1000);
        assert_eq!(s.left_secs(T0 + 1000), 25 * 60);
        assert!(!s.start(T0 + 2000), "already running");
        // 10 minutes in, pause for 5, resume.
        let t = T0 + 1000 + 10 * 60_000;
        assert!(s.pause(t));
        assert_eq!(s.phase, Phase::Paused);
        assert_eq!(s.left_ms(t + 5 * 60_000), 15 * 60_000, "the clock stops while paused");
        assert_eq!(s.focused_ms(t + 5 * 60_000), 10 * 60_000);
        assert!(s.resume(t + 5 * 60_000));
        assert_eq!(s.ends_ms, t + 20 * 60_000);
        assert!((s.elapsed_frac(t + 5 * 60_000) - 0.4).abs() < 1e-4);
        assert!(!s.due(s.ends_ms - 1));
        assert!(s.due(s.ends_ms));
        let end = s.ends_ms;
        assert!(s.finish(end, "2026-11-16"));
        assert_eq!(s.phase, Phase::Done);
        assert_eq!(s.left_secs(end + 10), 0);
        assert_eq!(s.day("2026-11-16"), Day { blocks: 1, minutes: 25 });
        assert!(!s.finish(end, "2026-11-16"), "counted once");
        // Press again: dismiss, then a new block of the same length.
        assert!(s.dismiss());
        assert_eq!((s.phase, s.minutes), (Phase::Ready, 25));
    }

    #[test]
    fn cancel_reports_focused_time() {
        let mut s = ready(30);
        assert_eq!(s.cancel(T0), None, "nothing to cancel");
        s.start(T0);
        assert_eq!(s.cancel(T0 + 7 * 60_000 + 500), Some(7 * 60_000 + 500));
        assert_eq!(s.phase, Phase::Ready);
        assert_eq!(s.minutes, 30, "keeps the length");
        s.start(T0);
        s.pause(T0 + 60_000);
        assert_eq!(s.cancel(T0 + 9 * 60_000), Some(60_000));
        assert!(s.days.is_empty(), "the app decides what to credit");
    }

    #[test]
    fn done_goes_back_to_ready_on_turn() {
        let mut s = ready(25);
        s.start(T0);
        let end = s.ends_ms;
        s.finish(end, "d");
        assert!(s.adjust(1, 1));
        assert_eq!((s.phase, s.minutes), (Phase::Ready, 26));
    }

    #[test]
    fn ring_ramps_green_to_red_in_steps() {
        assert_eq!(ramp(0.0), RING_GREEN);
        assert_eq!(ramp(0.65), RING_AMBER);
        assert_eq!(ramp(1.0), RING_RED);
        // Hue only ever moves toward red.
        let hues: Vec<f32> = (0..=20).map(|i| hsv(ramp(i as f32 / 20.0)).0).collect();
        assert!(hues.windows(2).all(|w| w[1] <= w[0] + 0.5), "{hues:?}");

        let mut s = ready(24);
        s.start(T0);
        let look = |s: &Status, min: f64| s.ring(T0 + (min * 60_000.0) as u64, 200).unwrap();
        // One step per minute for a 24-minute block: the same command within a step.
        assert_eq!(look(&s, 0.1), look(&s, 0.9));
        assert_ne!(look(&s, 0.9), look(&s, 1.1));
        assert_eq!(look(&s, 0.0).rgb, RING_GREEN);
        assert_eq!(look(&s, 0.0).brightness, 200);
        // Distinct commands over the whole block stay around RING_STEPS.
        let mut seen: Vec<RingLook> = (0..24 * 60).map(|sec| s.ring(T0 + sec * 1000, 200).unwrap()).collect();
        seen.dedup();
        assert!(seen.len() <= RING_STEPS as usize + 1, "{}", seen.len());
        // Dims as it goes, never off.
        assert!(seen.windows(2).all(|w| w[1].brightness <= w[0].brightness));
        assert!(look(&s, 23.9).brightness >= 90);
        let late = look(&s, 23.5).rgb;
        assert!(hsv(late).0 < 15.0, "red near the end: {late:06X}");

        s.pause(T0 + 60_000);
        let p = s.ring(T0 + 90_000, 200).unwrap();
        assert_eq!((p.effect, p.rgb), (crate::proto::EFFECT_BREATHING, RING_AMBER));
        s.resume(T0 + 120_000);
        let end = s.ends_ms;
        s.finish(end, "d");
        let d = s.ring(end + 1000, 200).unwrap();
        assert_eq!((d.effect, d.rgb), (crate::proto::EFFECT_BREATHING, RING_GREEN));
        assert_eq!(s.ring(end + DONE_RING_MS, 200), None, "back to the page color");
        assert_eq!(ready(25).ring(T0, 200), None);
    }

    #[test]
    fn day_rollover() {
        let mut s = ready(25);
        s.start(T0);
        let end = s.ends_ms;
        s.finish(end, "2026-11-16");
        s.credit("2026-11-16", 0, 12); // a cancelled block's 12 minutes
        assert_eq!(s.day("2026-11-16"), Day { blocks: 1, minutes: 37 });
        // Past midnight: a fresh day, yesterday kept.
        assert_eq!(s.day("2026-11-17"), Day::default());
        s.credit("2026-11-17", 1, 50);
        assert_eq!(s.day("2026-11-17"), Day { blocks: 1, minutes: 50 });
        assert_eq!(s.day("2026-11-16"), Day { blocks: 1, minutes: 37 });
        s.credit("2026-11-18", 0, 0);
        assert!(!s.days.contains_key("2026-11-18"), "nothing to record");
        // Old days fall off.
        for i in 0..(KEEP_DAYS + 10) {
            s.credit(&format!("2025-{:02}-{:02}", 1 + i / 28, 1 + i % 28), 1, 25);
        }
        assert_eq!(s.days.len(), KEEP_DAYS);
        assert!(s.days.contains_key("2026-11-17"), "keeps the newest");
        assert!(!s.days.contains_key("2025-01-01"), "drops the oldest");
    }

    #[test]
    fn local_days_are_distinct_and_in_order() {
        let days = last_days(T0, 7);
        assert_eq!(days.len(), 7);
        assert_eq!(days[6].0, day_key(T0));
        let mut keys: Vec<&String> = days.iter().map(|d| &d.0).collect();
        assert!(keys.windows(2).all(|w| w[0] < w[1]), "{keys:?}");
        keys.dedup();
        assert_eq!(keys.len(), 7);
        // A whole year back, across both daylight-saving changes.
        let year = last_days(T0, 366);
        let mut keys: Vec<&String> = year.iter().map(|d| &d.0).collect();
        keys.dedup();
        assert_eq!(keys.len(), 366);
        assert_eq!(day_key(T0).len(), 10);
        assert!(clock(T0).contains(':'));
    }

    #[test]
    fn store_round_trip() {
        let mut s = ready(45);
        s.credit("2026-11-15", 2, 60);
        s.start(T0);
        let p = std::env::temp_dir().join(format!("quaked-focus-test-{}.json", std::process::id()));
        save(&p, &s.store()).unwrap();
        let back = load(&p);
        let _ = std::fs::remove_file(&p);
        assert_eq!(back, s.store());
        let a = back.active.unwrap();
        assert_eq!((a.phase, a.minutes, a.ends_ms), (Phase::Running, 45, s.ends_ms));
        assert_eq!(load(Path::new("/nonexistent/focus.json")), Store::default());
        // A ready timer keeps no block.
        assert_eq!(ready(25).store().active, None);
        // Hand-edited or older files still load.
        let partial: Store = serde_json::from_str(r#"{"days":{"2026-11-16":{"blocks":3}}}"#).unwrap();
        assert_eq!(partial.days["2026-11-16"], Day { blocks: 3, minutes: 0 });
    }

    #[test]
    fn sample_is_a_running_block() {
        let s = sample();
        let now = crate::status::now_ms();
        assert_eq!(s.phase, Phase::Running);
        let left = s.left_secs(now);
        assert!((12 * 60..14 * 60).contains(&left), "{left}");
        assert_eq!(s.today(now).blocks, 3);
    }
}
