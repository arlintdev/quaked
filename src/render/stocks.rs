//! Stocks on the board.
//! - Tiles: per symbol the price, today's change, a 5D / 1M / YTD / 1Y strip and today's
//!   chart against yesterday's close.
//! - An opened stock: its numbers on the left and four time scales side by side (1D, 5D,
//!   1M, 1Y), each with its change, price axis, high / low and dates.
//! - One scale expanded: a big chart, and the knob walks 1D 5D 1M 6M YTD 1Y 5Y.
//!
//! Pre-market and after hours: a pill with the session, the extended price and its move
//! from the close, and the 1D chart carries on past the close in a dimmer line.
//!
//! Level::Detail("NVDA") is the four panes (v.scroll = the pane picked);
//! Level::Detail("NVDA@1Y") is one expanded.
use super::*;
use crate::stocks::{Quote, Range, Series, Session};

/// Tiles on screen at once; the knob pages through more.
const PER_SCREEN: usize = 8;
/// Tap indexes past the tiles: a pane, a range pill, Back.
pub const TAP_PANE: usize = 900;
pub const TAP_RANGE: usize = 1000;
pub const TAP_BACK: usize = 1100;
const LEFT_W: f32 = 430.0;
const AXIS_W: f32 = 74.0;
const AXIS_H: f32 = 22.0;
/// Pre-market and after hours.
const EXT: u32 = 0x8AB4F8;
/// The chart line outside the regular session.
const EXT_ALPHA: f32 = 0.45;

/// The pill beside the symbol when the regular session isn't on.
fn session_pill(q: &Quote) -> Option<(&'static str, u32, u32)> {
    match q.session {
        Session::Regular => None,
        Session::Pre => Some(("PRE-MARKET", 0x1A2333, EXT)),
        Session::Post => Some(("AFTER HOURS", 0x1A2333, EXT)),
        Session::Closed => Some(("CLOSED", 0x2A2A31, MUTED)),
    }
}

/// "After hours" / "Pre-market", for the extended price.
fn ext_label(pre: bool) -> &'static str {
    if pre { "Pre-market" } else { "After hours" }
}

/// Draws the extended price on one line from `x` (label, price, move); returns where it ends.
fn ext_line(c: &mut Canvas, q: &Quote, x: f32, y: f32, px: f32) -> f32 {
    let (Some(e), Some((_, p))) = (q.extended, q.extended_change()) else { return x };
    let mut cx = x;
    cx += c.text(F::Bold, ext_label(e.pre), px, cx, y, EXT, Align::Left) + px * 0.6;
    cx += c.text(F::Bold, &price(e.price), px, cx, y, TEXT_2, Align::Left) + px * 0.5;
    cx += c.text(F::Bold, &pct(p), px, cx, y, up_color(p), Align::Left);
    cx
}

/// Draws the line through `pts` (one per point): outside the regular session dimmer, after
/// the close colored by the move from the close, before the open by the move from
/// yesterday's close. Returns the color it ends in.
fn session_line(c: &mut Canvas, q: Option<&Quote>, points: &[(u64, f32)], pts: &[(f32, f32)], color: u32, width: f32) -> u32 {
    let regular = |i: usize| q.is_none_or(|q| q.in_regular(points[i].0));
    let ext_color = q.and_then(Quote::extended_change).map_or(color, |(chg, _)| up_color(chg));
    // Today's pre-market, once the regular session has started (before, it's the extended price).
    let opened = (0..points.len()).any(regular);
    let pre_color = match (q, points.iter().rev().find(|p| q.is_some_and(|q| p.0 < q.regular.0))) {
        (Some(q), Some(last)) if opened && q.prev_close > 0.0 => up_color(last.1 as f64 - q.prev_close),
        _ => ext_color,
    };
    let mut a = 0;
    while a + 1 < pts.len() {
        // A run of segments on the same side of the open / close.
        let on = regular(a) && regular(a + 1);
        let mut b = a + 1;
        while b + 1 < pts.len() && (regular(b) && regular(b + 1)) == on {
            b += 1;
        }
        if on {
            c.polyline(&pts[a..=b], color, width);
        } else {
            let before_open = q.is_some_and(|q| points[b].0 <= q.regular.0);
            c.polyline_alpha(&pts[a..=b], if before_open { pre_color } else { ext_color }, width, EXT_ALPHA + 0.15);
        }
        a = b;
    }
    if pts.len() >= 2 && !regular(pts.len() - 1) { ext_color } else { color }
}

