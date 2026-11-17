//! SHIP page: the app side (knob, touch, ring, timers).
//!
//! A deploy takes two deliberate steps on the board: press (or tap a card) arms the selected
//! project, then a second press (or a tap on the armed card's red button) within 4 seconds
//! runs it. Turning, double-clicking back, a hold, leaving the page or waiting it out disarms.
//! While a deploy runs, a hold once you're in the page (or two taps on Cancel) stops it; at
//! the top level a hold still dims the screen. Taps on a CI badge open the run.
use super::*;
use crate::deploy::{Armed, Cmd, Target, Update};

/// How long an armed deploy waits for the second press.
const ARM_MS: u64 = 4000;
/// A second press / tap sooner than this after arming doesn't count (bounces, fat fingers).
const CONFIRM_GUARD_MS: u64 = 450;
/// The ring pulses green / red this long when a deploy or CI run finishes.
const PULSE_MS: u64 = 10_000;
const TOAST_MS: u64 = 4000;
/// Countdown redraws while armed.
const FRAME_MS: u64 = 125;
const ARMED_COLOR: u32 = 0xFF3B30;
const OK_COLOR: u32 = 0x30D158;

impl App {
    /// An update from crate::deploy's thread.
    pub(super) fn on_deploy(&mut self, u: Update) {
        // Live log updates only matter while the page is up.
        let live = matches!(u, Update::Run(_));
        match u {
            Update::Projects(list, scan) => {
                self.deploy.projects = list;
                self.deploy.scan = scan;
                self.deploy.loaded = true;
                // The armed project moved or went away: never fire at whatever is there now.
                if let Some(a) = self.deploy.armed.as_ref().filter(|a| !a.cancel) {
                    if self.deploy.projects.get(a.index).map(|p| &p.name) != Some(&a.name) {
                        self.deploy.armed = None;
                    }
                }
            }
            Update::Run(r) => {
                if self.deploy.run.as_ref().map(|old| old.id) != Some(r.id) {
                    self.deploy.log_hidden = false;
                }
                if !r.running() && self.deploy.armed.as_ref().is_some_and(|a| a.cancel) {
                    self.deploy.armed = None;
                }
                self.deploy.run = Some(r);
            }
            Update::Finished(f) => {
                if f.cancelled {
                    if !f.ci {
                        self.deploy_toast(format!("{} deploy cancelled", f.project));
                    }
                } else {
                    let reason = match (f.ci, f.ok) {
                        (false, true) => format!("{} deployed", f.project),
                        (false, false) => format!("{} deploy failed", f.project),
                        (true, true) => format!("{} CI passed", f.project),
                        (true, false) => format!("{} CI failed", f.project),
                    };
                    if self.cfg.pages.contains(&PageId::Deploy) {
                        self.deploy.pulse = Some((f.ok, status::now_ms() + PULSE_MS));
                    }
                    // A failure is waiting on you; a success is just news.
                    let urgency = if f.ok { super::relevance::Urgency::Normal } else { super::relevance::Urgency::High };
                    self.alert_with(PageId::Deploy, reason, true, urgency);
                }
            }
            Update::Notice(text) => self.deploy_toast(text),
        }
        if self.page == PageId::Deploy || !live {
            self.redraw();
        }
    }

    fn deploy_toast(&mut self, text: String) {
        self.deploy.toast = Some((text, status::now_ms() + TOAST_MS));
    }

    fn deploy_disarm(&mut self) -> bool {
        if self.deploy.armed.take().is_some() {
            self.update_ring();
            self.redraw();
            return true;
        }
        false
    }

    /// First step: arm project `i` (nothing runs yet).
    fn deploy_arm(&mut self, i: usize) {
        let now = status::now_ms();
        if let Some(r) = self.deploy.running() {
            let msg = if r.index == i { format!("{} is deploying  ·  hold to cancel", r.project) } else { format!("{} is deploying  ·  one at a time", r.project) };
            return self.deploy_toast(msg);
        }
        let Some(p) = self.deploy.projects.get(i) else { return };
        if p.command.trim().is_empty() {
            return self.deploy_toast(format!("{} has no deploy command", p.name));
        }
        self.deploy.armed = Some(Armed { index: i, name: p.name.clone(), cancel: false, since_ms: now, until_ms: now + ARM_MS, frame_ms: now + FRAME_MS });
        self.deploy.log_hidden = true;
        self.deploy.toast = None;
    }

