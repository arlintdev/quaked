//! Calendar: the app side (knob, touch, ring, timers).
//! Runs on the main thread with full access to App (it's a child module of app).
//!
//! Turn walks the upcoming events, press joins the selected one (or opens it in
//! Calendar), and a meeting with a join link is announced a couple of minutes before it
//! starts: the ring blinks and the board shows the Calendar page.
use super::*;
use super::relevance::Urgency;
use crate::calendar::{self as cal, Event};
use objc2::rc::autoreleasepool;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::NSString;

/// Is there an app that opens this kind of link (zoommtg://, msteams:)?
fn can_open(url: &str) -> bool {
    autoreleasepool(|_| unsafe {
        let s = NSString::from_str(url);
        let u: *mut AnyObject = msg_send![class!(NSURL), URLWithString: &*s];
        if u.is_null() {
            return false;
        }
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: *mut AnyObject = msg_send![ws, URLForApplicationToOpenURL: u];
        !app.is_null()
    })
}

impl App {
    /// The paired Macs changed (self.peers): tell the calendar thread whose events to fetch.
    pub(super) fn calendar_peers_changed(&mut self) {
        let peers = self.peers.peers.iter().filter(|p| p.ok).map(|p| (p.name.clone(), p.host.clone(), p.port)).collect();
        let _ = self.calendar_cmd.send(cal::Cmd::Peers(peers, self.cfg.sync.key.clone()));
    }

    /// An update from crate::calendar's thread.
    pub(super) fn on_calendar(&mut self, u: crate::calendar::Update) {
        match u {
            crate::calendar::Update::Status(mut st) => {
                // The knob's pick and what's been announced live here, not in the thread.
                st.selected = std::mem::take(&mut self.calendar.selected);
                st.announced = std::mem::take(&mut self.calendar.announced);
                st.announced.retain(|k| st.events.iter().any(|e| k.rsplit_once('@').is_some_and(|(id, _)| id == e.id)));
                if !st.events.iter().any(|e| e.id == st.selected) {
                    st.selected.clear();
                }
                self.calendar = st;
                self.calendar_announce();
                self.redraw();
            }
            crate::calendar::Update::PeerDone(name, line, done) => {
                let Err(why) = done else { return };
                device::log(format!("calendar: {name} couldn't {}: {why}", line.split(' ').next().unwrap_or("")));
                // A meeting is about to start: join here rather than not at all.
                if let Some(url) = line.strip_prefix("join ") {
                    self.calendar_join(url);
                    self.alert_from(PageId::Calendar, format!("Joined here ({name}: {why})"), false, Urgency::Low, Some(""));
                } else {
                    self.alert_from(PageId::Calendar, format!("Couldn't open it on {name}: {why}"), false, Urgency::Low, Some(&name));
                }
            }
        }
    }

    /// The selected event (the knob's pick, else the featured one).
    fn calendar_selected(&self) -> Option<Event> {
        let now = status::now_ms();
        let i = self.calendar.selection(now)?;
        self.calendar.upcoming(now).get(i).map(|e| (*e).clone())
    }

    /// Opens a join link, straight in the meeting app when it's installed.
    pub(super) fn calendar_join(&mut self, url: &str) {
        let target = cal::app_link(url).filter(|l| can_open(l)).unwrap_or_else(|| url.to_string());
        device::log(format!("calendar: join {}", cal::provider(url)));
        open_target(&target);
    }

