//! Window switcher: the app side (knob, touch, timers). Runs on the main thread with full
//! access to App (it's a child module of app).
//!
//! The windows thread looks only while the page is shown (it refreshes itself every
//! couple of seconds); turn picks an app, press or tap brings it forward.
use super::*;
use crate::windows::{Cmd as WinCmd, Update as WinUpdate, FEEDBACK_MS};

impl App {
    /// An update from crate::windows's thread.
    pub(super) fn on_windows(&mut self, u: WinUpdate) {
        match u {
            WinUpdate::Status(st) => {
                // The order follows the Mac's windows; keep the knob on the same app.
                let picked = (self.page == PageId::Windows && self.level != Level::Top).then(|| self.windows.apps.get(self.tile).map(|a| a.pid)).flatten();
                let fresh = st.switched.clone().filter(|s| self.windows.switched.as_ref() != Some(s));
                self.windows = st;
                if self.page == PageId::Windows {
                    // Just switched from here: follow that app (it's in front now).
                    let follow = fresh.and_then(|(name, _)| self.windows.apps.iter().position(|a| a.front && a.name == name));
                    if let Some(i) = follow.or_else(|| picked.and_then(|pid| self.windows.apps.iter().position(|a| a.pid == pid))) {
                        self.tile = i;
                    }
                    self.tile = self.tile.min(self.windows.apps.len().saturating_sub(1));
                    self.redraw();
                }
            }
        }
    }

    /// Knob turned (on its page, past the top level).
    pub(super) fn windows_turn(&mut self, d: i32) {
        let n = self.windows.apps.len() as i32;
        if n > 0 {
            self.tile = (self.tile as i32 + d).clamp(0, n - 1) as usize;
        }
    }

    /// Knob pressed on its page (past the top level).
    pub(super) fn windows_press(&mut self) {
        self.windows_activate(self.tile);
    }

    /// Hit::Feature(_, i) tapped.
    pub(super) fn windows_tap(&mut self, i: usize) {
        self.tile = i;
        self.windows_activate(i);
    }

    fn windows_activate(&mut self, i: usize) {
        if let Some(a) = self.windows.apps.get(i) {
            let _ = self.windows_cmd.send(WinCmd::Activate(a.pid));
        }
    }

    /// A hold on its page: return true to take it (otherwise the board goes to the top level).
    pub(super) fn windows_hold(&mut self) -> bool {
        false
    }

    /// Its page was shown (true) or left (false): the thread looks only while it's shown.
    pub(super) fn windows_page_shown(&mut self, shown: bool) {
        let _ = self.windows_cmd.send(WinCmd::Shown(shown));
    }

    /// Called on every loop; wakes when the "Switched to" note should go away.
    pub(super) fn windows_tick(&mut self, now: Instant) -> Option<Instant> {
        let end = self.windows.switched.as_ref()?.1 + FEEDBACK_MS;
        let now_ms = status::now_ms();
        if now_ms >= end {
            self.windows.switched = None;
            if self.page == PageId::Windows {
                self.redraw();
            }
            return None;
        }
        Some(now + Duration::from_millis(end - now_ms))
    }

    /// While true the board redraws every second.
    pub(super) fn windows_animating(&self) -> bool {
        false
    }

    /// Its part of `quaked ctl status`.
    pub(super) fn windows_json(&self) -> serde_json::Value {
        let w = &self.windows;
        serde_json::json!({
            "enabled": !w.off,
            "apps": w.apps.len(),
            "windows": w.apps.iter().map(|a| a.windows).sum::<usize>(),
            "front": w.apps.iter().find(|a| a.front).map(|a| a.name.clone()),
            "titlesHidden": w.titles_hidden,
        })
    }
}
