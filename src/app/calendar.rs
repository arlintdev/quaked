//! Calendar: the app side (knob, touch, ring, timers).
//! Runs on the main thread with full access to App (it's a child module of app).
//!
//! Turn walks the upcoming events, press joins the selected one (or opens it in
//! Calendar), and a meeting with a join link is announced a couple of minutes before it
//! starts: the ring blinks and the board shows the Calendar page.
use super::*;
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
    /// An update from crate::calendar's thread.
    pub(super) fn on_calendar(&mut self, u: crate::calendar::Update) {
        match u {
            crate::calendar::Update::Status(mut st) => {
                // The knob's pick and what's been announced live here, not in the thread.
                st.selected = std::mem::take(&mut self.calendar.selected);
                st.announced = std::mem::take(&mut self.calendar.announced);
                st.announced.retain(|id| st.events.iter().any(|e| &e.id == id));
                if !st.events.iter().any(|e| e.id == st.selected) {
                    st.selected.clear();
                }
                self.calendar = st;
                self.calendar_announce();
                self.redraw();
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

    /// Joins the event, or shows it in Calendar when it has no link.
    fn calendar_open(&mut self, e: &Event) {
        if e.join_url.is_empty() {
            open_target(&cal::event_link(&e.id));
        } else {
            let url = e.join_url.clone();
            self.calendar_join(&url);
        }
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
            self.calendar_open(&e);
        }
    }

    /// Hit::Feature(_, i) tapped: a row (or its block on the ruler) selects that event,
    /// tapping it again or the card's button joins / opens it.
    pub(super) fn calendar_tap(&mut self, i: usize) {
        if i == cal::TAP_JOIN {
            return self.calendar_press();
        }
        let now = status::now_ms();
        let Some(e) = self.calendar.upcoming(now).get(i).map(|e| (*e).clone()) else { return };
        let again = self.level != Level::Top && self.calendar.selection(now) == Some(i);
        self.calendar.selected = e.id.clone();
        if again {
            self.calendar_open(&e);
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

    /// Meetings with a join link that start within `alertMinutes` and haven't been announced.
    fn calendar_announce(&mut self) {
        let lead = self.cfg.calendar.alert_minutes * 60_000;
        if lead == 0 || !self.calendar.authorized {
            return;
        }
        let now = status::now_ms();
        let due = self.calendar.events.iter().find(|e| !e.all_day && !e.join_url.is_empty() && e.start_ms > now && e.start_ms - now <= lead && !self.calendar.announced.contains(&e.id)).cloned();
        let Some(e) = due else { return };
        self.calendar.announced.push(e.id.clone());
        let mins = (e.start_ms - now).div_ceil(60_000);
        self.alert(PageId::Calendar, format!("{} in {mins} min", e.title), true);
        // After the alert (showing the page resets the pick): lead with this meeting.
        self.calendar.selected = e.id;
        self.redraw();
    }

    /// Called on every loop; return when it next needs waking (timers, animation).
    pub(super) fn calendar_tick(&mut self, now: Instant) -> Option<Instant> {
        self.calendar_announce();
        let lead = self.cfg.calendar.alert_minutes * 60_000;
        if lead == 0 {
            return None;
        }
        let ms = status::now_ms();
        self.calendar
            .events
            .iter()
            .filter(|e| !e.all_day && !e.join_url.is_empty() && !self.calendar.announced.contains(&e.id))
            .map(|e| e.start_ms.saturating_sub(lead))
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