    /// Joins the event on Mac `on` (an index into desk_macs, this one first; None = the one
    /// config `calendar.joinOn` picks), or shows it in Calendar on the Mac it came from when
    /// it has no link.
    fn calendar_open(&mut self, e: &Event, on: Option<usize>) {
        if e.join_url.is_empty() {
            if e.machine.is_empty() {
                open_target(&cal::event_link(&e.id));
            } else {
                let _ = self.calendar_cmd.send(cal::Cmd::OnPeer(e.machine.clone(), format!("show-event {}", cal::own_id(e))));
            }
            return;
        }
        let macs = self.desk_macs();
        let (_, primary) = cal::join_targets(&macs, &self.cfg.sides, e, self.cfg.calendar.join_on);
        match macs.get(on.unwrap_or(primary)).filter(|_| on.unwrap_or(primary) > 0) {
            Some(name) => {
                device::log(format!("calendar: join {} on {name}", cal::provider(&e.join_url)));
                let _ = self.calendar_cmd.send(cal::Cmd::OnPeer(name.clone(), format!("join {}", e.join_url)));
                self.alert_from(PageId::Calendar, format!("Joining {} on {name}", e.title), false, Urgency::Low, Some(name));
            }
            None => {
                let url = e.join_url.clone();
                self.calendar_join(&url);
            }
        }
    }

    /// `ctl join <link>` from a paired Mac: join that meeting here (only meeting links).
    pub(super) fn calendar_join_from_peer(&mut self, url: &str) -> Vec<u8> {
        if !cal::is_join(url) {
            return b"error: not a meeting link\n".to_vec();
        }
        self.calendar_join(url);
        b"ok\n".to_vec()
    }

    /// `ctl show-event <id>` from a paired Mac: show one of this Mac's events in Calendar.
    pub(super) fn calendar_show_from_peer(&mut self, id: &str) -> Vec<u8> {
        if !self.calendar.events.iter().any(|e| e.machine.is_empty() && e.id == id) {
            return b"error: no such event here\n".to_vec();
        }
        open_target(&cal::event_link(id));
        b"ok\n".to_vec()
    }

    /// Knob turned (on its page, past the top level).
    pub(super) fn calendar_turn(&mut self, d: i32) {
        let now = status::now_ms();
        let up = self.calendar.upcoming(now);
        if up.is_empty() {
            return;
        }
        let cur = self.calendar.selection(now).unwrap_or(0) as i32;
        let next = (cur + d).clamp(0, up.len() as i32 - 1) as usize;
        self.calendar.selected = up[next].id.clone();
    }

    /// Knob pressed on its page (past the top level).
    pub(super) fn calendar_press(&mut self) {
        if let Some(e) = self.calendar_selected() {
            self.calendar_open(&e, None);
        }
    }

    /// Hit::Feature(_, i) tapped: a row (or its block on the ruler) selects that event,
    /// tapping it again or the card's button joins / opens it.
    pub(super) fn calendar_tap(&mut self, i: usize) {
        if i >= cal::TAP_JOIN_ON {
            if let Some(e) = self.calendar_selected() {
                self.calendar_open(&e, Some(i - cal::TAP_JOIN_ON));
            }
            return;
        }
        if i == cal::TAP_JOIN {
            return self.calendar_press();
        }
        let now = status::now_ms();
        let Some(e) = self.calendar.upcoming(now).get(i).map(|e| (*e).clone()) else { return };
        let again = self.level != Level::Top && self.calendar.selection(now) == Some(i);
        self.calendar.selected = e.id.clone();
        if again {
            self.calendar_open(&e, None);
        } else {
            self.level = Level::Focus;
        }
    }

    /// A hold on its page: return true to take it (otherwise the board goes to the top level).
    pub(super) fn calendar_hold(&mut self) -> bool {
        false
    }

    /// Its page was shown (true) or left (false).
    pub(super) fn calendar_page_shown(&mut self, shown: bool) {
        if shown {
            // Each visit starts on the featured event, with fresh data.
            self.calendar.selected.clear();
            let _ = self.calendar_cmd.send(cal::Cmd::Refresh);
        }
    }

