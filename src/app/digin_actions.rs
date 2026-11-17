//! Digin on the board: open an issue, inbox line or page from the lists, read it, and act on
//! it (move an issue along its statuses, mark a line read, open it in Digin).
//!
//! The knob on an open item walks one line of stops: down the text, then along the action
//! bar (turning left from the first action goes back up into the text). A press on the text
//! jumps to the actions, landing on the issue's next status or "Mark read"; a press on an
//! action does it. Done and Canceled are two-step: the first press arms, a second within
//! [`ARM`] applies. Double-click (App::back) returns to the list.
//!
//! Requests go to the digin thread (`digin::Cmd`); its answers come back through
//! `Event::Digin` as a `digin::Reply` and land in [`App::digin_reply`].
use super::*;
use crate::digin::{Action, Body, Cmd, Detail, Reply};

/// How long a closing status waits for its second press.
const ARM: Duration = Duration::from_secs(4);
/// Lines the text moves per knob detent.
const SCROLL_STEP: i32 = 3;
/// A short rising chirp when a change is saved.
const SAVED_TONE: &[(u8, u64)] = &[(55, 45), (0, 25), (80, 70)];

impl App {
    /// Opens row `i` of the Digin lists (issues, then inbox, then pages) on the board.
    pub(super) fn digin_open(&mut self, i: usize) {
        let d = &self.digin;
        let (ni, nn) = (d.issues.len(), d.inbox.len());
        let opened = if i < ni {
            d.issues.get(i).filter(|x| x.id != 0).map(Detail::issue)
        } else if i < ni + nn {
            d.inbox.get(i - ni).filter(|n| n.id != 0).map(Detail::note)
        } else {
            d.pages.get(i - ni - nn).filter(|p| p.id != 0).map(Detail::page)
        };
        self.tile = i;
        match opened {
            Some((detail, cmd)) => {
                device::log(format!("digin: open {}", detail.key));
                self.level = Level::Detail(detail.key.clone());
                self.scroll = 0;
                self.digin.detail = Some(detail);
                let _ = self.digin_cmd.send(cmd);
            }
            // Nothing to fetch it by (an older server): the browser, as before.
            None => {
                if let Some(url) = render::digin_url(&self.digin, i) {
                    open_target(&url);
                }
            }
        }
    }

    /// The open item, if the board is showing it.
    fn digin_detail(&mut self) -> Option<&mut Detail> {
        let Level::Detail(key) = &self.level else { return None };
        self.digin.detail.as_mut().filter(|d| &d.key == key)
    }

    /// Knob press on the Digin page past the top level.
    pub(super) fn digin_press(&mut self) {
        if !matches!(self.level, Level::Detail(_)) {
            return self.digin_open(self.tile);
        }
        let Some(d) = self.digin_detail() else { return };
        match d.focus {
            None => {
                d.focus = Some(d.body.press_action());
                d.armed = None;
            }
            Some(i) => self.digin_apply(i),
        }
    }

    /// Knob turn on an open item: scroll the text, then move along the actions.
    pub(super) fn digin_turn(&mut self, delta: i32) {
        let max = render::digin_scroll_max(&self.fonts, &self.digin);
        let scroll = self.scroll.min(max);
        let Some(d) = self.digin_detail() else { return };
        if d.busy.is_some() {
            return; // the knob stays on the change being saved
        }
        d.armed = None;
        let n = d.body.actions().len();
        let mut new_scroll = scroll;
        match d.focus {
            None if delta > 0 && scroll >= max => d.focus = (n > 0).then(|| d.body.entry_action().min(n - 1)),
            None => new_scroll = (scroll as i32 + delta * SCROLL_STEP).clamp(0, max as i32) as usize,
            Some(i) => {
                let j = i as i32 + delta;
                d.focus = if j < 0 { None } else { Some((j as usize).min(n.saturating_sub(1))) };
            }
        }
        self.scroll = new_scroll;
    }

    /// A tap on the Digin page: a row opens it; on an open item, an action does it.
    pub(super) fn digin_tap(&mut self, i: usize) {
        if self.digin_detail().is_none() {
            return self.digin_open(i);
        }
        if i == render::DG_BACK {
            return self.back();
        }
        self.digin_apply(i);
    }

