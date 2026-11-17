//! Mac health: the app side (knob, touch, ring, timers). Scaffold: the night + health agent fills this in.
//! Runs on the main thread with full access to App (it's a child module of app).
#![allow(dead_code, unused_variables, unused_imports)]
use super::*;

impl App {
    /// An update from crate::health's thread.
    pub(super) fn on_health(&mut self, u: crate::health::Update) {
        match u {
            crate::health::Update::Status(st) => {
                self.health = st;
                self.redraw();
            }
        }
    }

    /// Knob turned (on its page, past the top level).
    pub(super) fn health_turn(&mut self, d: i32) {}

    /// Knob pressed on its page (past the top level).
    pub(super) fn health_press(&mut self) {}

    /// Hit::Feature(_, i) tapped.
    pub(super) fn health_tap(&mut self, i: usize) {}

    /// A hold on its page: return true to take it (otherwise the board goes to the top level).
    pub(super) fn health_hold(&mut self) -> bool {
        false
    }

    /// Its page was shown (true) or left (false).
    pub(super) fn health_page_shown(&mut self, shown: bool) {}

    /// The ring, when this feature wants it (after League ready checks, questions, alerts).
    pub(super) fn health_ring(&self) -> Option<Command> {
        None
    }

    /// Called on every loop; return when it next needs waking (timers, animation).
    pub(super) fn health_tick(&mut self, now: Instant) -> Option<Instant> {
        None
    }

    /// While true the board redraws every second.
    pub(super) fn health_animating(&self) -> bool {
        false
    }

    /// Its part of `quaked ctl status`.
    pub(super) fn health_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// `quaked ctl health`: this Mac's numbers, for the paired Mac's Health page (JSON).
    pub(super) fn health_peer_json(&self) -> String {
        "{}".into()
    }

    /// The paired Macs changed (self.peers): e.g. tell the health thread whom to ask.
    pub(super) fn health_peers_changed(&mut self) {}
}
