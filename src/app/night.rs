//! Night mode: the app side. Decides when the ambient screen shows (at night, after
//! `idle_minutes` without input, nothing else asking for you), dims the screen and darkens
//! the ring while it does, and puts everything back on any input (which it eats).
//! Runs on the main thread with full access to App (it's a child module of app).
use super::*;
use crate::night::{self as nt, Update as NightUpdate};
use std::cell::RefCell;

/// The ring while the night screen shows.
const RING_OFF: Command = Command::Ring { effect: proto::EFFECT_OFF, hue: 0, sat: 0, brightness: 0, speed: 128 };

/// Bookkeeping for the night screen (main thread only).
#[derive(Default)]
struct Book {
    /// Luminance to go back to on wake, and the one the night screen set.
    restore: Option<u8>,
    dim: u8,
    /// What `ring_sent` was made to look like while the ring is dark (see night_hold_ring).
    ring_held: Option<Command>,
    /// The location last handed to the night thread (outer None: not sent yet).
    sent_loc: Option<Option<nt::Location>>,
    /// The minute the clock last changed.
    minute: i64,
}

thread_local! {
    static BOOK: RefCell<Book> = RefCell::new(Book::default());
}

fn book<T>(f: impl FnOnce(&mut Book) -> T) -> T {
    BOOK.with(|b| f(&mut b.borrow_mut()))
}

impl App {
    /// An update from crate::night's thread.
    pub(super) fn on_night(&mut self, u: NightUpdate) {
        match u {
            NightUpdate::Weather(w) => {
                self.night.weather = w;
                if self.night.active {
                    self.redraw();
                }
            }
        }
    }

    /// The ring this feature wants: dark while the night screen shows. Not consulted by
    /// update_ring() yet, so night_hold_ring() keeps the ring dark in the meantime.
    #[allow(dead_code)]
    pub(super) fn night_ring(&self) -> Option<Command> {
        self.night.active.then_some(RING_OFF)
    }

    /// Called on every loop: hands the weather location to the thread, turns the night
    /// screen on and off, keeps it dark, and moves its clock along.
    pub(super) fn night_tick(&mut self, now: Instant) -> Option<Instant> {
        let loc = nt::location(&self.cfg.night, &self.ops.net.colo);
        let moved = book(|b| {
            let moved = b.sent_loc.as_ref() != Some(&loc);
            if moved {
                b.sent_loc = Some(loc.clone());
            }
            moved
        });
        if moved {
            self.night.place = loc.as_ref().map(|l| l.place.clone()).unwrap_or_default();
            let _ = self.night_cmd.send(nt::Cmd::Location(loc));
        }
        let want = self.night_wanted();
        if want && !self.night.active {
            self.night_on();
        } else if !want && self.night.active {
            self.night_wake("no longer quiet");
        }
        if !self.night.active {
            return None;
        }
        self.night_hold();
        let now_ms = status::now_ms();
        let minute = (now_ms / 60_000) as i64;
        if book(|b| std::mem::replace(&mut b.minute, minute)) != minute {
            self.night_refresh();
            self.redraw();
        }
        Some(now + Duration::from_millis(60_000 - now_ms % 60_000 + 20))
    }

    /// At night, untouched for `idle_minutes`, and nothing else wants the board or the ring.
    fn night_wanted(&self) -> bool {
        let c = &self.cfg.night;
        c.enabled
            && self.connected
            && nt::in_window(nt::local(nt::now_secs()).hour, c.start_hour, c.end_hour)
            && self.last_input.elapsed() >= Duration::from_secs(c.idle_minutes.max(1) * 60)
            && self.asks.is_empty()
            && self.attention.is_none()
            && self.pair_ask.is_none()
            && !self.call_active()
            // A match being accepted, picked or played (the board shows it).
            && !matches!(self.league.phase.as_str(), "ReadyCheck" | "ChampSelect" | "InProgress")
            // A feature driving the ring (focus timer, deploy, visualizer) outranks the dark.
            && self.focus_ring().is_none()
            && self.deploy_ring().is_none()
            && self.viz_ring().is_none()
    }