/// Shades between the line and `by` over the regular session only.
fn session_fill(c: &mut Canvas, q: Option<&Quote>, points: &[(u64, f32)], pts: &[(f32, f32)], by: f32, color: u32) {
    let regular = |i: usize| q.is_none_or(|q| q.in_regular(points[i].0));
    let (Some(a), Some(b)) = ((0..pts.len()).find(|&i| regular(i)), (0..pts.len()).rev().find(|&i| regular(i))) else { return };
    if b <= a {
        return;
    }
    let mut area = pts[a..=b].to_vec();
    area.push((pts[b].0, by));
    area.push((pts[a].0, by));
    c.poly_alpha(&area, color, 0.14);
}

fn up_color(change: f64) -> u32 {
    if change >= 0.0 { GREEN } else { RED }
}

/// "6,812.40", "0.4521" (small prices keep more digits).
pub fn price(p: f64) -> String {
    grouped(p, if p.abs() < 1.0 { 4 } else { 2 })
}

/// `p` with `decimals` places and thousands separators.
fn grouped(p: f64, decimals: usize) -> String {
    let s = format!("{:.*}", decimals, p.abs());
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    let mut out = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    format!("{}{out}{}{frac}", if p < 0.0 { "-" } else { "" }, if frac.is_empty() { "" } else { "." })
}

/// Axis labels: fewer digits for big numbers ("64,317", "155.46", "0.4521").
fn axis_price(p: f64) -> String {
    if p.abs() >= 10_000.0 { grouped(p, 0) } else { price(p) }
}

fn pct(p: f64) -> String {
    format!("{:+.2}%", p)
}

fn change_text(q: &Quote) -> String {
    format!("{}{}  {}", if q.change() >= 0.0 { "+" } else { "-" }, grouped(q.change().abs(), 2), pct(q.change_pct()))
}

fn volume(v: u64) -> String {
    match v {
        0 => "—".into(),
        1..=999_999 => grouped(v as f64, 0),
        1_000_000..=999_999_999 => format!("{:.1}M", v as f64 / 1e6),
        _ => format!("{:.2}B", v as f64 / 1e9),
    }
}

/// A time on a chart's axis, as the range wants it: "10:30", "Mon", "Nov 3", "Mar '26", "2023".
fn when(ts: u64, range: Range) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let tm = crate::calendar::local(ts * 1000);
    let month = MONTHS[tm.tm_mon.clamp(0, 11) as usize];
    match range {
        Range::Day => crate::calendar::hm(ts * 1000),
        Range::Week => DAYS[tm.tm_wday.clamp(0, 6) as usize].to_string(),
        Range::Month | Range::SixMonth | Range::Ytd => format!("{month} {}", tm.tm_mday),
        Range::Year => format!("{month} '{:02}", (tm.tm_year + 1900) % 100),
        Range::FiveYear => format!("{}", tm.tm_year + 1900),
    }
}

/// The grid for `n` tiles: (rows, cols) on one screen.
fn grid(n: usize) -> (usize, usize) {
    match n {
        0..=4 => (1, n.max(1)),
        _ => (2, n.min(PER_SCREEN).div_ceil(2)),
    }
}

/// The first tile on the screen holding `tile`.
fn first_on_screen(tile: usize) -> usize {
    tile / PER_SCREEN * PER_SCREEN
}

fn tile_rect(slot: usize, rows: usize, cols: usize) -> R {
    let w = (W - 2.0 * M - (cols as f32 - 1.0) * G) / cols as f32;
    let h = (CH - (rows as f32 - 1.0) * G) / rows as f32;
    let (r, c) = (slot / cols, slot % cols);
    (M + c as f32 * (w + G), CY + r as f32 * (h + G), w, h)
}

