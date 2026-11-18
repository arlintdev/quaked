//! Calendar on the board: a ruler of the next hours with your events on it, a big card
//! for the next (or selected) meeting with its join button, and the list of what's coming.
use super::*;
use crate::calendar::{self as cal, Event};

const HOUR: u64 = 3_600_000;
/// Hours across the ruler (it starts an hour back).
const SPAN: u64 = 10;
const RULER: R = (M, CY, W - 2.0 * M, 136.0);
const RULER_PAD: f32 = 22.0;
const LANES_TOP: f32 = CY + 40.0;
const LANES_H: f32 = 82.0;
const LOW_Y: f32 = CY + 136.0 + G;
const LOW_H: f32 = CY + CH - LOW_Y;
const CARD_W: f32 = 800.0;
const CARD: R = (M, LOW_Y, CARD_W, LOW_H);
const LIST: R = (M + CARD_W + G, LOW_Y, W - 2.0 * M - CARD_W - G, LOW_H);
const LIST_TOP: f32 = LOW_Y + 44.0;
const ROW_H: f32 = 54.0;
const ROWS: usize = 4;
const JOIN_GREEN: u32 = 0x2FA35C;

fn join_button() -> R {
    let (x, y, w, h) = CARD;
    (x + w - 236.0, y + h - 96.0, 212.0, 72.0)
}

/// With more than one Mac to join on: a Join button for each, left to right as the Macs
/// sit: (index into `desk.macs`, where, whether it's the one the knob joins on).
fn join_buttons(v: &View, e: &Event) -> Vec<(usize, R, bool)> {
    if e.join_url.is_empty() || v.desk.macs.len() < 2 {
        return Vec::new();
    }
    let (order, primary) = cal::join_targets(&v.desk.macs, v.desk.sides, e, v.desk.join_on);
    let n = order.len().min(3);
    let bw = if n == 2 { 196.0 } else { 160.0 };
    let (x, y, w, h) = CARD;
    let x0 = x + w - 24.0 - n as f32 * bw - (n - 1) as f32 * 12.0;
    order.into_iter().take(n).enumerate().map(|(k, i)| (i, (x0 + k as f32 * (bw + 12.0), y + h - 96.0, bw, 72.0), i == primary)).collect()
}

fn row_rect(slot: usize) -> R {
    (LIST.0 + 10.0, LIST_TOP + slot as f32 * ROW_H, LIST.2 - 20.0, ROW_H - 4.0)
}

fn color(e: &Event, accent: u32) -> u32 {
    crate::config::hex(&e.color, accent)
}

/// Everything draw / hit / rect agree on.
struct Layout<'a> {
    now: u64,
    up: Vec<&'a Event>,
    sel: Option<usize>,
    /// First upcoming event shown in the list.
    first: usize,
    t0: u64,
    blocks: Vec<Block<'a>>,
}

/// An event on the ruler.
struct Block<'a> {
    r: R,
    e: &'a Event,
    /// Its index in `up`, if it's still to come.
    idx: Option<usize>,
    /// Room for its title: up to the next block in its lane (titles may run past a short block).
    room: f32,
}

fn x_of(t0: u64, ms: u64) -> f32 {
    let (x0, x1) = (RULER.0 + RULER_PAD, RULER.0 + RULER.2 - RULER_PAD);
    let f = (ms as f64 - t0 as f64) / (SPAN * HOUR) as f64;
    x0 + (x1 - x0) * f.clamp(0.0, 1.0) as f32
}

