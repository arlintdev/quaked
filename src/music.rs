//! Now Playing and transport control for Spotify and Apple Music, through their official
//! AppleScript interfaces. macOS asks once to let Quaked control each app (Privacy &
//! Security > Automation). Volume is the player's own volume, which works on any output
//! device (USB headsets often report no system volume at all).

use crate::device::log;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tiny_skia::Pixmap;

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub app: String,
    pub playing: bool,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: f64,
    pub position: f64,
    pub art_url: String,
    /// The player's shuffle is on.
    pub shuffle: bool,
}

impl Track {
    /// Playback position now, extrapolated from when it was read.
    pub fn position_at(&self, read_at: Instant) -> f64 {
        let p = if self.playing { self.position + read_at.elapsed().as_secs_f64() } else { self.position };
        if self.duration > 0.0 { p.min(self.duration) } else { p }
    }

    fn same_song(&self, other: &Track) -> bool {
        self.app == other.app && self.title == other.title && self.artist == other.artist && self.playing == other.playing
    }
}

#[derive(Debug, Clone)]
pub enum MusicCmd {
    PlayPause,
    Next,
    Previous,
    /// Turns the player's shuffle on or off.
    Shuffle,
    SetVolume(u8),
}

pub enum MusicEvent {
    /// Current track (None when nothing is open), system volume, and a problem to show.
    State { track: Option<Track>, volume: Option<u8>, problem: Option<String> },
    Artwork { url: String, image: Option<Pixmap> },
}

const POLL: &str = r#"
set out to ""
if application "Spotify" is running then
  tell application "Spotify"
    try
      set t to current track
      set out to "Spotify" & tab & (player state as string) & tab & (name of t) & tab & (artist of t) & tab & (album of t) & tab & ((duration of t) / 1000) & tab & (player position) & tab & (artwork url of t) & tab & (shuffling as string) & tab & "VOL=" & (sound volume)
    end try
  end tell
end if
if out is "" or out does not contain (tab & "playing" & tab) then
  if application "Music" is running then
    tell application "Music"
      try
        if player state is playing or out is "" then
          set t to current track
          set out to "Music" & tab & (player state as string) & tab & (name of t) & tab & (artist of t) & tab & (album of t) & tab & (duration of t) & tab & (player position) & tab & "" & tab & (shuffle enabled as string) & tab & "VOL=" & (sound volume)
        end if
      end try
    end tell
  end if
end if
return out
"#;

fn osascript(script: &str) -> Result<String, String> {
    let out = Command::new("/usr/bin/osascript").arg("-e").arg(script).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn num(s: &str) -> f64 {
    s.trim().replace(',', ".").parse().unwrap_or(0.0)
}

fn poll() -> (Option<Track>, Option<u8>, Option<String>) {
    match osascript(POLL) {
        Ok(out) => {
            let (body, vol) = match out.rsplit_once("VOL=") {
                Some((b, v)) => (b.trim_end_matches('\t'), v.trim().parse::<u8>().ok()),
                None => (out.as_str(), None),
            };
            let f: Vec<&str> = body.split('\t').collect();
            let track = (f.len() >= 8 && !f[0].is_empty()).then(|| Track {
                app: f[0].to_string(),
                playing: f[1] == "playing",
                title: f[2].to_string(),
                artist: f[3].to_string(),
                album: f[4].to_string(),
                duration: num(f[5]),
                position: num(f[6]),
                art_url: f[7].to_string(),
                shuffle: f.get(8) == Some(&"true"),
            });
            (track, vol, None)
        }
        Err(e) if e.contains("-1743") || e.to_lowercase().contains("not authorized") => {
            (None, None, Some("Allow Quaked to control Spotify / Music: System Settings › Privacy & Security › Automation".into()))
        }
        Err(e) => (None, None, Some(crate::status::squash(&e, 120))),
    }
}

/// Runs a command; returns the new volume when it changed it.
fn run(cmd: &MusicCmd, track: Option<&Track>) -> Option<u8> {
    let app = track.map(|t| t.app.as_str()).unwrap_or("Spotify");
    let mut new_volume = None;
    let script = match cmd {
        MusicCmd::PlayPause => format!("tell application \"{app}\" to playpause"),
        MusicCmd::Next => format!("tell application \"{app}\" to next track"),
        MusicCmd::Previous => format!("tell application \"{app}\" to previous track"),
        MusicCmd::Shuffle if app == "Music" => "tell application \"Music\" to set shuffle enabled to not shuffle enabled".into(),
        MusicCmd::Shuffle => format!("tell application \"{app}\" to set shuffling to not shuffling"),
        MusicCmd::SetVolume(v) => {
            new_volume = Some((*v).min(100));
            format!("tell application \"{app}\" to set sound volume to {}", (*v).min(100))
        }
    };
    if let Err(e) = osascript(&script) {
        log(format!("music: {cmd:?} failed: {e}"));
        return None;
    }
    new_volume
}

fn art_cache() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join("Library/Caches/quaked/art")
}

