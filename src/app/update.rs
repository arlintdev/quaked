//! Updates: the app side (the Update pane, installing here or on a paired Mac, `ctl update-*`).
//! Runs on the main thread with full access to App (it's a child module of app).
//!
//! The pane joins the tab bar while a release is out that some Mac here doesn't have yet
//! (or one is being installed), right before Settings. Its rows are this Mac and the paired
//! ones; turning picks one, pressing twice installs there (tapping its button twice too).
use super::*;
use crate::render::update::{Row, RowState, TAP_RELEASE};
use crate::update::{newer, Cmd as UpdateCmd, Phase, Update as UpdateUpdate, CURRENT};

/// How long a first press stays armed.
const ARM_FOR: Duration = Duration::from_secs(6);
/// A paired Mac that hasn't come back on the new version by then didn't make it.
const PEER_INSTALL_FOR: Duration = Duration::from_secs(5 * 60);

impl App {
    /// An update from crate::update's thread.
    pub(super) fn on_update(&mut self, u: UpdateUpdate) {
        match u {
            UpdateUpdate::Status(st) => {
                self.update = st;
                // The pane joins the tab bar first: the notice points at its tab.
                self.refresh_pages();
                if let Some(r) = self.update.available().cloned() {
                    if self.update_announced != r.version {
                        self.update_announced = r.version.clone();
                        self.alert_with(PageId::Update, format!("quaked {} is out", r.version), false, Urgency::Low);
                    }
                }
                self.redraw();
            }
            UpdateUpdate::Peer(name, done) => {
                let version = self.update.latest.as_ref().map(|r| r.version.clone()).unwrap_or_default();
                match done {
                    Ok(()) => {
                        self.update_errors.remove(&name);
                        self.update_peers.insert(name.clone(), (version.clone(), Instant::now()));
                        self.alert_from(PageId::Update, format!("Installing quaked {version} on {name}"), false, Urgency::Low, Some(&name));
                    }
                    Err(e) => {
                        self.alert_from(PageId::Update, format!("{name} can't update: {e}"), false, Urgency::Low, Some(&name));
                        self.update_errors.insert(name, e);
                    }
                }
                self.refresh_pages();
                self.redraw();
            }
        }
    }

    /// The tab bar: the configured pages, plus Update while it has something to show (and
    /// while you're on it).
    pub(super) fn refresh_pages(&mut self) {
        let mut pages = self.cfg.pages.clone();
        let wanted = self.page == PageId::Update || self.update.busy() || self.update_rows().iter().any(|r| matches!(r.state, RowState::Behind | RowState::Busy(_) | RowState::Failed(_)));
        if wanted && !pages.contains(&PageId::Update) {
            let at = pages.iter().position(|p| *p == PageId::Settings).unwrap_or(pages.len());
            pages.insert(at, PageId::Update);
        }
        self.pages = pages;
    }

    /// This Mac and the paired ones, with where each stands.
    pub(super) fn update_rows(&self) -> Vec<Row> {
        let latest = self.update.latest.as_ref().map(|r| r.version.as_str());
        let here_state = match &self.update.phase {
            p @ (Phase::Downloading | Phase::Verifying | Phase::Installing | Phase::Restarting) => RowState::Busy(p.label(latest.unwrap_or("")).unwrap_or_default()),
            Phase::Failed(e) if self.update.available().is_some() => RowState::Failed(e.clone()),
            _ if self.update.available().is_some() => RowState::Behind,
            _ => RowState::Current,
        };
        let mut rows = vec![Row { name: self.peers.name.clone(), here: true, version: CURRENT.into(), build: env!("QUAKED_BUILD").into(), state: here_state }];
        for p in self.peers.peers.iter().filter(|p| p.ok) {
            let caught_up = !p.version.is_empty() && latest.is_some_and(|l| !newer(l, &p.version));
            let state = if caught_up {
                RowState::Current
            } else if let Some((target, since)) = self.update_peers.get(&p.name) {
                if since.elapsed() > PEER_INSTALL_FOR {
                    RowState::Failed(format!("didn't come back on {target}"))
                } else {
                    RowState::Busy("Installing…".into())
                }
            } else if let Some(e) = self.update_errors.get(&p.name) {
                RowState::Failed(e.clone())
            } else if p.version.is_empty() {
                RowState::Unknown
            } else if latest.is_some_and(|l| newer(l, &p.version)) {
                RowState::Behind
            } else {
                RowState::Current
            };
            rows.push(Row { name: p.name.clone(), here: false, version: p.version.clone(), build: p.build.clone(), state });
        }
        rows
    }

    /// The armed row, while it's still armed.
    pub(super) fn update_armed_row(&self) -> Option<usize> {
        self.update_armed.filter(|(_, at)| at.elapsed() < ARM_FOR).map(|(i, _)| i)
    }

    /// First press on a row arms it; a second one installs there.
    fn update_activate(&mut self, i: usize) {
        let rows = self.update_rows();
        let Some(row) = rows.get(i) else { return };
        if !matches!(row.state, RowState::Behind | RowState::Failed(_)) {
            return;
        }
        if self.update_armed_row() != Some(i) {
            self.update_armed = Some((i, Instant::now()));
            return;
        }
        self.update_armed = None;
        self.update_install_on(&row.name.clone(), row.here);
    }