/// The opened stock: its symbol, and the expanded range if one is.
pub fn opened(level: &Level) -> Option<(&str, Option<Range>)> {
    let Level::Detail(d) = level else { return None };
    Some(match d.split_once('@') {
        Some((sym, r)) => (sym, Range::from_label(r)),
        None => (d.as_str(), None),
    })
}

/// Today's chart on a tile: scaled to its own range, the area to yesterday's close shaded.
fn spark(c: &mut Canvas, r: R, points: &[(u64, f32)], base: f64, color: u32, sessions: Option<&Quote>) {
    let (x, y, w, h) = r;
    if points.len() < 2 {
        c.rect((x, y + h / 2.0, w, 1.0), LINE, 1.0);
        return;
    }
    let base = base as f32;
    let lo = points.iter().map(|p| p.1).fold(if base > 0.0 { base } else { f32::MAX }, f32::min);
    let hi = points.iter().map(|p| p.1).fold(if base > 0.0 { base } else { f32::MIN }, f32::max);
    let span = (hi - lo).max(hi.abs() * 1e-4).max(1e-6);
    let to_y = |v: f32| y + h - (v - lo) / span * h;
    let n = points.len();
    let pts: Vec<(f32, f32)> = points.iter().enumerate().map(|(i, p)| (x + w * i as f32 / (n - 1) as f32, to_y(p.1))).collect();
    if base > 0.0 {
        dashed(c, x, x + w, to_y(base));
        session_fill(c, sessions, points, &pts, to_y(base), color);
    }
    let end = session_line(c, sessions, points, &pts, color, 2.0);
    let (lx, ly) = pts[n - 1];
    c.circle(lx, ly, 3.2, end, 1.0);
}

fn dashed(c: &mut Canvas, x0: f32, x1: f32, y: f32) {
    let mut dx = x0;
    while dx < x1 {
        c.rect((dx, y, 6.0f32.min(x1 - dx), 1.0), MUTED, 0.7);
        dx += 12.0;
    }
}

/// A chart with axes: prices on the right (high, low, and between them when `rich`), times
/// along the bottom, a dashed line where the range started, the high and low marked.
fn plot(c: &mut Canvas, r: R, points: &Series, base: f64, range: Range, color: u32, rich: bool, sessions: Option<&Quote>) {
    let (x, y, w, h) = r;
    let (px, py, pw, ph) = (x, y + 6.0, w - AXIS_W, h - AXIS_H - 10.0);
    if points.len() < 2 {
        c.text(F::Medium, "No chart yet", 15.0, x + w / 2.0, y + h / 2.0, FAINT, Align::Center);
        return;
    }
    let base = base as f32;
    let (mut lo, mut hi) = points.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
    let (lo_i, hi_i) = (points.iter().position(|p| p.1 == lo).unwrap_or(0), points.iter().position(|p| p.1 == hi).unwrap_or(0));
    if base > 0.0 {
        lo = lo.min(base);
        hi = hi.max(base);
    }
    // Room above the high and below the low for their labels.
    let span = (hi - lo).max(hi.abs() * 1e-4).max(1e-6);
    let (top, bottom) = (20.0, 20.0);
    let to_y = |v: f32| py + top + (ph - top - bottom) * (1.0 - (v - lo) / span);
    let (lo_s, hi_s) = (lo - span * bottom / (ph - top - bottom), hi + span * top / (ph - top - bottom));
    let n = points.len();
    let to_x = |i: usize| px + pw * i as f32 / (n - 1) as f32;
    // Gridlines with prices (a label that would sit under the price badge is left out).
    let badge_y = to_y(points[points.len() - 1].1);
    let lines = if rich { 5 } else { 3 };
    for k in 0..lines {
        let v = lo_s + (hi_s - lo_s) * (k as f32 + 0.5) / lines as f32;
        let gy = to_y(v);
        c.rect((px, gy, pw, 1.0), LINE, 0.55);
        if (gy - badge_y).abs() > 16.0 {
            c.text(F::Mono, &axis_price(v as f64), 12.0, x + w - 4.0, gy + 4.0, FAINT, Align::Right);
        }
    }
    // Times along the bottom.
    let ticks = if rich { 5 } else { 3 };
    for k in 0..ticks {
        let i = ((n - 1) as f32 * k as f32 / (ticks - 1) as f32).round() as usize;
        let align = if k == 0 { Align::Left } else if k == ticks - 1 { Align::Right } else { Align::Center };
        c.text(F::Medium, &when(points[i].0, range), 12.0, to_x(i), y + h - 6.0, FAINT, align);
    }
    let pts: Vec<(f32, f32)> = points.iter().enumerate().map(|(i, p)| (to_x(i), to_y(p.1))).collect();
    if base > 0.0 {
        let by = to_y(base);
        dashed(c, px, px + pw, by);
        session_fill(c, sessions, points, &pts, by, color);
    }
    let end = session_line(c, sessions, points, &pts, color, if rich { 2.6 } else { 2.0 });
    // The high and the low.
    for (i, v, above) in [(hi_i, hi, true), (lo_i, lo, false)] {
        let (mx, my) = (to_x(i), to_y(v));
        c.circle(mx, my, 3.0, TEXT_2, 1.0);
        let label = format!("{} {}", if above { "H" } else { "L" }, axis_price(v as f64));
        let lw = c.measure(F::Bold, &label, 12.0);
        let lx = (mx - lw / 2.0).clamp(px + 2.0, px + pw - lw - 2.0);
        // Above the high, below the low, unless that runs into the edge or the dates.
        let (up, down) = (my - 8.0, my + 18.0);
        let ly = if above { if up < y + 12.0 { down } else { up } } else if down > py + ph + 2.0 { up } else { down };
        c.text(F::Bold, &label, 12.0, lx, ly, TEXT_2, Align::Left);
    }
    // Now, and the price at the axis.
    let (lx, ly) = pts[n - 1];
    c.circle(lx, ly, 4.0, end, 1.0);
    let last = axis_price(points[n - 1].1 as f64);
    let tw = c.measure(F::Bold, &last, 12.0) + 10.0;
    c.rrect((x + w - tw - 2.0, ly - 10.0, tw, 20.0), 5.0, end, 1.0);
    c.text(F::Bold, &last, 12.0, x + w - 7.0, ly + 4.0, 0x101013, Align::Right);
}