fn layout<'a>(v: &View<'a>) -> Layout<'a> {
    let s: &'a crate::calendar::Status = v.calendar;
    let now = v.now_ms;
    let up = s.upcoming(now);
    let sel = s.selection(now);
    let first = list_window(up.len(), sel.unwrap_or(0), ROWS);
    let t0 = cal::hour_start(now).saturating_sub(HOUR);
    let t1 = t0 + SPAN * HOUR;
    // Lanes: each event goes in the first lane free at its start (three at most).
    let shown: Vec<&Event> = s.events.iter().filter(|e| !e.all_day && e.end_ms > t0 && e.start_ms < t1).collect();
    let mut lane_end: Vec<u64> = Vec::new();
    let mut lanes: Vec<usize> = Vec::new();
    for e in &shown {
        let lane = match lane_end.iter().position(|&end| end <= e.start_ms) {
            Some(l) => l,
            None if lane_end.len() < 3 => {
                lane_end.push(0);
                lane_end.len() - 1
            }
            None => lane_end.iter().enumerate().min_by_key(|(_, &end)| end).map(|(l, _)| l).unwrap_or(0),
        };
        lane_end[lane] = lane_end[lane].max(e.end_ms);
        lanes.push(lane);
    }
    let n = lane_end.len().max(1) as f32;
    let lane_h = (LANES_H - (n - 1.0) * 4.0) / n;
    let right = RULER.0 + RULER.2 - 10.0;
    let blocks = shown
        .iter()
        .zip(&lanes)
        .enumerate()
        .map(|(k, (e, &lane))| {
            let x = x_of(t0, e.start_ms.max(t0));
            let w = (x_of(t0, e.end_ms.min(t1)) - x - 3.0).max(8.0);
            let r = (x, LANES_TOP + lane as f32 * (lane_h + 4.0), w, lane_h);
            let next = shown.iter().zip(&lanes).skip(k + 1).find(|(_, &l)| l == lane).map(|(n, _)| x_of(t0, n.start_ms.max(t0))).unwrap_or(right);
            Block { r, e, idx: up.iter().position(|u| u.id == e.id), room: (next - x - 12.0).max(w) }
        })
        .collect();
    Layout { now, up, sel, first, t0, blocks }
}

fn message(c: &mut Canvas, v: &View, title: &str, detail: &str, detail_color: u32) {
    c.rrect((M, CY, W - 2.0 * M, CH), 16.0, PANEL_DEEP, 1.0);
    c.symbol("calendar", 46.0, W / 2.0, CY + CH / 2.0 - 60.0, v.accent);
    c.text(F::Medium, title, 34.0, W / 2.0, CY + CH / 2.0 + 10.0, TEXT_2, Align::Center);
    c.fit(F::Regular, detail, 20.0, W - 200.0, W / 2.0, CY + CH / 2.0 + 50.0, detail_color, Align::Center);
}

pub(super) fn draw(c: &mut Canvas, v: &View) {
    let s = v.calendar;
    if !s.loaded {
        return message(c, v, "Reading your calendar…", "", FAINT);
    }
    if !s.authorized {
        let why = s.error.as_deref().unwrap_or("Allow Quaked in System Settings › Privacy & Security › Calendars");
        return message(c, v, "Calendar not connected", why, AMBER);
    }
    let l = layout(v);
    ruler(c, v, &l);
    card(c, v, &l);
    list(c, v, &l);
}

fn ruler(c: &mut Canvas, v: &View, l: &Layout) {
    c.rrect(RULER, 16.0, PANEL_DEEP, 1.0);
    let focus = v.level != &Level::Top;
    // Hours, with a faint tick on the half hour.
    for k in 0..=SPAN {
        let t = l.t0 + k * HOUR;
        let x = x_of(l.t0, t);
        c.rect((x, LANES_TOP - 6.0, 1.0, LANES_H + 12.0), LINE, 1.0);
        if k < SPAN {
            let hx = x_of(l.t0, t + HOUR / 2);
            c.rect((hx, LANES_TOP + LANES_H + 2.0, 1.0, 4.0), LINE, 1.0);
            let label = cal::hour_label(t, k == 0);
            c.text(F::Bold, &label, 15.0, x + 8.0, RULER.1 + 27.0, if t + HOUR <= l.now { FAINT } else { MUTED }, Align::Left);
        }
    }
    // Events.
    for b in &l.blocks {
        let (x, y, w, h) = b.r;
        let e = b.e;
        let col = color(e, v.accent);
        let past = e.end_ms <= l.now;
        c.rrect(b.r, 8.0, col, if past { 0.13 } else { 0.30 });
        c.rrect((x, y, 5.0_f32.min(w), h), 2.5, col, if past { 0.45 } else { 1.0 });
        let room = b.room - 14.0;
        if room > 30.0 {
            let fg = if past { MUTED } else { TEXT };
            if h >= 60.0 {
                c.clip(F::Bold, &e.title, 18.0, room, x + 14.0, y + h / 2.0 - 4.0, fg);
                c.clip(F::Medium, &cal::span(e.start_ms, e.end_ms), 14.0, room, x + 14.0, y + h / 2.0 + 18.0, if past { FAINT } else { TEXT_2 });
            } else {
                c.clip(F::Bold, &e.title, 16.0, room, x + 13.0, y + h / 2.0 + 6.0, fg);
            }
        }
        if b.idx.is_some() && b.idx == l.sel {
            c.outline((x - 2.0, y - 2.0, w + 4.0, h + 4.0), 10.0, if focus { TEXT } else { col }, 2.0);
        }
    }
    // Now.
    let nx = x_of(l.t0, l.now);
    c.rect((nx - 1.25, LANES_TOP - 10.0, 2.5, LANES_H + 18.0), v.accent, 1.0);
    c.circle(nx, LANES_TOP - 10.0, 5.5, v.accent, 1.0);
}