/// Downloads cover art once and converts it to PNG (the format tiny-skia reads).
fn fetch_art(url: &str) -> Option<Pixmap> {
    use std::hash::{Hash, Hasher};
    let dir = art_cache();
    std::fs::create_dir_all(&dir).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    let png = dir.join(format!("{:016x}.png", h.finish()));
    if !png.exists() {
        let raw = dir.join(format!("{:016x}.download", h.finish()));
        let ok = Command::new("/usr/bin/curl").args(["-sfL", "--max-time", "10", "-o"]).arg(&raw).arg(url).status().ok()?.success();
        if !ok {
            return None;
        }
        Command::new("/usr/bin/sips").args(["-s", "format", "png", "-Z", "440"]).arg(&raw).arg("--out").arg(&png).output().ok()?;
        let _ = std::fs::remove_file(&raw);
    }
    Pixmap::load_png(&png).ok()
}

/// Polls every second while the music page is on screen (`fast`), every 5s otherwise.
pub fn spawn(fast: Arc<AtomicBool>, emit: impl Fn(MusicEvent) + Send + Clone + 'static) -> Sender<MusicCmd> {
    let (tx, rx) = std::sync::mpsc::channel::<MusicCmd>();
    std::thread::Builder::new()
        .name("music".into())
        .spawn(move || music_thread(rx, fast, emit))
        .unwrap();
    tx
}

/// How long after the last volume change polls wait (they'd hold up the glide) and the
/// player's own reading is not trusted yet.
const VOLUME_SETTLE: Duration = Duration::from_millis(1500);
/// One step of the glide toward the knob's volume.
const GLIDE_TICK: Duration = Duration::from_millis(12);
/// The volume helper quits after this long without a change.
const HELPER_IDLE: Duration = Duration::from_secs(120);

/// A long-running `osascript` (JavaScript) that sets the player's volume from lines on its
/// stdin ("Spotify 57"), so a change costs a few ms instead of a new process (~120 ms) and
/// the volume can glide between knob clicks. It applies only the newest line it has.
const VOLUME_JS: &str = r#"
ObjC.import('Foundation');
function run() {
  const input = $.NSFileHandle.fileHandleWithStandardInput;
  const out = $.NSFileHandle.fileHandleWithStandardOutput;
  let buf = '';
  for (;;) {
    const data = input.availableData;
    if (data.length == 0) return;
    buf += $.NSString.alloc.initWithDataEncoding(data, $.NSUTF8StringEncoding).js;
    const lines = buf.split('\n');
    buf = lines.pop();
    const last = lines.filter(l => l.trim()).pop();
    if (!last) continue;
    const sp = last.lastIndexOf(' ');
    let reply = 'ok';
    try { Application(last.slice(0, sp)).soundVolume = parseInt(last.slice(sp + 1), 10); } catch (e) { reply = 'error ' + e; }
    out.writeData($.NSString.alloc.initWithString(reply + '\n').dataUsingEncoding($.NSUTF8StringEncoding));
  }
}
"#;

struct VolumeHelper {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    used: Instant,
}

impl VolumeHelper {
    fn start() -> Option<VolumeHelper> {
        let mut child = Command::new("/usr/bin/osascript").args(["-l", "JavaScript", "-e", VOLUME_JS]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        std::thread::Builder::new()
            .name("music-volume".into())
            .spawn(move || {
                let mut last = String::new();
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    match line.strip_prefix("error ") {
                        Some(e) if e != last => {
                            log(format!("music: volume: {e}"));
                            last = e.to_string();
                        }
                        Some(_) => {}
                        None => last.clear(),
                    }
                }
            })
            .ok();
        Some(VolumeHelper { child, stdin, used: Instant::now() })
    }

    /// False when the helper is gone (start another).
    fn set(&mut self, app: &str, v: u8) -> bool {
        if !matches!(self.child.try_wait(), Ok(None)) {
            return false;
        }
        self.used = Instant::now();
        writeln!(self.stdin, "{app} {v}").and_then(|_| self.stdin.flush()).is_ok()
    }
}

impl Drop for VolumeHelper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The next volume on the way from `cur` to `want`: a third of the way, at least 1, so a
/// knob click (a few %) eases over a few frames and a big jump settles in ~0.2 s.
fn glide_step(cur: u8, want: u8) -> u8 {
    let diff = want as i32 - cur as i32;
    let step = (diff.abs() / 3).max(1).min(diff.abs());
    (cur as i32 + diff.signum() * step) as u8
}