fn arrow(c: &mut Canvas, up: bool, x: f32, cy: f32, px: f32, color: u32) {
    c.symbol(if up { "arrowtriangle.up.fill" } else { "arrowtriangle.down.fill" }, px, x, cy, color);
}

pub(super) fn draw(c: &mut Canvas, v: &View) {
    let s = v.stocks;
    if let Some((sym, expanded)) = opened(v.level) {
        if let Some(q) = s.quotes.iter().find(|q| q.symbol == sym) {
            return detail(c, v, q, expanded);
        }
    }
    if s.off || s.quotes.is_empty() {
        let r = (M, CY, W - 2.0 * M, CH);
        c.rrect(r, 14.0, PANEL_DEEP, 1.0);
        c.symbol("chart.line.uptrend.xyaxis", 40.0, W / 2.0, CY + CH / 2.0 - 34.0, FAINT);
        let (title, sub) = if s.off {
            ("Stocks are off", "Turn them on in Quaked Settings (Extras › Stocks).")
        } else if !s.loaded {
            ("Getting prices…", "From Yahoo Finance.")
        } else {
            ("No stocks yet", "Add symbols in Quaked Settings (Extras › Stocks): AAPL, SPY, ^GSPC, BTC-USD…")
        };
        c.text(F::Bold, title, 26.0, W / 2.0, CY + CH / 2.0 + 24.0, TEXT_2, Align::Center);
        c.text(F::Regular, sub, 18.0, W / 2.0, CY + CH / 2.0 + 56.0, FAINT, Align::Center);
        return;
    }
    let first = first_on_screen(v.tile);
    let shown = &s.quotes[first..(first + PER_SCREEN).min(s.quotes.len())];
    let (rows, cols) = grid(shown.len());
    let focus = v.level != &Level::Top;
    for (slot, q) in shown.iter().enumerate() {
        let r = tile_rect(slot, rows, cols);
        tile(c, v, q, r, rows == 1);
        if focus && v.tile == first + slot {
            c.outline(r, 14.0, TEXT, 3.0);
        }
    }
    if s.quotes.len() > PER_SCREEN {
        let pages = s.quotes.len().div_ceil(PER_SCREEN);
        let label = format!("{} / {pages}", first / PER_SCREEN + 1);
        let w = c.measure(F::Bold, &label, 13.0) + 13.0 * 1.4;
        c.pill(W - M - w - 10.0, CY + CH - 34.0, &label, 13.0, 0x2A2A31, TEXT_2);
    }
}

