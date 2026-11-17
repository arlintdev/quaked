//! Now Playing and transport control for Spotify and Apple Music, through their official
//! AppleScript interfaces. macOS asks once to let Quaked control each app (Privacy &
//! Security > Automation). Volume is the player's own volume, which works on any output
//! device (USB headsets often report no system volume at all).

use crate::device::log;
use std::path::PathBuf;
use std::process::Command;
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
    VolumeBy(i32),
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
      set out to "Spotify" & tab & (player state as string) & tab & (name of t) & tab & (artist of t) & tab & (album of t) & tab & ((duration of t) / 1000) & tab & (player position) & tab & (artwork url of t) & tab & "VOL=" & (sound volume)
    end try
  end tell
end if
if out is "" or out does not contain (tab & "playing" & tab) then
  if application "Music" is running then
    tell application "Music"
      try
        if player state is playing or out is "" then
          set t to current track
          set out to "Music" & tab & (player state as string) & tab & (name of t) & tab & (artist of t) & tab & (album of t) & tab & (duration of t) & tab & (player position) & tab & "" & tab & "VOL=" & (sound volume)
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
fn run(cmd: &MusicCmd, track: Option<&Track>, volume: Option<u8>) -> Option<u8> {
    let app = track.map(|t| t.app.as_str()).unwrap_or("Spotify");
    let mut new_volume = None;
    let script = match cmd {
        MusicCmd::PlayPause => format!("tell application \"{app}\" to playpause"),
        MusicCmd::Next => format!("tell application \"{app}\" to next track"),
        MusicCmd::Previous => format!("tell application \"{app}\" to previous track"),
        MusicCmd::VolumeBy(d) => {
            let v = (volume.unwrap_or(50) as i32 + d).clamp(0, 100) as u8;
            new_volume = Some(v);
            format!("tell application \"{app}\" to set sound volume to {v}")
        }
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

fn music_thread(rx: Receiver<MusicCmd>, fast: Arc<AtomicBool>, emit: impl Fn(MusicEvent) + Send + Clone + 'static) {
    let mut track: Option<Track> = None;
    let mut volume: Option<u8> = None;
    let mut problem: Option<String> = None;
    let mut art_url = String::new();
    let mut last_emit = Instant::now() - Duration::from_secs(60);
    loop {
        let (t, v, p) = poll();
        let changed = match (&track, &t) {
            (Some(a), Some(b)) => !a.same_song(b) || (a.position_at(last_emit) - b.position).abs() > 2.5,
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
        let wait = if fast.load(Ordering::Relaxed) { Duration::from_secs(1) } else { Duration::from_secs(5) };
        match rx.recv_timeout(wait) {
            Ok(cmd) => {
                let mut target = volume;
                let mut pending = Some(cmd);
                // Fold a burst of knob ticks into one volume change.
                while let Some(c) = pending.take() {
                    match c {
                        MusicCmd::VolumeBy(d) => target = Some((target.unwrap_or(50) as i32 + d).clamp(0, 100) as u8),
                        MusicCmd::SetVolume(v) => target = Some(v.min(100)),
                        other => {
                            run(&other, track.as_ref(), volume);
                        }
                    }
                    pending = rx.try_recv().ok();
                }
                if target != volume {
                    if let Some(v) = target {
                        if run(&MusicCmd::SetVolume(v), track.as_ref(), volume).is_some() {
                            volume = Some(v);
                            emit(MusicEvent::State { track: track.clone(), volume, problem: problem.clone() });
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(150));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}