    /// Installs the latest release here, or asks the paired Mac `name` to.
    fn update_install_on(&mut self, name: &str, here: bool) -> Vec<u8> {
        if here {
            if self.update.busy() {
                return b"error: already installing\n".to_vec();
            }
            // No refusing on what's known: the last check may be from before the release
            // came out, so the thread asks GitHub again unless it already knows of one.
            device::log("update: install here");
            let _ = self.update_cmd.send(UpdateCmd::Install);
            return b"ok\n".to_vec();
        }
        let Some(p) = self.peers.peers.iter().find(|p| p.ok && p.name == name) else {
            return format!("error: {name} isn't answering\n").into_bytes();
        };
        device::log(format!("update: ask {name} to install"));
        self.update_errors.remove(name);
        let _ = self.update_cmd.send(UpdateCmd::InstallOn(p.name.clone(), p.host.clone(), p.port, self.cfg.sync.key.clone()));
        b"ok\n".to_vec()
    }

    /// `ctl update-check`, `ctl update-install [mac]` (from the settings page, a paired Mac or
    /// the command line). None = not one of these.
    pub(super) fn update_ctl(&mut self, line: &str) -> Option<Vec<u8>> {
        if line == "update-check" {
            let _ = self.update_cmd.send(UpdateCmd::Check);
            return Some(b"ok\n".to_vec());
        }
        let rest = line.strip_prefix("update-install")?;
        let name = rest.trim();
        let here = name.is_empty() || name == self.peers.name;
        let name = name.to_string();
        Some(self.update_install_on(&name, here))
    }

    /// Knob turned (on its page, past the top level): pick a Mac.
    pub(super) fn update_turn(&mut self, d: i32) {
        let n = self.update_rows().len() as i32;
        if n > 0 {
            self.tile = (self.tile as i32 + d).clamp(0, n - 1) as usize;
        }
        self.update_armed = None;
    }

    /// Knob pressed on its page (past the top level).
    pub(super) fn update_press(&mut self) {
        self.update_activate(self.tile);
    }

    /// Hit::Feature(_, i): row i's button, or the release card (its page on GitHub).
    pub(super) fn update_tap(&mut self, i: usize) {
        if i == TAP_RELEASE {
            if let Some(r) = &self.update.latest {
                open_target(&r.url);
            }
            return;
        }
        self.tile = i;
        if self.level == Level::Top {
            self.level = Level::Focus;
        }
        self.update_activate(i);
    }

    /// Its page was shown (true) or left (false).
    pub(super) fn update_page_shown(&mut self, shown: bool) {
        self.update_armed = None;
        if shown {
            self.tile = self.update_rows().iter().position(|r| matches!(r.state, RowState::Behind | RowState::Failed(_))).unwrap_or(0);
        } else {
            // Gone from the tab bar once there's nothing left to do.
            self.refresh_pages();
        }
    }

    /// Called on every loop: an armed press running out, paired Macs coming back updated.
    pub(super) fn update_tick(&mut self, now: Instant) -> Option<Instant> {
        if let Some((_, at)) = self.update_armed {
            if at.elapsed() >= ARM_FOR {
                self.update_armed = None;
                self.redraw();
            } else {
                return Some(at + ARM_FOR);
            }
        }
        // An old error says nothing about a Mac that has since caught up (by hand, say).
        if let Some(l) = self.update.latest.as_ref().map(|r| r.version.clone()) {
            let caught_up = |n: &String| self.peers.peers.iter().any(|p| &p.name == n && !p.version.is_empty() && !newer(&l, &p.version));
            let stale: Vec<String> = self.update_errors.keys().filter(|n| caught_up(n)).cloned().collect();
            for n in stale {
                self.update_errors.remove(&n);
            }
        }
        // A paired Mac back on the version it was asked to install.
        let done: Vec<String> = self.update_peers.iter().filter(|(n, (v, _))| self.peers.peers.iter().any(|p| &p.name == *n && &p.version == v)).map(|(n, _)| n.clone()).collect();
        for name in done {
            if let Some((v, _)) = self.update_peers.remove(&name) {
                self.alert_from(PageId::Update, format!("{name} is on quaked {v}"), false, Urgency::Low, Some(&name));
            }
        }
        if !self.update_peers.is_empty() {
            self.refresh_pages();
            return Some(now + Duration::from_secs(2));
        }
        None
    }

    /// Its part of `quaked ctl status`.
    pub(super) fn update_json(&self) -> serde_json::Value {
        let st = &self.update;
        let state = |s: &RowState| match s {
            RowState::Current => serde_json::json!({"state": "current"}),
            RowState::Behind => serde_json::json!({"state": "behind"}),
            RowState::Busy(t) => serde_json::json!({"state": "busy", "text": t}),
            RowState::Failed(t) => serde_json::json!({"state": "failed", "text": t}),
            RowState::Unknown => serde_json::json!({"state": "unknown"}),
        };
        serde_json::json!({
            "current": CURRENT,
            "build": env!("QUAKED_BUILD"),
            "repo": crate::update::REPO,
            "checking": st.checking,
            "checkedMs": st.checked_ms,
            "error": st.error,
            "available": st.available().map(|r| r.version.clone()),
            "latest": st.latest.as_ref().map(|r| serde_json::json!({"version": r.version, "name": r.name, "notes": r.notes, "publishedMs": r.published_ms, "url": r.url, "size": r.zip_size})),
            "macs": self.update_rows().iter().map(|r| {
                let mut v = state(&r.state);
                v["name"] = r.name.clone().into();
                v["here"] = r.here.into();
                v["version"] = r.version.clone().into();
                v["build"] = r.build.clone().into();
                v
            }).collect::<Vec<_>>(),
        })
    }
}
