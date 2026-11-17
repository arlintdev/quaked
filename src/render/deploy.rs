//! Deploy and CI on the board. Scaffold: the deploy agent fills this in.
#![allow(dead_code, unused_variables, unused_imports)]
use super::*;

pub(super) fn draw(c: &mut Canvas, v: &View) {
    c.rrect((M, CY, W - 2.0 * M, CH), 16.0, PANEL_DEEP, 1.0);
    c.text(F::Medium, "Deploy and CI", 30.0, W / 2.0, CY + CH / 2.0, FAINT, Align::Center);
}

/// Touch on this page: return Hit::Feature(Page::<this page>, i) for row / card `i`.
pub(super) fn hit(v: &View, x: f32, y: f32) -> Option<Hit> {
    None
}

/// Where Hit::Feature(_, i) is, for the tap flash.
pub(super) fn rect(v: &View, i: usize) -> Option<R> {
    None
}

pub(super) fn hints(level: &Level) -> &'static str {
    match level {
        Level::Top => "turn  switch page   ·   press  choose",
        _ => "turn  choose   ·   press  select   ·   double-click  back",
    }
}

/// The breadcrumb next to the tabs (e.g. "›  the selected item").
pub(super) fn crumb(v: &View) -> Option<String> {
    None
}
