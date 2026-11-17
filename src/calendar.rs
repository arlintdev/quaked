//! Calendar. Scaffold: the calendar + call agent fills this in.
#![allow(dead_code, unused_variables, unused_imports)]

use crate::config::CalendarSettings as Cfg;
use std::sync::mpsc::{Receiver, Sender};

/// One calendar event. Other features (call mode, night mode) read these fields: keep
/// them, add more as needed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Event {
    pub title: String,
    /// Unix ms
    pub start_ms: u64,
    pub end_ms: u64,
    pub all_day: bool,
    pub location: String,
    /// A Zoom / Meet / Teams / FaceTime link found in the event, if any.
    pub join_url: String,
    pub calendar: String,
    /// "#RRGGBB" of its calendar.
    pub color: String,
}

/// What the board shows. Add fields as needed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// Calendar access granted.
    pub authorized: bool,
    /// Today's and upcoming events, soonest first.
    pub events: Vec<Event>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum Cmd {
    Configure(Cfg),
}

/// What the module's thread sends the app (handled in app/calendar.rs on the main thread).
#[derive(Debug)]
pub enum Update {
    Status(Status),
}

/// Starts the module's background work; `emit` hands updates to the app.
pub fn spawn(cfg: Cfg, emit: impl Fn(Update) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("calendar".into()).spawn(move || run(cfg, rx, emit)).unwrap();
    tx
}

fn run(cfg: Cfg, rx: Receiver<Cmd>, emit: impl Fn(Update)) {
    for cmd in rx {
        match cmd {
            Cmd::Configure(_c) => {}
        }
    }
}

/// Sample data for `quaked preview` (make it look like a real moment).
pub fn sample() -> Status {
    Status::default()
}