    fn night_on(&mut self) {
        let lum = self.luminance;
        let dim = self.cfg.night.brightness.max(render::SCREEN_MIN).min(lum);
        book(|b| {
            b.restore = Some(lum);
            b.dim = dim;
            b.ring_held = None;
            b.minute = -1;
        });
        self.night.active = true;
        self.night_refresh();
        if dim != lum {
            self.set_luminance(dim);
        }
        self.night_hold_ring();
        device::log(format!("night: on (screen {lum} -> {dim})"));
        self.redraw();
    }

    /// Back to normal: the screen as it was, the ring as the board wants it.
    fn night_wake(&mut self, why: &str) {
        self.night.active = false;
        let (restore, dim) = book(|b| {
            b.ring_held = None;
            (b.restore.take(), b.dim)
        });
        if let Some(prev) = restore {
            // Unless something else changed it meanwhile (it would have been re-dimmed).
            if self.luminance == dim && prev != dim {
                self.set_luminance(prev);
            }
        }
        // The ring is really dark whatever ring_sent says: make update_ring() send.
        self.ring_sent = None;
        self.update_ring();
        device::log(format!("night: off ({why})"));
        self.redraw();
    }

    /// Keeps it dark while it shows: the screen (a reconnect or a settings change may have
    /// brightened it; that becomes the level to go back to) and the ring.
    fn night_hold(&mut self) {
        let dim = book(|b| b.dim);
        if self.luminance != dim {
            let lum = self.luminance;
            let d = self.cfg.night.brightness.max(render::SCREEN_MIN).min(lum);
            book(|b| {
                b.restore = Some(lum);
                b.dim = d;
            });
            if d != lum {
                self.set_luminance(d);
            }
        }
        self.night_hold_ring();
    }

    /// update_ring() doesn't know about night mode and runs after most events. So the ring
    /// gets "off" once, and `ring_sent` is then set to what update_ring() would send now;
    /// its later calls see nothing to change. If it did send something (what it wants
    /// changed, or the Quake reconnected), the ring goes dark again here.
    fn night_hold_ring(&mut self) {
        if !self.cfg.ring.enabled || !self.connected {
            book(|b| b.ring_held = None);
            return;
        }
        let want = self.ring_want();
        let held = book(|b| b.ring_held.clone());
        if held.is_none() || self.ring_sent != held {
            let _ = self.commands.send(RING_OFF);
        }
        book(|b| b.ring_held = want.clone());
        self.ring_sent = want;
    }

    /// What update_ring() would send right now, without sending it.
    fn ring_want(&mut self) -> Option<Command> {
        let (tx, _rx) = std::sync::mpsc::channel();
        let real = std::mem::replace(&mut self.commands, tx);
        let before = self.ring_sent.take();
        self.update_ring();
        let want = std::mem::replace(&mut self.ring_sent, before);
        self.commands = real;
        want
    }

    /// The clock and the first thing on the calendar for the morning ahead.
    fn night_refresh(&mut self) {
        let now = status::now_ms();
        self.night.time = nt::local((now / 1000) as i64);
        self.night.next = nt::next_event(&self.calendar.events, now);
        self.night.calendar_ok = self.calendar.authorized;
    }

    /// While true the board redraws every second (night_tick redraws once a minute instead).
    pub(super) fn night_animating(&self) -> bool {
        false
    }

    /// Its part of `quaked ctl status`.
    pub(super) fn night_json(&self) -> serde_json::Value {
        let c = &self.cfg.night;
        serde_json::json!({
            "enabled": c.enabled,
            "active": self.night.active,
            "window": format!("{}:00-{}:00", c.start_hour, c.end_hour),
            "idleMinutes": c.idle_minutes,
            "place": self.night.place,
            "weather": self.night.weather.as_ref().map(|w| serde_json::json!({
                "temp": w.temp, "code": w.code, "text": nt::weather_text(w.code), "isDay": w.is_day,
                "days": w.days.iter().map(|d| serde_json::json!({"high": d.high, "low": d.low, "code": d.code})).collect::<Vec<_>>(),
            })),
            "next": self.night.next.as_ref().map(|e| serde_json::json!({"title": e.title, "day": e.day, "when": e.when})),
        })
    }

    /// Input while the night screen shows: wake up and swallow it (return true).
    pub(super) fn night_swallow(&mut self) -> bool {
        if !self.night.active {
            return false;
        }
        self.night_wake("touched");
        true
    }
}
