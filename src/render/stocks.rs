//! Stocks on the board: a tile per symbol (price, today's change, today's chart against
//! yesterday's close), and a stock opened big with 1D / 5D / 1M / 1Y charts.
use super::*;
use crate::stocks::{Quote, Range};

/// Tiles on screen at once; the knob pages through more.
const PER_SCREEN: usize = 8;
/// Tap indexes in the opened stock: the range pills, then Back.
pub const TAP_RANGE: usize = 1000;
pub const TAP_BACK: usize = 1100;
const LEFT_W: f32 = 520.0;

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
    let mut grouped = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    format!("{}{grouped}{}{frac}", if p < 0.0 { "-" } else { "" }, if frac.is_empty() { "" } else { "." })
}

fn change_text(q: &Quote) -> String {
    format!("{}{}  {:+.2}%", if q.change() >= 0.0 { "+" } else { "-" }, grouped(q.change().abs(), 2), q.change_pct())
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

/// A price chart scaled to its own range, with a dashed line at `base` (yesterday's close,
/// or where the range started) and the area to it shaded.
fn chart(c: &mut Canvas, r: R, points: &[f32], base: f64, color: u32) {
    let (x, y, w, h) = r;
    if points.len() < 2 {
        c.rect((x, y + h / 2.0, w, 1.0), LINE, 1.0);
        return;
    }
    let base = base as f32;
    let lo = points.iter().cloned().fold(if base > 0.0 { base } else { f32::MAX }, f32::min);
    let hi = points.iter().cloned().fold(if base > 0.0 { base } else { f32::MIN }, f32::max);
    let span = (hi - lo).max(hi.abs() * 1e-4).max(1e-6);
    let to_y = |v: f32| y + h - (v - lo) / span * h;
    let n = points.len();
    let pts: Vec<(f32, f32)> = points.iter().enumerate().map(|(i, v)| (x + w * i as f32 / (n - 1) as f32, to_y(*v))).collect();
    if base > 0.0 {
        let by = to_y(base);
        let mut dx = x;
        while dx < x + w {
            c.rect((dx, by, 6.0f32.min(x + w - dx), 1.0), MUTED, 0.7);
            dx += 12.0;
        }
        let mut area = pts.clone();
        area.push((x + w, by));
        area.push((x, by));
        c.poly_alpha(&area, color, 0.14);
    }
    c.polyline(&pts, color, 2.2);
    let (lx, ly) = pts[n - 1];
    c.circle(lx, ly, 3.5, color, 1.0);
}

fn arrow(c: &mut Canvas, up: bool, x: f32, cy: f32, px: f32, color: u32) {
    c.symbol(if up { "arrowtriangle.up.fill" } else { "arrowtriangle.down.fill" }, px, x, cy, color);
}

pub(super) fn draw(c: &mut Canvas, v: &View) {
    let s = v.stocks;
    if let Level::Detail(sym) = v.level {
        if let Some(q) = s.quotes.iter().find(|q| &q.symbol == sym) {
            return detail(c, v, q);
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

fn tile(c: &mut Canvas, v: &View, q: &Quote, r: R, tall: bool) {
    let (x, y, w, h) = r;
    c.rrect(r, 14.0, PANEL, 1.0);
    let ix = x + 20.0;
    let iw = w - 40.0;
    let symbol = q.symbol.as_str();
    if let Some(e) = &q.error {
        c.fit(F::Bold, symbol, 24.0, iw, ix, y + 38.0, TEXT_2, Align::Left);
        c.symbol("exclamationmark.triangle", 16.0, ix + 8.0, y + 66.0, AMBER);
        c.clip(F::Regular, e, 15.0, iw - 26.0, ix + 24.0, y + 72.0, MUTED);
        return;
    }
    let col = up_color(q.change());
    c.rrect((x, y, 5.0, h), 2.5, col, 1.0);
    let state_w = if q.open { 0.0 } else { c.pill(x + w - 20.0 - (c.measure(F::Bold, "CLOSED", 11.0) + 11.0 * 1.4), y + 18.0, "CLOSED", 11.0, 0x2A2A31, MUTED) + 10.0 };
    c.fit(F::Bold, symbol, 22.0, iw - state_w, ix, y + 36.0, TEXT, Align::Left);
    c.clip(F::Medium, &q.name, 14.0, iw, ix, y + 58.0, MUTED);
    let (py, cy) = if tall { (y + 122.0, y + 154.0) } else { (y + 104.0, y + 130.0) };
    let pw = c.fit(F::Bold, &price(q.price), if tall { 44.0 } else { 34.0 }, iw, ix, py, TEXT, Align::Left);
    if !q.currency.is_empty() && q.currency != "USD" {
        c.text(F::Medium, &q.currency, 14.0, ix + pw + 8.0, py, MUTED, Align::Left);
    }
    arrow(c, q.change() >= 0.0, ix + 7.0, cy - 6.0, 13.0, col);
    c.fit(F::Bold, &change_text(q), 17.0, iw - 20.0, ix + 20.0, cy, col, Align::Left);
    let top = cy + 14.0;
    let bottom = y + h - 16.0;
    if bottom - top > 24.0 {
        chart(c, (ix, top, iw, bottom - top), &q.series, q.prev_close, col);
    }
    let _ = v;
}

fn range_rect(k: usize) -> R {
    let (w, gap) = (78.0, 10.0);
    (M + 24.0 + k as f32 * (w + gap), CY + CH - 62.0, w, 40.0)
}

fn back_rect() -> R {
    (M + LEFT_W - 24.0 - 110.0, CY + CH - 62.0, 110.0, 40.0)
}

fn detail(c: &mut Canvas, v: &View, q: &Quote) {
    let lr = (M, CY, LEFT_W, CH);
    let (x, y, _, h) = lr;
    c.rrect(lr, 14.0, PANEL, 1.0);
    let col = up_color(q.change());
    c.rrect((x, y, 6.0, h), 3.0, col, 1.0);
    let ix = x + 26.0;
    let iw = LEFT_W - 52.0;
    c.fit(F::Bold, &q.symbol, 30.0, iw, ix, y + 44.0, TEXT, Align::Left);
    c.clip(F::Medium, &q.name, 16.0, iw, ix, y + 70.0, MUTED);
    c.fit(F::Bold, &price(q.price), 50.0, iw, ix, y + 134.0, TEXT, Align::Left);
    arrow(c, q.change() >= 0.0, ix + 8.0, y + 160.0, 15.0, col);
    c.fit(F::Bold, &format!("{}  today", change_text(q)), 20.0, iw - 24.0, ix + 24.0, y + 167.0, col, Align::Left);
    let mut rows: Vec<(String, String)> = Vec::new();
    if q.day_low > 0.0 && q.day_high > 0.0 {
        rows.push(("Day range".into(), format!("{} – {}", price(q.day_low), price(q.day_high))));
    }
    if q.prev_close > 0.0 {
        rows.push(("Previous close".into(), price(q.prev_close)));
    }
    let when = if q.market_time > 0 { crate::calendar::hm_ampm(q.market_time * 1000) } else { String::new() };
    rows.push(("Market".into(), if q.open { format!("open  ·  {when}") } else { format!("closed  ·  as of {when}") }));
    for (k, (label, value)) in rows.iter().enumerate() {
        let ry = y + 210.0 + k as f32 * 30.0;
        c.text(F::Medium, label, 15.0, ix, ry, MUTED, Align::Left);
        c.text(F::Medium, value, 15.0, ix + iw, ry, TEXT_2, Align::Right);
    }
    let current = v.stocks.chart.as_ref().filter(|ch| ch.symbol == q.symbol).map_or(Range::Day, |ch| ch.range);
    for (k, r) in Range::ALL.iter().enumerate() {
        let rr = range_rect(k);
        let on = *r == current;
        c.rrect(rr, 10.0, if on { TEXT } else { 0x26262D }, 1.0);
        c.text(F::Bold, r.label(), 16.0, rr.0 + rr.2 / 2.0, rr.1 + 26.0, if on { 0x111114 } else { TEXT_2 }, Align::Center);
    }
    let br = back_rect();
    c.rrect(br, 10.0, 0x26262D, 1.0);
    c.text(F::Medium, "‹  Back", 16.0, br.0 + br.2 / 2.0, br.1 + 26.0, TEXT, Align::Center);

    // The chart: the range that's loaded for this stock, else today's.
    let rr = (M + LEFT_W + G, CY, W - 2.0 * M - LEFT_W - G, CH);
    let (rx, ry, rw, rh) = rr;
    c.rrect(rr, 14.0, PANEL_DEEP, 1.0);
    let ch = v.stocks.chart.as_ref().filter(|ch| ch.symbol == q.symbol && ch.range != Range::Day);
    let (points, base, range): (&[f32], f64, Range) = match ch {
        Some(ch) => (&ch.points, ch.base, ch.range),
        None => (&q.series, q.prev_close, Range::Day),
    };
    let loading = v.stocks.chart.as_ref().is_some_and(|ch| ch.symbol == q.symbol && ch.loading);
    let last = points.last().copied().map(|p| p as f64).unwrap_or(q.price);
    let moved = if range == Range::Day { q.change() } else { last - base };
    let pct = if base > 0.0 { moved / base * 100.0 } else { 0.0 };
    let mcol = up_color(moved);
    c.text(F::Bold, range.label(), 13.0, rx + 24.0, ry + 30.0, FAINT, Align::Left);
    let note = if loading { "loading…".to_string() } else if range == Range::Day { "today, against yesterday's close".to_string() } else { format!("{}{}  {:+.2}%", if moved >= 0.0 { "+" } else { "-" }, grouped(moved.abs(), 2), pct) };
    c.text(F::Bold, &note, 16.0, rx + rw - 24.0, ry + 30.0, if loading { MUTED } else { mcol }, Align::Right);
    chart(c, (rx + 24.0, ry + 56.0, rw - 48.0, rh - 84.0), points, base, mcol);
}

/// Touch: a tile (its index), a range pill (TAP_RANGE + k) or Back (TAP_BACK).
pub(super) fn hit(v: &View, x: f32, y: f32) -> Option<Hit> {
    let s = v.stocks;
    if let Level::Detail(_) = v.level {
        if let Some(k) = (0..Range::ALL.len()).find(|&k| inside(range_rect(k), x, y)) {
            return Some(Hit::Feature(Page::Stocks, TAP_RANGE + k));
        }
        return inside(back_rect(), x, y).then_some(Hit::Feature(Page::Stocks, TAP_BACK));
    }
    let first = first_on_screen(v.tile);
    let n = s.quotes.len().saturating_sub(first).min(PER_SCREEN);
    let (rows, cols) = grid(n);
    (0..n).find(|&slot| inside(tile_rect(slot, rows, cols), x, y)).map(|slot| Hit::Feature(Page::Stocks, first + slot))
}

/// Where Hit::Feature(_, i) is, for the tap flash.
pub(super) fn rect(v: &View, i: usize) -> Option<R> {
    if i == TAP_BACK {
        return Some(back_rect());
    }
    if i >= TAP_RANGE {
        return Some(range_rect(i - TAP_RANGE));
    }
    let first = first_on_screen(v.tile);
    let n = v.stocks.quotes.len().saturating_sub(first).min(PER_SCREEN);
    let (rows, cols) = grid(n);
    i.checked_sub(first).filter(|&s| s < n).map(|slot| tile_rect(slot, rows, cols))
}

pub(super) fn hints(level: &Level) -> &'static str {
    match level {
        Level::Top => "press  pick a stock",
        Level::Focus => "turn  choose  ·  press  open",
        Level::Detail(_) => "turn  1D · 5D · 1M · 1Y  ·  press  Yahoo Finance  ·  double-click  back",
    }
}

pub(super) fn crumb(v: &View) -> Option<String> {
    match v.level {
        Level::Detail(sym) => Some(format!("›  {sym}")),
        Level::Focus => v.stocks.quotes.get(v.tile).map(|q| format!("›  {}", q.symbol)),
        Level::Top => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_read_like_prices() {
        assert_eq!(price(6812.4), "6,812.40");
        assert_eq!(price(64484.17), "64,484.17");
        assert_eq!(price(351.91), "351.91");
        assert_eq!(price(0.45213), "0.4521");
        assert_eq!(price(1234567.0), "1,234,567.00");
        assert_eq!(price(-12.5), "-12.50");
        assert_eq!(grouped(0.87, 2), "0.87");
        assert_eq!(grouped(1427.832, 2), "1,427.83");
    }

    #[test]
    fn grids_fit_the_count() {
        assert_eq!(grid(1), (1, 1));
        assert_eq!(grid(4), (1, 4));
        assert_eq!(grid(5), (2, 3));
        assert_eq!(grid(8), (2, 4));
        assert_eq!(first_on_screen(9), 8);
    }
}
