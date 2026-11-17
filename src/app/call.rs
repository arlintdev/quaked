//! Call mode: the app side (knob, touch, ring, timers).
//! Runs on the main thread with full access to App (it's a child module of app).
//!
//! The screen shows while a call is on (the soundboard's listener, or a call app holding
//! a mic) unless call mode is off or you hid it with a double-click. Press mutes, turn
//! picks a clip, hold plays it, double-click hides the screen until the call ends.
use super::*;
use crate::call::{self, Mute};

/// The call screen's red (the ring and the LIVE card).
const CALL_RED: u32 = 0xFF3B30;
/// How long a press's feedback line stays up.
const NOTE_MS: u64 = 4000;
/// Wait this long after bringing the call app forward before typing its shortcut.
const KEYS_DELAY: Duration = Duration::from_millis(450);

impl App {
    /// An update from crate::call's thread.
    pub(super) fn on_call(&mut self, u: crate::call::Update) {
        match u {
            crate::call::Update::Capturing(list) => {
                self.call.capturing = list;
                self.call_refresh();
            }
        }
    }

    /// Folds the soundboard's listener, the call apps holding a mic, the settings and the
    /// calendar into `self.call`. Cheap; runs on every loop.
    fn call_refresh(&mut self) {
        let now = status::now_ms();
        let mut c = self.call.clone();
        match call::decide(self.sounds.listener.as_deref(), &c.capturing, &self.cfg.call) {
            Some((bundle, mute)) => {
                if !c.on_call {
                    c = call::Status { on_call: true, started_ms: now, capturing: c.capturing, ..Default::default() };
                }
                if c.bundle != bundle {
                    c.app = call::app_name(&bundle)
                        .map(String::from)
                        .or_else(|| crate::symbols::running_app_name(&bundle))
                        .unwrap_or_else(|| bundle.rsplit('.').next().unwrap_or(&bundle).to_string());
                    c.bundle = bundle;
                }
                c.mute = mute;
            }
            None => c = call::Status { capturing: c.capturing, ..Default::default() },
        }
        c.passthrough = self.cfg.soundboard.passthrough;
        // What's on the calendar right now (starting within five minutes counts).
        c.meeting = self
            .calendar
            .events
            .iter()
            .filter(|e| !e.all_day && e.start_ms <= now + 5 * 60_000 && e.end_ms > now)
            .max_by_key(|e| (!e.join_url.is_empty(), e.start_ms))
            .map(|e| e.title.clone())
            .unwrap_or_default();
        if c.note.as_ref().is_some_and(|(_, at)| now.saturating_sub(*at) > NOTE_MS) {
            c.note = None;
        }
        c.sel = c.sel.min(self.sounds.clips.len().min(call::CLIPS).saturating_sub(1));
        c.active = self.cfg.call.enabled && c.on_call && !c.dismissed;
        if c != self.call {
            if c.active != self.call.active {
                device::log(format!("call: screen {}", if c.active { format!("on ({})", c.app) } else { "off".into() }));
            }
            self.call = c;
            self.update_ring();
            self.redraw();
        }
    }

    fn call_note(&mut self, text: String) {
        self.call.note = Some((text, status::now_ms()));
        self.redraw();
    }

    /// Brings the call app forward.
    fn call_show_app(&self) {
        if self.call.bundle.contains('.') {
            let mut c = std::process::Command::new("/usr/bin/open");
            c.args(["-b", &self.call.bundle]);
            spawn_detached(c);
        }
    }