/// One of a card's four mini charts: its scale, its change, and the line against where the
/// scale started.
fn mini(c: &mut Canvas, v: &View, q: &Quote, range: Range, r: R) {
    let (x, y, w, h) = r;
    c.rrect(r, 10.0, 0x18181C, 1.0);
    let now = v.now_ms / 1000;
    let (points, base) = q.over(range, &v.stocks.extra, now);
    let moved = q.change_over(range, &v.stocks.extra, now);
    let col = moved.map_or(MUTED, up_color);
    c.text(F::Bold, range.label(), 12.0, x + 10.0, y + 18.0, FAINT, Align::Left);
    if let Some(p) = moved {
        c.text(F::Bold, &format!("{:+.1}%", p), 13.0, x + w - 10.0, y + 18.0, col, Align::Right);
    }
    if h > 40.0 {
        spark(c, (x + 10.0, y + 28.0, w - 20.0, h - 38.0), &points, base, col, (range == Range::Day).then_some(q));
    }
}

/// A card: symbol and name, price and today's change, (with room) the day and 52-week
/// ranges and volume, then 1D / 5D / 1M / 1Y side by side, or 2 x 2 on a narrow card.
fn tile(c: &mut Canvas, v: &View, q: &Quote, r: R, tall: bool) {
    let (x, y, w, h) = r;
    c.rrect(r, 14.0, PANEL, 1.0);
    let ix = x + 20.0;
    let iw = w - 40.0;
    if let Some(e) = &q.error {
        c.fit(F::Bold, &q.symbol, 24.0, iw, ix, y + 38.0, TEXT_2, Align::Left);
        c.symbol("exclamationmark.triangle", 16.0, ix + 8.0, y + 66.0, AMBER);
        c.clip(F::Regular, e, 15.0, iw - 26.0, ix + 24.0, y + 72.0, MUTED);
        return;
    }
    let col = up_color(q.change());
    c.rrect((x, y, 5.0, h), 2.5, col, 1.0);
    // Price and today's change on the right; symbol and name on the left get the rest.
    let (price_px, change_px) = if tall { (34.0, 17.0) } else { (24.0, 15.0) };
    let pw = c.text(F::Bold, &price(q.price), price_px, x + w - 20.0, y + 40.0, TEXT, Align::Right);
    let change = change_text(q);
    let cw = c.text(F::Bold, &change, change_px, x + w - 20.0, y + 64.0, col, Align::Right);
    arrow(c, q.change() >= 0.0, x + w - 20.0 - cw - 12.0, y + 64.0 - change_px * 0.36, change_px * 0.75, col);
    let left_w = (iw - pw.max(cw + 22.0) - 16.0).max(60.0);
    let sw = c.fit(F::Bold, &q.symbol, 22.0, left_w, ix, y + 36.0, TEXT, Align::Left);
    if let Some((label, bg, fg)) = session_pill(q) {
        if sw + 10.0 + c.measure(F::Bold, label, 11.0) + 16.0 < left_w {
            c.pill(ix + sw + 10.0, y + 18.0, label, 11.0, bg, fg);
        }
    }
    // A short card puts the extended price where the name goes.
    if !tall && q.extended.is_some() {
        ext_line(c, q, ix, y + 60.0, 13.0);
    } else {
        c.clip(F::Medium, &q.name, 14.0, left_w, ix, y + 60.0, MUTED);
    }
    let mut top = y + 78.0;
    if tall {
        let mut parts: Vec<String> = Vec::new();
        if q.day_low > 0.0 && q.day_high > 0.0 {
            parts.push(format!("Day {} – {}", price(q.day_low), price(q.day_high)));
        }
        if q.low_52w > 0.0 && q.high_52w > 0.0 {
            parts.push(format!("52w {} – {}", price(q.low_52w), price(q.high_52w)));
        }
        if q.volume > 0 {
            parts.push(format!("Vol {}", volume(q.volume)));
        }
        // The extended price leads the line.
        let mut sx = ix;
        if q.extended.is_some() {
            sx = ext_line(c, q, ix, y + 92.0, 13.0);
            if !parts.is_empty() {
                sx += c.text(F::Medium, "   ·   ", 13.0, sx, y + 92.0, FAINT, Align::Left);
            }
        }
        c.clip(F::Medium, &parts.join("   ·   "), 13.0, (ix + iw - sx).max(0.0), sx, y + 92.0, FAINT);
        top = y + 106.0;
    }
    // The four scales.
    let area = (ix - 6.0, top, iw + 12.0, y + h - 10.0 - top);
    let cols = if area.2 / area.3 > 2.4 { 4 } else { 2 };
    let rows = 4 / cols;
    let mw = (area.2 - (cols as f32 - 1.0) * 8.0) / cols as f32;
    let mh = (area.3 - (rows as f32 - 1.0) * 8.0) / rows as f32;
    for (k, range) in Range::PANES.into_iter().enumerate() {
        let (row, colk) = (k / cols, k % cols);
        mini(c, v, q, range, (area.0 + colk as f32 * (mw + 8.0), area.1 + row as f32 * (mh + 8.0), mw, mh));
    }
}

