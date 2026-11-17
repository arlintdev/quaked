//! Call mode on the board (shown over every page while active). Scaffold: the calendar + call agent fills this in.
#![allow(dead_code, unused_variables, unused_imports)]
use super::*;

pub(super) fn draw(c: &mut Canvas, v: &View) {
    c.text(F::Medium, "Call mode", 30.0, W / 2.0, H / 2.0, FAINT, Align::Center);
}

/// Touch while it's showing: return Hit::Feature(Page::Dash, i) for card / button `i`.
pub(super) fn hit(v: &View, x: f32, y: f32) -> Option<Hit> {
    None
}

/// Where Hit::Feature(_, i) is, for the tap flash.
pub(super) fn rect(v: &View, i: usize) -> Option<R> {
    None
}
