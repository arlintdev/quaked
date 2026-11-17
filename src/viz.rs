//! Music visualizer. Scaffold: the visualizer agent fills this in.
#![allow(dead_code, unused_variables, unused_imports)]

use crate::config::VizSettings as Cfg;
use std::sync::mpsc::{Receiver, Sender};

/// What the board shows. Add fields as needed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// Spectrum bands, 0.0-1.0, low to high.
    pub bands: Vec<f32>,
    /// Overall loudness / beat energy, 0.0-1.0.
    pub level: f32,
}

#[derive(Debug)]
pub enum Cmd {
    Configure(Cfg),
}

/// What the module's thread sends the app (handled in app/viz.rs on the main thread).
#[derive(Debug)]
pub enum Update {
    Status(Status),
}

/// Starts the module's background work; `emit` hands updates to the app.
pub fn spawn(cfg: Cfg, emit: impl Fn(Update) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("viz".into()).spawn(move || run(cfg, rx, emit)).unwrap();
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