// ---------------------------------------------------------------- an opened stock

fn chart_area() -> R {
    (M + LEFT_W + G, CY, W - 2.0 * M - LEFT_W - G, CH)
}

fn pane_rect(k: usize) -> R {
    let (x, y, w, h) = chart_area();
    let pw = (w - G) / 2.0;
    let ph = (h - G) / 2.0;
    (x + (k % 2) as f32 * (pw + G), y + (k / 2) as f32 * (ph + G), pw, ph)
}

fn range_rect(k: usize) -> R {
    let (x, y, _, _) = chart_area();
    (x + 20.0 + k as f32 * 74.0, y + 14.0, 66.0, 34.0)
}

fn back_rect() -> R {
    (M + 24.0, CY + CH - 56.0, 110.0, 40.0)
}

fn detail(c: &mut Canvas, v: &View, q: &Quote, expanded: Option<Range>) {
    let now = v.now_ms / 1000;
    let lr = (M, CY, LEFT_W, CH);
    let (x, y, _, h) = lr;
    c.rrect(lr, 14.0, PANEL, 1.0);
    let col = up_color(q.change());
    c.rrect((x, y, 6.0, h), 3.0, col, 1.0);
    let ix = x + 26.0;
    let iw = LEFT_W - 50.0;
    let sw = c.fit(F::Bold, &q.symbol, 28.0, iw - 90.0, ix, y + 42.0, TEXT, Align::Left);
    if let Some((label, bg, fg)) = session_pill(q) {
        c.pill(ix + sw + 12.0, y + 22.0, label, 11.0, bg, fg);
    }
    let sub = if q.exchange.is_empty() { q.name.clone() } else { format!("{}  ·  {}", q.name, q.exchange) };
    c.clip(F::Medium, &sub, 15.0, iw, ix, y + 66.0, MUTED);
    c.fit(F::Bold, &price(q.price), 46.0, iw, ix, y + 124.0, TEXT, Align::Left);
    arrow(c, q.change() >= 0.0, ix + 8.0, y + 148.0, 14.0, col);
    c.fit(F::Bold, &format!("{}  today", change_text(q)), 18.0, iw - 24.0, ix + 24.0, y + 154.0, col, Align::Left);
    // The extended price and its move from the close.
    let mut rows_y = y + 194.0;
    if let (Some(e), Some((chg, p))) = (q.extended, q.extended_change()) {
        let ecol = up_color(chg);
        let lw = c.text(F::Bold, &price(e.price), 18.0, ix, y + 186.0, TEXT, Align::Left);
        arrow(c, chg >= 0.0, ix + lw + 18.0, y + 180.0, 12.0, ecol);
        let mw = c.text(F::Bold, &format!("{}{}  {}", if chg >= 0.0 { "+" } else { "-" }, grouped(chg.abs(), 2), pct(p)), 16.0, ix + lw + 30.0, y + 186.0, ecol, Align::Left);
        c.clip(F::Bold, &ext_label(e.pre).to_lowercase(), 16.0, (iw - lw - mw - 40.0).max(0.0), ix + lw + mw + 40.0, y + 186.0, EXT);
        rows_y += 26.0;
    }
    let range_of = |lo: f64, hi: f64| if lo > 0.0 && hi > 0.0 { format!("{} – {}", price(lo), price(hi)) } else { "—".into() };
    let when_text = if q.market_time > 0 { crate::calendar::hm_ampm(q.market_time * 1000) } else { String::new() };
    let rows = [
        ("Day range", range_of(q.day_low, q.day_high)),
        ("52-week range", range_of(q.low_52w, q.high_52w)),
        ("Previous close", if q.prev_close > 0.0 { price(q.prev_close) } else { "—".into() }),
        ("Volume", volume(q.volume)),
        (
            "Market",
            match q.session {
                Session::Regular => format!("open  ·  {when_text}"),
                Session::Pre => "pre-market".into(),
                Session::Post => "after hours".into(),
                Session::Closed => format!("closed  ·  as of {when_text}"),
            },
        ),
    ];
    for (k, (label, value)) in rows.iter().enumerate() {
        let ry = rows_y + k as f32 * 28.0;
        c.text(F::Medium, label, 14.0, ix, ry, MUTED, Align::Left);
        c.text(F::Medium, value, 14.0, ix + iw, ry, TEXT_2, Align::Right);
    }
    let br = back_rect();
    c.rrect(br, 10.0, 0x26262D, 1.0);
    c.text(F::Medium, "‹  Back", 16.0, br.0 + br.2 / 2.0, br.1 + 26.0, TEXT, Align::Center);

    let loading = |r: Range| v.stocks.extra.iter().any(|ch| ch.symbol == q.symbol && ch.range == r && ch.loading);
    match expanded {
        None => {
            // Four time scales side by side; the knob picks one to expand.
            for (k, range) in Range::PANES.into_iter().enumerate() {
                let r = pane_rect(k);
                let (px, py, pw, ph) = r;
                c.rrect(r, 14.0, PANEL_DEEP, 1.0);
                let (points, base) = q.over(range, &v.stocks.extra, now);
                let moved = q.change_over(range, &v.stocks.extra, now);
                let mcol = moved.map_or(MUTED, up_color);
                c.text(F::Bold, range.label(), 15.0, px + 16.0, py + 26.0, TEXT, Align::Left);
                let note = if loading(range) { "loading…".to_string() } else { moved.map(pct).unwrap_or_default() };
                c.text(F::Bold, &note, 15.0, px + pw - 16.0, py + 26.0, if loading(range) { MUTED } else { mcol }, Align::Right);
                plot(c, (px + 14.0, py + 36.0, pw - 22.0, ph - 44.0), &points, base, range, mcol, false, (range == Range::Day).then_some(q));
                if v.scroll == k && v.level != &Level::Top {
                    c.outline(r, 14.0, TEXT, 2.0);
                }
            }
        }
        Some(range) => {
            let area = chart_area();
            let (ax, ay, aw, ah) = area;
            c.rrect(area, 14.0, PANEL_DEEP, 1.0);
            for (k, r) in Range::ALL.iter().enumerate() {
                let rr = range_rect(k);
                let on = *r == range;
                c.rrect(rr, 9.0, if on { TEXT } else { 0x26262D }, 1.0);
                c.text(F::Bold, r.label(), 15.0, rr.0 + rr.2 / 2.0, rr.1 + 23.0, if on { 0x111114 } else { TEXT_2 }, Align::Center);
            }
            let (points, base) = q.over(range, &v.stocks.extra, now);
            let moved = q.change_over(range, &v.stocks.extra, now);
            let mcol = moved.map_or(MUTED, up_color);
            let note = if loading(range) {
                "loading…".to_string()
            } else {
                match (moved, points.first()) {
                    (Some(p), Some(_)) if range == Range::Day => format!("{}  since yesterday's close", pct(p)),
                    (Some(p), Some(first)) => format!("{}  since {}", pct(p), when(first.0, if range == Range::FiveYear { Range::Year } else { Range::Month })),
                    _ => String::new(),
                }
            };
            c.text(F::Bold, &note, 16.0, ax + aw - 20.0, ay + 37.0, if loading(range) { MUTED } else { mcol }, Align::Right);
            plot(c, (ax + 20.0, ay + 60.0, aw - 30.0, ah - 66.0), &points, base, range, mcol, true, (range == Range::Day).then_some(q));
        }
    }
}