fn card(c: &mut Canvas, v: &View, l: &Layout) {
    let (x, y, w, h) = CARD;
    c.rrect(CARD, 16.0, PANEL, 1.0);
    let Some(e) = l.sel.and_then(|i| l.up.get(i).copied()) else {
        c.rrect((x, y, 6.0, h), 3.0, FAINT, 1.0);
        c.text(F::Bold, "FREE", 14.0, x + 32.0, y + 42.0, MUTED, Align::Left);
        c.text(F::Bold, "Nothing else today", 44.0, x + 32.0, y + 104.0, TEXT_2, Align::Left);
        c.text(F::Medium, "No more meetings in the next 24 hours", 20.0, x + 32.0, y + 142.0, MUTED, Align::Left);
        return;
    };
    let now = l.now;
    let col = color(e, v.accent);
    let running = e.start_ms <= now;
    c.rrect((x, y, 6.0, h), 3.0, col, 1.0);
    // NOW / NEXT / LATER, then when and which calendar.
    let tag = if running {
        "NOW"
    } else if l.up.iter().find(|u| u.start_ms > now).is_some_and(|u| u.id == e.id) {
        "NEXT"
    } else {
        "LATER"
    };
    let pw = c.pill(x + 30.0, y + 22.0, tag, 14.0, if running { v.accent } else { col }, 0x101013);
    let mut meta = cal::span(e.start_ms, e.end_ms);
    if !e.calendar.is_empty() {
        meta = format!("{meta}   ·   {}", e.calendar);
    }
    // Then the Mac it came from, pointing its way.
    let mac = v.desk.label(&e.machine);
    let mx = x + 44.0 + pw;
    let room = w - pw - 80.0 - if mac.is_some() { 200.0 } else { 0.0 };
    let mw = c.fit(F::Medium, &meta, 18.0, room, mx, y + 41.0, TEXT_2, Align::Left);
    if let Some((name, side)) = mac {
        let dx = mx + mw + 14.0;
        let dw = c.text(F::Medium, "·", 18.0, dx, y + 41.0, TEXT_2, Align::Left);
        super::mac_tag(c, &name, side, 18.0, dx + dw + 14.0, y + 41.0, x + w - 30.0 - (dx + dw + 14.0));
    }
    c.fit(F::Bold, &e.title, 46.0, w - 60.0, x + 30.0, y + 106.0, TEXT, Align::Left);
    // Where: the meeting app, and the place unless it's just the link again.
    let place = if e.location.contains("://") || e.location.to_ascii_lowercase().contains("meeting") { String::new() } else { one_line(&e.location) };
    let mut sx = x + 30.0;
    if !e.join_url.is_empty() {
        c.symbol("video.fill", 18.0, sx + 12.0, y + 140.0, MUTED);
        sx += 36.0 + c.text(F::Medium, cal::provider(&e.join_url), 20.0, sx + 36.0, y + 147.0, TEXT_2, Align::Left) + 26.0;
    }
    if !place.is_empty() {
        c.symbol("mappin.and.ellipse", 18.0, sx + 10.0, y + 140.0, MUTED);
        c.clip(F::Medium, &place, 20.0, x + w - sx - 60.0, sx + 32.0, y + 147.0, TEXT_2);
    }
    // When, big.
    let soon = running || e.start_ms - now <= 15 * 60_000;
    let when = cal::when(now, e.start_ms, e.end_ms);
    let when = when.trim_start_matches("now · ");
    let several = join_buttons(v, e);
    let jb = several.first().map(|b| b.1).unwrap_or_else(join_button);
    c.fit(F::Bold, when, 54.0, jb.0 - x - 50.0, x + 30.0, y + h - 38.0, if soon { v.accent } else { TEXT }, Align::Left);
    if running {
        let frac = (now - e.start_ms) as f32 / (e.end_ms - e.start_ms).max(1) as f32;
        bar(c, x + 30.0, y + h - 22.0, jb.0 - x - 60.0, frac, v.accent);
    }
    // Join on each Mac, or the one Join / Open.
    if !several.is_empty() {
        for (i, r, primary) in &several {
            let name = v.desk.macs.get(*i).map(String::as_str).unwrap_or("");
            join_on(c, *r, name, v.desk.side(if *i == 0 { "" } else { name }), *primary);
            if *primary && v.level != &Level::Top {
                c.outline(*r, 14.0, TEXT, 2.0);
            }
        }
        return;
    }
    if e.join_url.is_empty() {
        button(c, jb, "Open", 0x2A2A31, TEXT, "calendar");
    } else {
        button(c, jb, "Join", JOIN_GREEN, 0xFFFFFF, "video.fill");
    }
    if v.level != &Level::Top {
        c.outline(jb, 14.0, TEXT, 2.0);
    }
}