    /// The knob's press: mute / unmute however we can.
    fn call_mute(&mut self) {
        match self.call.mute.clone() {
            Mute::Soundboard => {
                // The soundboard publishes the new state at once; the screen and ring follow it.
                let _ = self.audio.cmd.send(audio::Cmd::ToggleMute);
            }
            Mute::Keys(combo) => {
                let Some((modifiers, key)) = keys::parse(&combo) else { return };
                self.call_show_app();
                // The Quake types it like a keyboard, so the call app has to be in front first.
                let tx = self.commands.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(KEYS_DELAY);
                    let _ = tx.send(Command::Keys { modifiers, key });
                });
                let text = format!("Sent {} to {}", call::keys_label(&combo), self.call.app);
                self.call_note(text);
            }
            Mute::App => {
                self.call_show_app();
                let text = format!("Mute in {}: brought it forward", self.call.app);
                self.call_note(text);
            }
        }
    }

    fn call_clip_count(&self) -> usize {
        self.sounds.clips.len().min(call::CLIPS)
    }

    /// Knob turned (on its page, past the top level / while the call screen is up).
    pub(super) fn call_turn(&mut self, d: i32) {
        let n = self.call_clip_count() as i32;
        if n > 0 {
            self.call.sel = (self.call.sel as i32 + d).clamp(0, n - 1) as usize;
        }
    }

    /// Hit::Feature(_, i) tapped.
    pub(super) fn call_tap(&mut self, i: usize) {
        match i {
            call::TAP_MUTE => self.call_mute(),
            call::TAP_HIDE => self.call_dismiss(),
            call::TAP_EVENT => {
                let now = status::now_ms();
                if let Some(url) = self.calendar.next(now).map(|e| e.join_url.clone()).filter(|u| !u.is_empty()) {
                    self.calendar_join(&url);
                }
            }
            i if i >= call::TAP_CLIP && i - call::TAP_CLIP < self.call_clip_count() => {
                self.call.sel = i - call::TAP_CLIP;
                self.toggle_clip(self.call.sel);
            }
            _ => {}
        }
    }

    fn call_dismiss(&mut self) {
        device::log("call: screen hidden until the call ends");
        self.call.dismissed = true;
        self.call.active = false;
        self.update_ring();
        self.redraw();
    }

    /// The ring, when this feature wants it (after League ready checks, questions, alerts).
    pub(super) fn call_ring(&self) -> Option<Command> {
        if !self.call.active {
            return None;
        }
        let (hue, sat) = config::hue_sat(CALL_RED);
        let full = self.cfg.ring.brightness.max(140);
        let muted = self.call.mute == Mute::Soundboard && (self.sounds.mic_muted || !self.call.passthrough);
        // Live: solid red. Muted: a dim ember, so a glance tells you which.
        let brightness = if muted { (full / 7).max(14) } else { full };
        Some(Command::Ring { effect: proto::EFFECT_SOLID, hue, sat, brightness, speed: 128 })
    }

    /// Called on every loop; return when it next needs waking (timers, animation).
    pub(super) fn call_tick(&mut self, now: Instant) -> Option<Instant> {
        // The soundboard's listener and mute arrive without a call update: look every loop.
        self.call_refresh();
        if self.call.active {
            self.update_ring();
            return Some(now + Duration::from_secs(1));
        }
        None
    }

    /// While true the board redraws every second.
    pub(super) fn call_animating(&self) -> bool {
        self.call.active
    }

    /// Its part of `quaked ctl status`.
    pub(super) fn call_json(&self) -> serde_json::Value {
        let c = &self.call;
        serde_json::json!({
            "active": c.active,
            "onCall": c.on_call,
            "dismissed": c.dismissed,
            "app": c.app,
            "bundle": c.bundle,
            "meeting": c.meeting,
            "startedMs": c.started_ms,
            "mute": match &c.mute {
                Mute::App => "app".to_string(),
                Mute::Soundboard => if self.sounds.mic_muted { "soundboard:muted".to_string() } else { "soundboard:live".to_string() },
                Mute::Keys(k) => format!("keys:{k}"),
            },
            "capturing": c.capturing,
        })
    }

    /// While true, the call screen is up and gets the knob first.
    pub(super) fn call_active(&self) -> bool {
        self.call.active
    }

    /// Knob press / double-click / hold while the call screen is up (KNOB_PRESS etc.).
    pub(super) fn call_press(&mut self, code: u8) {
        match code {
            KNOB_PRESS => self.call_mute(),
            KNOB_DOUBLE => self.call_dismiss(),
            KNOB_HOLD => {
                if self.call.sel < self.call_clip_count() {
                    self.toggle_clip(self.call.sel);
                }
            }
            _ => {}
        }
    }
}