    /// Meetings reaching one of `jumpMinutes` before they start (15, 10, 5 and 0 by default):
    /// blink, say it and jump to the Calendar page, leading with that meeting. 5 minutes and
    /// less is critical (it takes the board from a game or a focus block too).
    fn calendar_announce(&mut self) {
        if self.cfg.calendar.jump_minutes.is_empty() || !self.calendar.authorized {
            return;
        }
        let now = status::now_ms();
        let due = self.calendar.events.iter().filter(|e| !e.all_day).find_map(|e| {
            let reached = reached_marks(&self.cfg.calendar.jump_minutes, e.start_ms, now);
            let closest = *reached.iter().min()?;
            (!self.calendar.announced.contains(&mark_key(&e.id, closest))).then(|| (e.clone(), reached, closest))
        });
        let Some((e, reached, closest)) = due else { return };
        // Every mark already passed counts as said (quaked starting 3 minutes before a
        // meeting doesn't announce 15, 10 and 5 in a row).
        for m in reached {
            let k = mark_key(&e.id, m);
            if !self.calendar.announced.contains(&k) {
                self.calendar.announced.push(k);
            }
        }
        let reason = if e.start_ms <= now {
            format!("{} is starting", e.title)
        } else {
            format!("{} in {} min", e.title, (e.start_ms - now).div_ceil(60_000))
        };
        let urgency = if closest <= 5 { Urgency::Critical } else { Urgency::High };
        self.alert_from(PageId::Calendar, reason, closest <= 5, urgency, Some(&e.machine));
        // After the alert (showing the page resets the pick): lead with this meeting.
        self.calendar.selected = e.id;
        self.redraw();
    }

    /// Called on every loop; return when it next needs waking (the next mark).
    pub(super) fn calendar_tick(&mut self, now: Instant) -> Option<Instant> {
        self.calendar_announce();
        let ms = status::now_ms();
        let marks = &self.cfg.calendar.jump_minutes;
        self.calendar
            .events
            .iter()
            .filter(|e| !e.all_day)
            .flat_map(|e| marks.iter().filter(|&&m| !self.calendar.announced.contains(&mark_key(&e.id, m))).map(move |&m| e.start_ms.saturating_sub(m as u64 * 60_000)))
            .filter(|&at| at > ms)
            .min()
            .map(|at| now + Duration::from_millis(at - ms))
    }

    /// While true the board redraws every second.
    pub(super) fn calendar_animating(&self) -> bool {
        false
    }

    /// Its part of `quaked ctl status`.
    pub(super) fn calendar_json(&self) -> serde_json::Value {
        let now = status::now_ms();
        let c = &self.calendar;
        let ev = |e: &Event| serde_json::json!({ "title": e.title, "startMs": e.start_ms, "endMs": e.end_ms, "calendar": e.calendar, "joinUrl": e.join_url });
        serde_json::json!({
            "authorized": c.authorized,
            "error": c.error,
            "events": c.events.len(),
            "upcoming": c.upcoming(now).len(),
            "selected": self.calendar_selected().as_ref().map(ev),
            "next": c.next(now).map(ev),
        })
    }
}

/// The id `announced` keeps for one meeting at one mark ("<event id>@<minutes>").
fn mark_key(id: &str, minutes: u32) -> String {
    format!("{id}@{minutes}")
}

/// The marks (minutes before `start`) already reached at `now`, while the meeting is still
/// starting (up to a minute after it begins).
fn reached_marks(marks: &[u32], start: u64, now: u64) -> Vec<u32> {
    let left = start as i64 - now as i64;
    if left <= -60_000 {
        return Vec::new();
    }
    marks.iter().copied().filter(|&m| left <= m as i64 * 60_000).collect()
}

#[cfg(test)]
mod mark_tests {
    use super::*;

    const M: u64 = 60_000;

    #[test]
    fn marks_reached_as_the_meeting_nears() {
        let marks = [15, 10, 5, 0];
        let start = 100 * M;
        assert!(reached_marks(&marks, start, start - 16 * M).is_empty());
        assert_eq!(reached_marks(&marks, start, start - 15 * M), [15]);
        assert_eq!(reached_marks(&marks, start, start - 7 * M), [15, 10]);
        assert_eq!(reached_marks(&marks, start, start - 3 * M), [15, 10, 5]);
        assert_eq!(reached_marks(&marks, start, start), [15, 10, 5, 0]);
        assert_eq!(reached_marks(&marks, start, start + 30_000), [15, 10, 5, 0]);
        assert!(reached_marks(&marks, start, start + 2 * M).is_empty());
        assert_eq!(mark_key("abc@123", 5), "abc@123@5");
    }
}