/// "Join" on one Mac: its arrow on the side it sits (the camera when it has no side), its
/// name underneath. The knob's pick is filled green.
fn join_on(c: &mut Canvas, r: R, name: &str, side: Option<Side>, primary: bool) {
    let (bg, fg, sub) = if primary { (JOIN_GREEN, 0xFFFFFF, 0xDFF5E7) } else { (0x1D2A22, 0x7FD9A0, MUTED) };
    c.rrect(r, 14.0, bg, 1.0);
    let (x, y, w, h) = r;
    let mid = y + h / 2.0;
    let (icon_x, tx0, tx1) = match side {
        Some(Side::Right) => (x + w - 26.0, x + 14.0, x + w - 46.0),
        _ => (x + 26.0, x + 46.0, x + w - 14.0),
    };
    match side {
        Some(s) => super::side_arrow(c, s, icon_x, mid, 24.0, fg),
        None => {
            c.symbol("video.fill", 20.0, icon_x, mid, fg);
        }
    }
    let cx = (tx0 + tx1) / 2.0;
    c.text(F::Bold, "Join", 22.0, cx, y + 34.0, fg, Align::Center);
    let label = if name.is_empty() { "this Mac" } else { name };
    let nw = c.measure(F::Medium, label, 13.0).min(tx1 - tx0);
    c.fit(F::Medium, label, 13.0, tx1 - tx0, cx - nw / 2.0, y + 56.0, sub, Align::Left);
}

fn list(c: &mut Canvas, v: &View, l: &Layout) {
    let s = v.calendar;
    let (x, y, w, h) = LIST;
    c.rrect(LIST, 16.0, PANEL_DEEP, 1.0);
    let focus = v.level != &Level::Top;
    let n = l.up.len();
    let title = format!("COMING UP   ·   {}", cal::day_label(l.now).to_uppercase());
    let tw = c.text(F::Bold, &title, 12.0, x + 22.0, y + 27.0, FAINT, Align::Left);
    // Right of the header: problems, else all-day events, then the count.
    let mut rx = x + w - 22.0;
    if n > ROWS {
        rx -= c.text(F::Bold, &format!("{}–{} of {n}", l.first + 1, (l.first + ROWS).min(n)), 12.0, rx, y + 27.0, FAINT, Align::Right) + 18.0;
    }
    if let Some(err) = s.error.as_ref().or(s.peer_note.as_ref()) {
        c.fit(F::Medium, err, 14.0, rx - x - tw - 50.0, rx, y + 27.0, AMBER, Align::Right);
    } else {
        let room = rx - (x + 22.0 + tw + 24.0);
        let mut cx = rx;
        for e in s.all_day(l.now) {
            let lw = (c.measure(F::Medium, &e.title, 13.0) + 30.0).min(260.0);
            if cx - lw < rx - room {
                break;
            }
            cx -= lw;
            chip(c, cx, y + 10.0, color(e, v.accent), &e.title, 0x22222A, TEXT_2, 260.0);
            cx -= 8.0;
        }
    }
    if n == 0 {
        c.text(F::Medium, "No more events in the next 24 hours", 22.0, x + w / 2.0, y + h / 2.0 + 14.0, FAINT, Align::Center);
        return;
    }
    let first_future = l.up.iter().position(|e| e.start_ms > l.now);
    for slot in 0..ROWS {
        let i = l.first + slot;
        let Some(e) = l.up.get(i) else { break };
        let r = row_rect(slot);
        let (rx, ry, rw, rh) = r;
        let mid = ry + rh / 2.0;
        let selected = l.sel == Some(i);
        if selected && focus {
            c.rrect(r, 10.0, 0x22222A, 1.0);
            c.outline(r, 10.0, TEXT, 2.0);
        } else if selected {
            c.rrect(r, 10.0, 0x1A1A1F, 1.0);
        } else if slot > 0 && l.sel != Some(i - 1) {
            c.rect((rx + 14.0, ry - 2.0, rw - 28.0, 1.0), LINE, 0.7);
        }
        let col = color(e, v.accent);
        let running = e.start_ms <= l.now;
        let soon = running || Some(i) == first_future && e.start_ms - l.now <= 15 * 60_000;
        c.text(F::Bold, &cal::hm(e.start_ms), 21.0, rx + 86.0, mid + 7.0, if running { v.accent } else { TEXT }, Align::Right);
        c.rrect((rx + 102.0, ry + 10.0, 4.0, rh - 20.0), 2.0, col, 1.0);
        let left = cal::until_short(l.now, e.start_ms, e.end_ms);
        let lw = c.text(F::Bold, &left, 17.0, rx + rw - 16.0, mid + 6.0, if soon { v.accent } else { MUTED }, Align::Right);
        let mut tx_end = rx + rw - 16.0 - lw - 18.0;
        if !e.join_url.is_empty() {
            c.symbol("video.fill", 17.0, tx_end - 10.0, mid, if soon { JOIN_GREEN } else { MUTED });
            tx_end -= 34.0;
        }
        let mut tx = rx + 122.0;
        if let Some(day) = cal::other_day(l.now, e.start_ms) {
            tx += c.text(F::Bold, day, 13.0, tx, mid + 6.0, FAINT, Align::Left) + 12.0;
        }
        c.clip(F::Medium, &e.title, 21.0, tx_end - tx, tx, mid + 7.0, TEXT);
    }
}