    /// Second step: run the armed deploy, if it is still armed for project `i`.
    fn deploy_confirm(&mut self, i: usize) -> bool {
        let now = status::now_ms();
        let Some(a) = self.deploy.armed_deploy(now).filter(|a| a.index == i).cloned() else { return false };
        if now.saturating_sub(a.since_ms) < CONFIRM_GUARD_MS {
            return true; // too soon to count, but don't re-arm either
        }
        if self.deploy.projects.get(i).map(|p| &p.name) != Some(&a.name) {
            self.deploy.armed = None;
            return true;
        }
        device::log(format!("deploy: {} confirmed on the board", a.name));
        let _ = self.deploy_cmd.send(Cmd::Deploy(i, a.name));
        self.deploy.armed = None;
        self.deploy.log_hidden = false;
        true
    }

    /// Knob turned (on its page, past the top level).
    pub(super) fn deploy_turn(&mut self, d: i32) {
        self.deploy_disarm();
        let n = self.deploy.projects.len() as i32;
        if n > 0 {
            self.tile = (self.tile as i32 + d).clamp(0, n - 1) as usize;
        }
        self.redraw();
    }

    /// Knob pressed on its page (past the top level): arm, or run what's armed.
    pub(super) fn deploy_press(&mut self) {
        let n = self.deploy.projects.len();
        if n == 0 {
            return;
        }
        let i = self.tile.min(n - 1);
        self.tile = i;
        if !self.deploy_confirm(i) {
            self.deploy_arm(i);
        }
        self.update_ring();
        self.redraw();
    }

    /// Hit::Feature(_, i) tapped.
    pub(super) fn deploy_tap(&mut self, i: usize) {
        if self.level == Level::Top {
            self.level = Level::Focus;
        }
        match Target::from_code(i) {
            Target::Card(k) => {
                if k >= self.deploy.projects.len() {
                    return;
                }
                self.tile = k;
                // Tapping the armed card again only keeps it armed: running it takes the red button.
                if self.deploy.armed_deploy(status::now_ms()).is_none_or(|a| a.index != k) {
                    self.deploy.armed = None;
                    self.deploy_arm(k);
                }
            }
            Target::Confirm => {
                if let Some(k) = self.deploy.armed_deploy(status::now_ms()).map(|a| a.index) {
                    self.deploy_confirm(k);
                }
            }
            Target::Ci(k) => {
                self.deploy.armed = None;
                match self.deploy.projects.get(k).and_then(|p| p.ci.as_ref()).map(|c| c.url.clone()).filter(|u| !u.is_empty()) {
                    Some(url) => open_target(&url),
                    None => self.deploy_toast("No CI runs to open".into()),
                }
            }
            Target::Cancel => {
                let now = status::now_ms();
                let Some((index, name)) = self.deploy.running().map(|r| (r.index, r.project.clone())) else { return };
                let armed = self.deploy.armed.as_ref().is_some_and(|a| a.cancel && now < a.until_ms && now.saturating_sub(a.since_ms) >= CONFIRM_GUARD_MS);
                if armed {
                    let _ = self.deploy_cmd.send(Cmd::Cancel);
                    self.deploy.armed = None;
                    self.deploy_toast(format!("Cancelling {name}…"));
                } else if !self.deploy.armed.as_ref().is_some_and(|a| a.cancel) {
                    self.deploy.armed = Some(Armed { index, name, cancel: true, since_ms: now, until_ms: now + ARM_MS, frame_ms: now + FRAME_MS });
                }
            }
            Target::Close => {
                if self.deploy.running().is_none() {
                    self.deploy.log_hidden = true;
                }
            }
        }
        self.update_ring();
        self.redraw();
    }

    /// A hold on its page (once you're in it; at the top level a hold keeps dimming the
    /// screen): cancels a running deploy, disarms, or puts the last log away.
    pub(super) fn deploy_hold(&mut self) -> bool {
        if self.level == Level::Top {
            return false;
        }
        if let Some(name) = self.deploy.running().filter(|r| !r.cancelled).map(|r| r.project.clone()) {
            let _ = self.deploy_cmd.send(Cmd::Cancel);
            self.deploy.armed = None;
            self.deploy_toast(format!("Cancelling {name}…"));
            self.update_ring();
            self.redraw();
            return true;
        }
        if self.deploy.running().is_some() {
            return true; // already cancelling
        }
        if self.deploy_disarm() {
            return true;
        }
        if self.deploy.log_shown() {
            self.deploy.log_hidden = true;
            self.redraw();
            return true;
        }
        false
    }

    /// Its page was shown (true) or left (false).
    pub(super) fn deploy_page_shown(&mut self, shown: bool) {
        let _ = self.deploy_cmd.send(Cmd::PageShown(shown));
        if !shown {
            self.deploy.armed = None;
        }
    }

