//! Clipboard picker: the app side (knob, touch, ring, timers). Scaffold: the clipboard + windows agent fills this in.
//! Runs on the main thread with full access to App (it's a child module of app).
#![allow(dead_code, unused_variables, unused_imports)]
use super::*;

impl App {
    /// An update from crate::clipboard's thread.
    pub(super) fn on_paste(&mut self, u: crate::clipboard::Update) {
        match u {
            crate::clipboard::Update::Status(st) => {
                self.paste = st;
                self.redraw();
            }
        }
    }

    /// Knob turned (on its page, past the top level).
    pub(super) fn paste_turn(&mut self, d: i32) {}

    /// Knob pressed on its page (past the top level).
    pub(super) fn paste_press(&mut self) {}

    /// Hit::Feature(_, i) tapped.
    pub(super) fn paste_tap(&mut self, i: usize) {}

    /// A hold on its page: return true to take it (otherwise the board goes to the top level).
    pub(super) fn paste_hold(&mut self) -> bool {
        false
    }

    /// Its page was shown (true) or left (false).
    pub(super) fn paste_page_shown(&mut self, shown: bool) {}

    /// The ring, when this feature wants it (after League ready checks, questions, alerts).
    pub(super) fn paste_ring(&self) -> Option<Command> {
        None
    }

    /// Called on every loop; return when it next needs waking (timers, animation).
    pub(super) fn paste_tick(&mut self, now: Instant) -> Option<Instant> {
        None
    }

    /// While true the board redraws every second.
    pub(super) fn paste_animating(&self) -> bool {
        false
    }

    /// Its part of `quaked ctl status`.
    pub(super) fn paste_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
}
