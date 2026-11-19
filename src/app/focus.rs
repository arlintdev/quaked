//! Focus timer: the app side (knob, touch, ring, timers). The timer runs here on the main
//! thread, whatever page the board shows; crate::focus has its rules and the tally file.
use super::*;
use crate::focus::{clamp_minutes, day_key, Cmd, Phase, Update, DONE_LINGER_MS, DONE_RING_MS, STEP_MIN};

/// Played when a block ends: two blips and a long one.
const DONE_TONE: &[(u8, u64)] = &[(70, 110), (0, 90), (70, 110), (0, 90), (85, 320)];

impl App {
    /// An update from crate::focus's thread.
    pub(super) fn on_focus(&mut self, u: Update) {
        match u {
            Update::Loaded(store) => {
                // Anything counted before the file was read is added to it.
                let counted_early = !self.focus.days.is_empty();
                for (k, d) in store.days {
                    let e = self.focus.days.entry(k).or_default();
                    e.blocks += d.blocks;
                    e.minutes += d.minutes;
                }
                self.focus.loaded = true;
                // A block that was running when quaked last stopped carries on (and is
                // finished on the next tick if it ran out meanwhile).
                let sane = |a: &crate::focus::Active| match a.phase {
                    Phase::Running => a.ends_ms > 0,
                    Phase::Paused => a.left_ms > 0,
                    _ => false,
                };
                if let Some(a) = store.active.filter(|a| self.focus.phase == Phase::Ready && sane(a)) {
                    let st = &mut self.focus;
                    st.minutes = clamp_minutes(a.minutes);
                    st.started_ms = a.started_ms;
                    if a.phase == Phase::Paused {
                        st.phase = Phase::Paused;
                        st.left_ms = a.left_ms.min(st.total_ms());
                    } else {
                        st.phase = Phase::Running;
                        st.ends_ms = a.ends_ms;
                    }
                    device::log(format!("focus: picked up a {} min block ({})", st.minutes, st.phase.name()));
                }
                if counted_early {
                    self.focus_save();
                }
                self.update_ring();
                self.redraw();
            }
        }
    }

    /// Knob turned (on its page, past the top level): the length, while not running. A
    /// minute a click; a fast spin moves 5, then 15 at a time.
    pub(super) fn focus_turn(&mut self, d: i32) {
        let step = self.focus_spin.click(Instant::now(), d);
        self.focus_step(d, step);
    }

    fn focus_step(&mut self, d: i32, step: u64) {
        if self.focus.adjust(d, step) {
            self.update_ring();
            self.redraw();
        }
    }

    /// Knob pressed on its page (past the top level): start, pause, resume; on a
    /// finished block, back to ready.
    pub(super) fn focus_press(&mut self) {
        match self.focus.phase {
            Phase::Ready => self.focus_start(),
            Phase::Running => {
                if self.focus.pause(status::now_ms()) {
                    device::log("focus: paused");
                    self.focus_changed();
                }
            }
            Phase::Paused => {
                if self.focus.resume(status::now_ms()) {
                    device::log("focus: resumed");
                    self.focus_changed();
                }
            }
            Phase::Done => {
                if self.focus.dismiss() {
                    self.focus_changed();
                }
            }
        }
    }

    /// Hit::Feature(_, i) tapped: 0 the big button (or the dial), 1 / 2 shorter / longer,
    /// 3 cancel (or dismiss a finished block).
    pub(super) fn focus_tap(&mut self, i: usize) {
        match i {
            0 if self.focus.phase == Phase::Done => self.focus_start(),
            0 => self.focus_press(),
            1 => self.focus_step(-1, STEP_MIN),
            2 => self.focus_step(1, STEP_MIN),
            3 if self.focus.active() => self.focus_cancel(),
            3 => self.focus_press(),
            _ => {}
        }
    }

    /// A hold on its page cancels a running or paused block (or clears a finished one).
    /// Only past the top level: there a hold dims the screen, which you may well want
    /// in the middle of a block.
    pub(super) fn focus_hold(&mut self) -> bool {
        if self.level == Level::Top {
            return false;
        }
        match self.focus.phase {
            Phase::Running | Phase::Paused => {
                self.focus_cancel();
                true
            }
            Phase::Done => {
                self.focus.dismiss();
                self.focus_changed();
                true
            }
            Phase::Ready => false,
        }
    }

    /// Its page was shown (true) or left (false). The timer doesn't care.
    pub(super) fn focus_page_shown(&mut self, _shown: bool) {}

    /// The ring, while a block runs, is paused or has just ended (on every page).
    pub(super) fn focus_ring(&self) -> Option<Command> {
        let look = self.focus.ring(status::now_ms(), self.cfg.ring.brightness)?;
        let (hue, sat) = config::hue_sat(look.rgb);
        Some(Command::Ring { effect: look.effect, hue, sat, brightness: look.brightness, speed: look.speed })
    }