/// Touch: a tile (its index), a pane (TAP_PANE + k), a range pill (TAP_RANGE + k), Back.
pub(super) fn hit(v: &View, x: f32, y: f32) -> Option<Hit> {
    if let Some((_, expanded)) = opened(v.level) {
        if inside(back_rect(), x, y) {
            return Some(Hit::Feature(Page::Stocks, TAP_BACK));
        }
        return match expanded {
            None => (0..Range::PANES.len()).find(|&k| inside(pane_rect(k), x, y)).map(|k| Hit::Feature(Page::Stocks, TAP_PANE + k)),
            Some(_) => (0..Range::ALL.len()).find(|&k| inside(range_rect(k), x, y)).map(|k| Hit::Feature(Page::Stocks, TAP_RANGE + k)),
        };
    }
    let first = first_on_screen(v.tile);
    let n = v.stocks.quotes.len().saturating_sub(first).min(PER_SCREEN);
    let (rows, cols) = grid(n);
    (0..n).find(|&slot| inside(tile_rect(slot, rows, cols), x, y)).map(|slot| Hit::Feature(Page::Stocks, first + slot))
}

/// Where Hit::Feature(_, i) is, for the tap flash.
pub(super) fn rect(v: &View, i: usize) -> Option<R> {
    match i {
        TAP_BACK => Some(back_rect()),
        i if i >= TAP_RANGE => Some(range_rect(i - TAP_RANGE)),
        i if i >= TAP_PANE => Some(pane_rect(i - TAP_PANE)),
        _ => {
            let first = first_on_screen(v.tile);
            let n = v.stocks.quotes.len().saturating_sub(first).min(PER_SCREEN);
            let (rows, cols) = grid(n);
            i.checked_sub(first).filter(|&s| s < n).map(|slot| tile_rect(slot, rows, cols))
        }
    }
}

