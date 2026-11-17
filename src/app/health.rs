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
                // Running low: say so once (again only after it recovers).
                let found = problems(&st.macs);
                let news: Vec<String> = found.iter().filter(|(k, _)| !self.health_flags.contains(k)).map(|(_, t)| t.clone()).collect();
                self.health_flags = found.into_iter().map(|(k, _)| k).collect();
                self.health = st;
                if let Some(r) = Self::summarize(news) {
                    self.alert(PageId::Health, r, true);
                }
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

/// What's running low on these Macs: (a key that stays the same while it lasts, the line
/// to show). Disks under 5% or 10 GB free, memory pressure critical.
pub(super) fn problems(macs: &[crate::health::Mac]) -> Vec<(String, String)> {
    const GB: u64 = 1 << 30;
    let low = |free: u64, total: u64| total > 0 && (free < 10 * GB || free * 20 < total);
    let gb = |b: u64| format!("{:.1} GB", b as f64 / GB as f64);
    let mut out = Vec::new();
    for m in macs.iter().filter(|m| m.ok) {
        if low(m.disk_free, m.disk_total) {
            out.push((format!("disk:{}", m.name), format!("{} is almost out of disk space: {} free", m.name, gb(m.disk_free))));
        }
        for v in m.volumes.iter().filter(|v| low(v.free, v.total)) {
            out.push((format!("vol:{}:{}", m.name, v.name), format!("{} on {} is almost full: {} free", v.name, m.name, gb(v.free))));
        }
        if m.pressure >= 4 {
            out.push((format!("mem:{}", m.name), format!("{} is out of memory", m.name)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::{Mac, Volume};

    #[test]
    fn low_disk_and_memory() {
        let gb = 1u64 << 30;
        let fine = Mac { name: "Mac mini".into(), ok: true, disk_free: 179 * gb, disk_total: 460 * gb, pressure: 1, volumes: vec![Volume { name: "storage".into(), free: 262 * gb, total: 931 * gb }], ..Default::default() };
        assert!(problems(std::slice::from_ref(&fine)).is_empty());
        let tight = Mac { name: "AustinM5Max".into(), ok: true, disk_free: 8 * gb, disk_total: 2000 * gb, pressure: 4, volumes: vec![Volume { name: "Backup".into(), free: 40 * gb, total: 4000 * gb }], ..Default::default() };
        let p = problems(&[fine.clone(), tight]);
        let keys: Vec<&str> = p.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["disk:AustinM5Max", "vol:AustinM5Max:Backup", "mem:AustinM5Max"]);
        assert_eq!(p[0].1, "AustinM5Max is almost out of disk space: 8.0 GB free");
        // A Mac that isn't answering has no numbers to judge.
        let quiet = Mac { name: "Old".into(), ok: false, disk_total: 100 * gb, ..Default::default() };
        assert!(problems(&[quiet]).is_empty());
    }
}