/// A multi-line location ("Studio\n2500 Murphy St") on one line; the fonts have no glyph
/// for line breaks.
fn one_line(s: &str) -> String {
    s.split(['\n', '\r', '\u{2028}', '\u{2029}']).map(str::trim).filter(|p| !p.is_empty()).collect::<Vec<_>>().join("  ·  ")
}

/// Touch on this page: return Hit::Feature(Page::<this page>, i) for row / card `i`.
pub(super) fn hit(v: &View, x: f32, y: f32) -> Option<Hit> {
    let s = v.calendar;
    if !s.loaded || !s.authorized {
        return None;
    }
    let l = layout(v);
    if let Some(e) = l.sel.and_then(|i| l.up.get(i)) {
        let several = join_buttons(v, e);
        if let Some((i, _, _)) = several.iter().find(|b| inside(b.1, x, y)) {
            return Some(Hit::Feature(Page::Calendar, cal::TAP_JOIN_ON + i));
        }
        if several.is_empty() && inside(join_button(), x, y) {
            return Some(Hit::Feature(Page::Calendar, cal::TAP_JOIN));
        }
    }
    if let Some(i) = (0..ROWS).find(|&s| inside(row_rect(s), x, y)).map(|s| l.first + s).filter(|&i| i < l.up.len()) {
        return Some(Hit::Feature(Page::Calendar, i));
    }
    l.blocks.iter().rev().find(|b| b.idx.is_some() && inside(b.r, x, y)).and_then(|b| b.idx).map(|i| Hit::Feature(Page::Calendar, i))
}

/// Where Hit::Feature(_, i) is, for the tap flash.
pub(super) fn rect(v: &View, i: usize) -> Option<R> {
    if i == cal::TAP_JOIN {
        return Some(join_button());
    }
    let l = layout(v);
    if i >= cal::TAP_JOIN_ON {
        let e = l.sel.and_then(|s| l.up.get(s))?;
        return join_buttons(v, e).into_iter().find(|b| b.0 == i - cal::TAP_JOIN_ON).map(|b| b.1);
    }
    match i.checked_sub(l.first).filter(|&s| s < ROWS) {
        Some(slot) => Some(row_rect(slot)),
        None => l.blocks.iter().find(|b| b.idx == Some(i)).map(|b| b.r),
    }
}

pub(super) fn hints(level: &Level) -> &'static str {
    match level {
        Level::Top => "turn  switch page   ·   press  choose an event   ·   tap  select",
        _ => "turn  choose   ·   press  join / open   ·   double-click  back",
    }
}

/// The breadcrumb next to the tabs (e.g. "›  the selected item").
pub(super) fn crumb(v: &View) -> Option<String> {
    if v.level == &Level::Top {
        return None;
    }
    let now = v.now_ms;
    let i = v.calendar.selection(now)?;
    v.calendar.upcoming(now).get(i).map(|e| format!("›  {}", e.title))
}