    /// Called on every loop; returns when it next needs waking.
    pub(super) fn focus_tick(&mut self, now: Instant) -> Option<Instant> {
        let now_ms = status::now_ms();
        // The length from the settings: at startup, and when it changes there.
        let cfg_minutes = clamp_minutes(self.cfg.focus.minutes);
        if self.focus.cfg_minutes != cfg_minutes {
            self.focus.cfg_minutes = cfg_minutes;
            if self.focus.phase == Phase::Ready || self.focus.minutes == 0 {
                self.focus.minutes = cfg_minutes;
                self.redraw();
            }
        }
        if self.focus.due(now_ms) {
            self.focus_finish(now_ms);
        }
        let at = |ms: u64| now + Duration::from_millis(ms.saturating_sub(now_ms).max(5));
        match self.focus.phase {
            Phase::Running => {
                // The ring moves in steps; update_ring only sends it when it changes.
                self.update_ring();
                // On each whole second (the block ends on one), for the countdown.
                Some(at(now_ms - now_ms % 1000 + 1005))
            }
            Phase::Done => {
                // The green breathing starts once the alert's blink is over.
                if self.cfg.alerts.blink && self.attention.as_ref().is_some_and(|a| a.page == PageId::Focus) {
                    self.focus.ring_until_ms = self.focus.ring_until_ms.max(now_ms + DONE_RING_MS);
                }
                let back = self.focus.done_ms + DONE_LINGER_MS;
                if now_ms >= back {
                    self.focus.dismiss();
                    self.redraw();
                }
                self.update_ring();
                if self.focus.phase != Phase::Done {
                    return None;
                }
                let ring_end = self.focus.ring_until_ms;
                Some(at(if now_ms < ring_end { ring_end.min(back) } else { back }))
            }
            Phase::Ready | Phase::Paused => None,
        }
    }

    /// While true the board redraws every second.
    pub(super) fn focus_animating(&self) -> bool {
        self.page == PageId::Focus && self.focus.phase == Phase::Running
    }

    /// Its part of `quaked ctl status`.
    pub(super) fn focus_json(&self) -> serde_json::Value {
        let now = status::now_ms();
        let st = &self.focus;
        let today = st.today(now);
        serde_json::json!({
            "phase": st.phase.name(),
            "minutes": st.minutes,
            "leftSecs": st.left_secs(now),
            "endsAt": (st.phase == Phase::Running).then_some(st.ends_ms),
            "today": { "blocks": today.blocks, "minutes": today.minutes },
        })
    }

    // ------------------------------------------------------------ helpers

    fn focus_start(&mut self) {
        if self.focus.phase == Phase::Done {
            self.focus.dismiss();
        }
        if !self.focus.start(status::now_ms()) {
            return;
        }
        let minutes = self.focus.minutes;
        device::log(format!("focus: {minutes} min block started"));
        // The length you started with becomes the default.
        if clamp_minutes(self.cfg.focus.minutes) != minutes {
            self.cfg.focus.minutes = minutes;
            self.focus.cfg_minutes = minutes;
            self.save_config();
        }
        let shortcut = self.cfg.focus.start_shortcut.clone();
        self.focus_shortcut(&shortcut);
        self.focus_changed();
    }

    fn focus_cancel(&mut self) {
        let now = status::now_ms();
        let Some(focused) = self.focus.cancel(now) else { return };
        // The minutes you did count toward today; the block doesn't.
        self.focus.credit(&day_key(now), 0, focused / 60_000);
        device::log(format!("focus: block cancelled after {} min", focused / 60_000));
        let shortcut = self.cfg.focus.end_shortcut.clone();
        self.focus_shortcut(&shortcut);
        self.focus_changed();
    }

    /// The running block reached zero: count it, chime, flag the page, end shortcut.
    fn focus_finish(&mut self, now_ms: u64) {
        let ended = self.focus.ends_ms;
        if !self.focus.finish(ended, &day_key(ended)) {
            return;
        }
        device::log(format!("focus: {} min block done", self.focus.minutes));
        let shortcut = self.cfg.focus.end_shortcut.clone();
        self.focus_shortcut(&shortcut);
        if now_ms.saturating_sub(ended) > DONE_LINGER_MS {
            // It ran out long ago (quaked wasn't running, or the Mac slept): count it quietly.
            self.focus.dismiss();
        } else {
            self.focus.ring_until_ms = now_ms + DONE_RING_MS;
            if self.cfg.buzzer {
                let _ = self.commands.send(Command::Tone(DONE_TONE.to_vec()));
            }
            // Our own chime above, so not the alert's.
            self.alert(PageId::Focus, "Focus block done".into(), false);
        }
        self.focus_changed();
    }

    /// Runs a macOS Shortcut by name (if one is set), without waiting for it.
    fn focus_shortcut(&self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        device::log(format!("focus: running shortcut {name:?}"));
        let mut cmd = std::process::Command::new("/usr/bin/shortcuts");
        cmd.arg("run").arg(name);
        spawn_detached(cmd);
    }

    /// Keeps focus.json up to date (the tally and the block in progress).
    fn focus_save(&self) {
        if self.focus.loaded {
            let _ = self.focus_cmd.send(Cmd::Save(self.focus.store()));
        }
    }

    fn focus_changed(&mut self) {
        self.focus_save();
        self.update_ring();
        self.redraw();
    }
}
