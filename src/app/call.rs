//! Call mode: the app side (knob, touch, ring, timers). Scaffold: the calendar + call agent fills this in.
//! Runs on the main thread with full access to App (it's a child module of app).
#![allow(dead_code, unused_variables, unused_imports)]
use super::*;

impl App {
    /// An update from crate::call's thread.
    pub(super) fn on_call(&mut self, u: crate::call::Update) {
        match u {
            crate::call::Update::Status(st) => {
                self.call = st;
                self.redraw();
            }
        }
    }

    /// Knob turned (on its page, past the top level / while the call screen is up).
    pub(super) fn call_turn(&mut self, d: i32) {}

    /// Hit::Feature(_, i) tapped.
    pub(super) fn call_tap(&mut self, i: usize) {}

    /// A hold on its page: return true to take it (otherwise the board goes to the top level).
    pub(super) fn call_hold(&mut self) -> bool {
        false
    }

    /// Its page was shown (true) or left (false).
    pub(super) fn call_page_shown(&mut self, shown: bool) {}

    /// The ring, when this feature wants it (after League ready checks, questions, alerts).
    pub(super) fn call_ring(&self) -> Option<Command> {
        None
    }

    /// Called on every loop; return when it next needs waking (timers, animation).
    pub(super) fn call_tick(&mut self, now: Instant) -> Option<Instant> {
        None
    }

    /// While true the board redraws every second.
    pub(super) fn call_animating(&self) -> bool {
        false
    }

    /// Its part of `quaked ctl status`.
    pub(super) fn call_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// While true, the call screen is up and gets the knob first.
    pub(super) fn call_active(&self) -> bool {
        self.call.active
    }

    /// Knob press / double-click / hold while the call screen is up (KNOB_PRESS etc.).
    pub(super) fn call_press(&mut self, code: u8) {}
}