    /// Does action `i` of the open item (a closing status only on its second press).
    fn digin_apply(&mut self, i: usize) {
        let Some(d) = self.digin_detail() else { return };
        let Some(action) = d.body.actions().get(i).cloned() else { return };
        d.focus = Some(i);
        let cmd = match action {
            Action::Open => {
                let url = d.body.url().to_string();
                device::log(format!("digin: open {url} in the browser"));
                open_target(&url);
                return;
            }
            Action::MarkRead => {
                let Body::Note(n) = &d.body else { return };
                if d.busy.is_some() || n.read {
                    return;
                }
                let cmd = Cmd::MarkRead { key: d.key.clone(), account: d.account, id: n.id, channel: n.channel, message: n.message };
                d.busy = Some("Marking it read…".into());
                d.said = None;
                cmd
            }
            Action::Status(s) => {
                let Body::Issue(x) = &d.body else { return };
                if d.busy.is_some() || d.body.current() == Some(i) {
                    return;
                }
                let confirmed = d.armed.is_some_and(|(k, at)| k == i && at.elapsed() < ARM);
                if s.closes() && !confirmed {
                    d.armed = Some((i, Instant::now()));
                    d.said = None;
                    // Wake the board when the second press stops counting.
                    let _ = self.digin_cmd.send(Cmd::Nudge(ARM + Duration::from_millis(50)));
                    return;
                }
                let cmd = Cmd::SetStatus { key: d.key.clone(), account: d.account, issue: x.id, status: s.clone() };
                d.armed = None;
                d.busy = Some(format!("Moving it to {}…", s.name));
                d.said = None;
                device::log(format!("digin: {} → {}", x.key, s.name));
                cmd
            }
        };
        let _ = self.digin_cmd.send(cmd);
    }

    /// The digin thread's answer about the open item.
    pub(super) fn digin_reply(&mut self, r: Reply) {
        let Some(d) = self.digin.detail.as_mut() else { return };
        let mut chirp = false;
        match r {
            Reply::Tick => {
                if d.armed.is_some_and(|(_, at)| at.elapsed() >= ARM) {
                    d.armed = None;
                }
            }
            Reply::Opened(key, res) if key == d.key => {
                d.loading = false;
                match res {
                    Ok(body) => {
                        // The bar grows (an issue's statuses arrive): keep the knob on the
                        // same action, never on whatever now has its place.
                        let was = d.focus.and_then(|f| d.body.actions().get(f).cloned());
                        d.body = *body;
                        d.error = None;
                        d.armed = None;
                        d.focus = was.and_then(|a| d.body.actions().iter().position(|b| *b == a));
                    }
                    Err(e) => d.error = Some(e),
                }
            }
            Reply::Saved(key, res) if key == d.key => {
                d.busy = None;
                match res {
                    Ok(now) => {
                        if let Body::Issue(x) = &mut d.body {
                            x.status = now.category.clone();
                            x.status_name = now.name.clone();
                            if now.id != 0 {
                                x.status_id = now.id;
                            }
                        }
                        d.said = Some((format!("Moved to {}", now.name), true));
                        chirp = true;
                    }
                    Err(e) if digin::needs_permit(&e) => d.said = Some((e, false)),
                    Err(e) => d.said = Some((format!("Couldn't change the status: {e}"), false)),
                }
            }
            Reply::Read(key, res) if key == d.key => {
                d.busy = None;
                match res {
                    Ok(()) => {
                        if let Body::Note(n) = &mut d.body {
                            n.read = true;
                        }
                        // "Mark read" leaves the bar: the knob stays on "Open in Digin".
                        d.focus = Some(0);
                        d.said = Some(("Marked read".into(), true));
                        chirp = true;
                    }
                    Err(e) if digin::needs_permit(&e) => d.said = Some((e, false)),
                    Err(e) => d.said = Some((format!("Couldn't mark it read: {e}"), false)),
                }
            }
            _ => {}
        }
        if chirp && self.cfg.buzzer {
            let _ = self.commands.send(Command::Tone(SAVED_TONE.to_vec()));
        }
        self.redraw();
    }
}