pub(super) fn hints(level: &Level) -> &'static str {
    match opened(level) {
        Some((_, None)) => "turn  pick a scale  ·  press  expand  ·  double-click  back",
        Some((_, Some(_))) => "turn  1D … 5Y  ·  press  Yahoo Finance  ·  double-click  back",
        None if level == &Level::Top => "press  pick a stock",
        None => "turn  choose  ·  press  open",
    }
}

pub(super) fn crumb(v: &View) -> Option<String> {
    match opened(v.level) {
        Some((sym, Some(r))) => Some(format!("›  {sym}  ·  {}", r.label())),
        Some((sym, None)) => Some(format!("›  {sym}")),
        None if v.level == &Level::Focus => v.stocks.quotes.get(v.tile).map(|q| format!("›  {}", q.symbol)),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_read_like_prices() {
        assert_eq!(price(6812.4), "6,812.40");
        assert_eq!(price(64484.17), "64,484.17");
        assert_eq!(price(0.45213), "0.4521");
        assert_eq!(price(-12.5), "-12.50");
        assert_eq!(grouped(1427.832, 2), "1,427.83");
        assert_eq!(axis_price(64317.2), "64,317");
        assert_eq!(volume(345_447_821), "345.4M");
        assert_eq!(volume(2_100_000_000), "2.10B");
    }

    #[test]
    fn levels_name_the_stock_and_scale() {
        assert_eq!(opened(&Level::Detail("NVDA".into())), Some(("NVDA", None)));
        assert_eq!(opened(&Level::Detail("^GSPC@YTD".into())), Some(("^GSPC", Some(Range::Ytd))));
        assert_eq!(opened(&Level::Focus), None);
        assert_eq!(grid(5), (2, 3));
        assert_eq!(first_on_screen(9), 8);
    }
}
