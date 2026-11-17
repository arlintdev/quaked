//! Mac health: the app side (knob, touch, the paired Macs, `ctl health`).
//! Runs on the main thread with full access to App (it's a child module of app).
use super::*;
use crate::health::{Cmd as HealthCmd, Update as HealthUpdate};

impl App {
    /// An update from crate::health's thread.
    pub(super) fn on_health(&mut self, u: HealthUpdate) {
        match u {
            HealthUpdate::Status(mut st) => {
                // This Mac under the name the other Macs know it by.
                if let Some(me) = st.macs.first_mut().filter(|_| !self.peers.name.is_empty()) {
                    me.name = self.peers.name.clone();
                }
                if self.page == PageId::Health {
                    self.tile = self.tile.min(st.macs.len().saturating_sub(1));
                }
                let changed = st != self.health;
                self.health = st;
                if changed && self.page == PageId::Health {
                    self.redraw();
                }
            }
        }
    }

    /// Knob turned (on its page, past the top level): pick a Mac.
    pub(super) fn health_turn(&mut self, d: i32) {
        let n = self.health.macs.len() as i32;
        if n > 0 {
            self.tile = (self.tile as i32 + d).clamp(0, n - 1) as usize;
        }
    }

    /// Knob pressed on its page (past the top level): this Mac opens Activity Monitor.
    pub(super) fn health_press(&mut self) {
        self.health_open(self.tile);
    }

    /// Hit::Feature(_, i) tapped: Mac column `i`.
    pub(super) fn health_tap(&mut self, i: usize) {
        if i >= self.health.macs.len() {
            return;
        }
        self.tile = i;
        if i == 0 {
            self.health_open(0);
        } else {
            self.level = Level::Focus;
        }
    }

    fn health_open(&mut self, i: usize) {
        if i == 0 {
            let mut c = std::process::Command::new("/usr/bin/open");
            c.args(["-a", "Activity Monitor"]);
            spawn_detached(c);
        }
    }

    /// A hold on its page: return true to take it (otherwise the board goes to the top level).
    pub(super) fn health_hold(&mut self) -> bool {
        false
    }

    /// Its page was shown (true) or left (false): sample every 2 s and ask the paired Macs.
    pub(super) fn health_page_shown(&mut self, shown: bool) {
        let _ = self.health_cmd.send(HealthCmd::PageShown(shown));
    }

    /// Called on every loop; return when it next needs waking (timers, animation).
    pub(super) fn health_tick(&mut self, _now: Instant) -> Option<Instant> {
        None
    }

    /// While true the board redraws every second (the thread's updates redraw instead).
    pub(super) fn health_animating(&self) -> bool {
        false
    }

    /// Its part of `quaked ctl status`.
    pub(super) fn health_json(&self) -> serde_json::Value {
        let macs: Vec<serde_json::Value> = self
            .health
            .macs
            .iter()
            .map(|m| {
                serde_json::json!({
                    "name": m.name, "ok": m.ok, "error": m.error, "cpu": m.cpu, "memUsed": m.mem_used, "memTotal": m.mem_total,
                    "swapUsed": m.swap_used, "pressure": m.pressure, "gpu": m.gpu, "netIn": m.net_in.round(), "netOut": m.net_out.round(),
                    "diskFree": m.disk_free, "uptime": m.uptime_s, "power": m.power,
                })
            })
            .collect();
        serde_json::json!({ "enabled": self.cfg.health.enabled, "macs": macs, "batteries": self.health.batteries() })
    }

    /// `quaked ctl health`: this Mac's numbers, for the paired Mac's Health page (JSON).
    pub(super) fn health_peer_json(&self) -> String {
        // Someone's watching: sample quickly for a while.
        let _ = self.health_cmd.send(HealthCmd::Polled);
        if !self.cfg.health.enabled {
            return "error: Mac health is off there".into();
        }
        match self.health.macs.first() {
            Some(me) if me.at_ms > 0 => serde_json::to_string(me).unwrap_or_else(|_| "error: can't encode".into()),
            _ => "error: no numbers yet".into(),
        }
    }

    /// The paired Macs changed (self.peers): tell the health thread whom to ask.
    pub(super) fn health_peers_changed(&mut self) {
        let peers = self.peers.peers.iter().filter(|p| p.ok).map(|p| (p.name.clone(), p.host.clone(), p.port)).collect();
        let _ = self.health_cmd.send(HealthCmd::Peers(peers, self.cfg.sync.key.clone()));
    }
}
