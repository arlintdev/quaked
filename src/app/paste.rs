//! Clipboard picker: the app side (knob, touch, timers). Runs on the main thread with full
//! access to App (it's a child module of app).
//!
//! Turn picks a copy, press (or a tap on its row) pastes it into the app in front: the
//! clipboard thread puts it on the pasteboard and then asks for ⌘V, which the Quake types
//! as a USB keyboard. Hold forgets the selected copy.
use super::*;
use crate::clipboard::{Cmd as ClipCmd, Update as ClipUpdate, FEEDBACK_MS};

impl App {
    /// An update from crate::clipboard's thread.
    pub(super) fn on_paste(&mut self, u: ClipUpdate) {
        match u {
            ClipUpdate::Status(st) => {
                // Keep the knob on the same copy when new ones arrive above it, and on a
                // copy just pasted (it moves to the top).
                let fresh = st.pasted.as_ref().filter(|p| self.paste.pasted.as_ref() != Some(*p)).map(|p| p.id);
                let picked = fresh.or_else(|| self.paste_picking().then(|| self.paste.items.get(self.tile).map(|c| c.id)).flatten());
                self.paste = st;
                if self.page == PageId::Paste {
                    if let Some(i) = picked.and_then(|id| self.paste.items.iter().position(|c| c.id == id)) {
                        self.tile = i;
                    }
                    self.tile = self.tile.min(self.paste.items.len().saturating_sub(1));
                    self.redraw();
                }
            }
            ClipUpdate::TypePaste => match keys::parse("cmd+v") {
                Some((modifiers, key)) => {
                    let _ = self.commands.send(Command::Keys { modifiers, key });
                }
                None => device::log("clipboard: can't type cmd+v"),
            },
        }
    }

    /// On the page with the knob driving it.
    fn paste_picking(&self) -> bool {
        self.page == PageId::Paste && self.level != Level::Top
    }

    /// Knob turned (on its page, past the top level).
    pub(super) fn paste_turn(&mut self, d: i32) {
        let n = self.paste.items.len() as i32;
        if n > 0 {
            self.tile = (self.tile as i32 + d).clamp(0, n - 1) as usize;
        }
    }

    /// Knob pressed on its page (past the top level).
    pub(super) fn paste_press(&mut self) {
        self.paste_item(self.tile);
    }

    /// Hit::Feature(_, i) tapped.
    pub(super) fn paste_tap(&mut self, i: usize) {
        self.tile = i;
        self.paste_item(i);
    }

    fn paste_item(&mut self, i: usize) {
        if let Some(c) = self.paste.items.get(i) {
            let _ = self.paste_cmd.send(ClipCmd::Paste(c.id));
        }
    }

    /// A hold on its page: forget the selected copy (at the top level, the usual dimming).
    pub(super) fn paste_hold(&mut self) -> bool {
        if self.level == Level::Top || self.tile >= self.paste.items.len() {
            return false;
        }
        let c = self.paste.items.remove(self.tile);
        let _ = self.paste_cmd.send(ClipCmd::Forget(c.id));
        if self.paste.current == Some(c.id) {
            self.paste.current = None;
        }
        self.tile = self.tile.min(self.paste.items.len().saturating_sub(1));
        device::log("clipboard: forgot a copy");
        true
    }

    /// Its page was shown (true) or left (false).
    pub(super) fn paste_page_shown(&mut self, shown: bool) {
        let _ = self.paste_cmd.send(ClipCmd::Shown(shown));
    }

    /// Called on every loop; wakes when the "Pasted" note should go away.
    pub(super) fn paste_tick(&mut self, now: Instant) -> Option<Instant> {
        let end = self.paste.pasted.as_ref()?.at_ms + FEEDBACK_MS;
        let now_ms = status::now_ms();
        if now_ms >= end {
            self.paste.pasted = None;
            if self.page == PageId::Paste {
                self.redraw();
            }
            return None;
        }
        Some(now + Duration::from_millis(end - now_ms))
    }

    /// While true the board redraws every second.
    pub(super) fn paste_animating(&self) -> bool {
        false
    }

    /// Its part of `quaked ctl status` (counts only, never what was copied).
    pub(super) fn paste_json(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": !self.paste.off,
            "kept": self.paste.items.len(),
            "skipped": self.paste.skipped,
        })
    }
}