/// Sets the player's volume through the helper (starting it if needed), or a one-off
/// osascript if the helper can't run.
fn set_volume(helper: &mut Option<VolumeHelper>, app: &str, v: u8) -> bool {
    for _ in 0..2 {
        if helper.is_none() {
            *helper = VolumeHelper::start();
        }
        let Some(h) = helper.as_mut() else { break };
        if h.set(app, v) {
            return true;
        }
        *helper = None;
    }
    osascript(&format!("tell application \"{app}\" to set sound volume to {v}")).is_ok()
}

fn music_thread(rx: Receiver<MusicCmd>, fast: Arc<AtomicBool>, emit: impl Fn(MusicEvent) + Send + Clone + 'static) {
    let mut track: Option<Track> = None;
    let mut volume: Option<u8> = None;
    let mut problem: Option<String> = None;
    let mut art_url = String::new();
    let mut last_emit = Instant::now() - Duration::from_secs(60);
    let mut helper: Option<VolumeHelper> = None;
    // Where the knob wants the volume, and when it last moved it.
    let mut target: Option<u8> = None;
    let mut touched = Instant::now() - Duration::from_secs(60);
    let mut next_poll = Instant::now();
    loop {
        let now = Instant::now();
        let gliding = target.is_some() && target != volume;
        if !gliding && now >= next_poll && now.duration_since(touched) >= VOLUME_SETTLE {
            let (t, v, p) = poll();
            let changed = match (&track, &t) {
                (Some(a), Some(b)) => !a.same_song(b) || a.shuffle != b.shuffle || (a.position_at(last_emit) - b.position).abs() > 2.5,
                (None, None) => false,
                _ => true,
            } || v != volume
                || p != problem;
            if changed {
                if p != problem {
                    if let Some(msg) = &p {
                        log(format!("music: {msg}"));
                    }
                }
                track = t.clone();
                volume = v;
                target = None;
                problem = p.clone();
                last_emit = Instant::now();
                emit(MusicEvent::State { track: t, volume: v, problem: p });
            }
            let url = track.as_ref().map(|t| t.art_url.clone()).unwrap_or_default();
            if url != art_url {
                art_url = url.clone();
                let e = emit.clone();
                std::thread::spawn(move || {
                    let image = if url.is_empty() { None } else { fetch_art(&url) };
                    e(MusicEvent::Artwork { url, image });
                });
            }
            next_poll = Instant::now() + if fast.load(Ordering::Relaxed) { Duration::from_secs(1) } else { Duration::from_secs(5) };
        }
        if let (true, Some(want)) = (gliding, target) {
            // Only with a player open: setting the volume would launch Spotify otherwise.
            match track.as_ref().map(|t| t.app.clone()) {
                Some(app) => {
                    let next = glide_step(volume.unwrap_or(want), want);
                    if set_volume(&mut helper, &app, next) {
                        volume = Some(next);
                        touched = Instant::now();
                        if next == want {
                            emit(MusicEvent::State { track: track.clone(), volume, problem: problem.clone() });
                        }
                    } else {
                        log("music: couldn't set the volume");
                        target = None;
                    }
                }
                None => target = None,
            }
        }
        if helper.as_ref().is_some_and(|h| h.used.elapsed() > HELPER_IDLE) {
            helper = None;
        }
        let wait = if target.is_some() && target != volume {
            GLIDE_TICK
        } else {
            next_poll.max(touched + VOLUME_SETTLE).saturating_duration_since(Instant::now()).min(Duration::from_secs(5))
        };
        let first = match rx.recv_timeout(wait) {
            Ok(cmd) => cmd,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let mut cmds = vec![first];
        while let Ok(c) = rx.try_recv() {
            cmds.push(c);
        }
        for c in cmds {
            match c {
                MusicCmd::SetVolume(v) => {
                    target = Some(v.min(100));
                    touched = Instant::now();
                }
                other => {
                    run(&other, track.as_ref());
                    // Let the player catch up, then read it again.
                    next_poll = Instant::now() + Duration::from_millis(150);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glide_eases_and_lands() {
        // A knob click (+4) eases in 1% steps; a big jump starts fast and slows down.
        let walk = |from: u8, to: u8| {
            let mut v = from;
            let mut steps = vec![];
            while v != to {
                v = glide_step(v, to);
                steps.push(v);
            }
            steps
        };
        assert_eq!(walk(50, 54), [51, 52, 53, 54]);
        assert_eq!(walk(54, 50), [53, 52, 51, 50]);
        let big = walk(20, 80);
        assert_eq!(big.first(), Some(&40));
        assert_eq!(big.last(), Some(&80));
        assert!(big.len() < 16);
        assert_eq!(walk(0, 100).last(), Some(&100));
        assert_eq!(glide_step(37, 37), 37);
    }
}