    /// The ring: a fast red breathe while a deploy is armed, then a green / red pulse when
    /// a deploy or CI run finishes.
    pub(super) fn deploy_ring(&self) -> Option<Command> {
        let now = status::now_ms();
        let brightness = self.cfg.ring.brightness;
        if self.deploy.armed.as_ref().is_some_and(|a| now < a.until_ms) && self.page == PageId::Deploy {
            let (hue, sat) = config::hue_sat(ARMED_COLOR);
            return Some(Command::Ring { effect: proto::EFFECT_BREATHING, hue, sat, brightness: brightness.max(200), speed: 255 });
        }
        if let Some((ok, _)) = self.deploy.pulse.filter(|(_, until)| now < *until) {
            let (hue, sat) = config::hue_sat(if ok { OK_COLOR } else { ARMED_COLOR });
            return Some(Command::Ring { effect: proto::EFFECT_BREATHING, hue, sat, brightness: brightness.max(180), speed: 200 });
        }
        None
    }

    /// Expires the arming, the pulse and the strip message; redraws the countdown.
    pub(super) fn deploy_tick(&mut self, now: Instant) -> Option<Instant> {
        let ms = status::now_ms();
        let at = |t: u64| now + Duration::from_millis(t.saturating_sub(ms));
        let mut wake: Option<Instant> = None;
        let soonest = |t: Instant, wake: &mut Option<Instant>| *wake = Some(wake.map_or(t, |w| w.min(t)));
        // Leaving the page or backing out to the top level disarms.
        if self.deploy.armed.is_some() && (self.page != PageId::Deploy || self.level == Level::Top) {
            self.deploy_disarm();
        }
        let (mut expired, mut frame) = (false, false);
        if let Some(a) = &mut self.deploy.armed {
            if ms >= a.until_ms {
                expired = true;
            } else {
                if ms >= a.frame_ms {
                    a.frame_ms = ms + FRAME_MS;
                    frame = true;
                }
                soonest(at(a.frame_ms), &mut wake);
                soonest(at(a.until_ms), &mut wake);
            }
        }
        if expired {
            self.deploy_disarm();
        } else if frame {
            self.redraw();
        }
        if let Some((_, until)) = self.deploy.pulse {
            if ms >= until {
                self.deploy.pulse = None;
                self.update_ring();
            } else {
                soonest(at(until), &mut wake);
            }
        }
        if let Some((_, until)) = &self.deploy.toast {
            if ms >= *until {
                self.deploy.toast = None;
                self.redraw();
            } else {
                soonest(at(*until), &mut wake);
            }
        }
        wake
    }

    /// While true the board redraws every second (elapsed times, CI clocks).
    pub(super) fn deploy_animating(&self) -> bool {
        self.page == PageId::Deploy && (self.deploy.running().is_some() || self.deploy.armed.is_some() || self.deploy.projects.iter().any(|p| p.ci.as_ref().is_some_and(|c| c.running())))
    }

    /// Its part of `quaked ctl status`.
    pub(super) fn deploy_json(&self) -> serde_json::Value {
        let d = &self.deploy;
        let now = status::now_ms();
        serde_json::json!({
            "projects": d.projects.iter().map(|p| serde_json::json!({
                "name": p.name,
                "path": p.path,
                "command": p.command,
                "repo": p.repo,
                "branch": p.branch,
                "ahead": p.ahead,
                "behind": p.behind,
                "dirty": p.dirty,
                "commit": p.subject,
                "gitError": p.git_error,
                "ci": p.ci.as_ref().map(|c| serde_json::json!({
                    "workflow": c.workflow, "status": c.status, "conclusion": c.conclusion,
                    "job": c.job, "step": c.step, "url": c.url,
                })),
                "ciError": p.ci_error,
                "lastDeploy": p.last.as_ref().map(|l| serde_json::json!({ "ok": l.ok, "cancelled": l.cancelled, "exit": l.exit, "endedMs": l.ended_ms, "secs": l.secs })),
            })).collect::<Vec<_>>(),
            "run": d.run.as_ref().map(|r| serde_json::json!({
                "project": r.project,
                "running": r.running(),
                "ok": r.ok(),
                "cancelled": r.cancelled,
                "exit": r.exit,
                "seconds": r.secs(now) as u64,
                "lines": r.total,
                "tail": r.lines.iter().rev().take(5).rev().collect::<Vec<_>>(),
            })),
            "armed": d.armed.as_ref().filter(|a| now < a.until_ms).map(|a| serde_json::json!({ "project": a.name, "cancel": a.cancel })),
        })
    }
}
