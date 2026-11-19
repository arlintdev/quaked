//! Focus timer on the board: a dial and a big countdown, today's tally and what's next
//! on the calendar (does a block fit before it?).
use super::*;
use crate::focus::{Phase, Status as Timer};
use std::f32::consts::{FRAC_PI_2, TAU};

const MAIN_W: f32 = 1336.0;
const SIDE_X: f32 = M + MAIN_W + G;
const SIDE_W: f32 = W - M - SIDE_X;
const TODAY_H: f32 = 200.0;
/// The dial: center and radius.
const DIAL_R: f32 = 168.0;
const DIAL_X: f32 = M + 40.0 + DIAL_R;
const DIAL_Y: f32 = CY + CH / 2.0;
const DIAL_W: f32 = 26.0;
/// The countdown and its lines start here.
const TX: f32 = DIAL_X + DIAL_R + 64.0;
/// The buttons' column.
const BW: f32 = 236.0;
const BX: f32 = M + MAIN_W - 24.0 - BW;
const BH: f32 = (CH - 48.0 - 14.0) / 2.0;

fn main_panel() -> R {
    (M, CY, MAIN_W, CH)
}
fn today_card() -> R {
    (SIDE_X, CY, SIDE_W, TODAY_H)
}
fn next_card() -> R {
    (SIDE_X, CY + TODAY_H + G, SIDE_W, CH - TODAY_H - G)
}

/// The buttons for this state: (index, rect). 0 start / pause / resume / again,
/// 1 shorter, 2 longer, 3 cancel / dismiss.
fn buttons(t: &Timer) -> Vec<(usize, R)> {
    let top = (BX, CY + 24.0, BW, BH);
    let y2 = CY + 24.0 + BH + 14.0;
    match t.phase {
        Phase::Ready => {
            let half = (BW - 14.0) / 2.0;
            vec![(0, top), (1, (BX, y2, half, BH)), (2, (BX + half + 14.0, y2, half, BH))]
        }
        _ => vec![(0, top), (3, (BX, y2, BW, BH))],
    }
}

/// The color of the state: the page's color while ready, green to red while running.
fn state_color(v: &View, now: u64) -> u32 {
    let t = v.focus;
    match t.phase {
        Phase::Ready => v.accent,
        Phase::Running => crate::focus::ramp(t.elapsed_frac(now)),
        Phase::Paused => AMBER,
        Phase::Done => crate::focus::RING_GREEN,
    }
}

/// "1:05:00" or "24:59".
fn countdown(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

/// "45m", "1h 15m".
fn span(minutes: u64) -> String {
    if minutes < 60 {
        format!("{minutes}m")
    } else {
        format!("{}h {:02}m", minutes / 60, minutes % 60)
    }
}

/// "in 40 min", "in 2h 05m".
fn until(ms: u64) -> String {
    let m = ms.div_ceil(60_000);
    match m {
        0 | 1 => "in a minute".into(),
        m if m < 60 => format!("in {m} min"),
        m => format!("in {}h {:02}m", m / 60, m % 60),
    }
}

fn plural_min(m: u64) -> String {
    match m {
        1 => "1 min".into(),
        0..=119 => format!("{m} min"),
        _ => format!("{} h", m / 60),
    }
}

/// An arc of the dial from `a` to `b` (fractions of a turn, clockwise from the top).
fn arc(c: &mut Canvas, a: f32, b: f32, color: u32, width: f32) {
    if b <= a {
        return;
    }
    let n = (((b - a) * 180.0).ceil() as usize).max(2);
    let pts: Vec<(f32, f32)> = (0..=n)
        .map(|i| {
            let ang = -FRAC_PI_2 + TAU * (a + (b - a) * i as f32 / n as f32);
            (DIAL_X + DIAL_R * ang.cos(), DIAL_Y + DIAL_R * ang.sin())
        })
        .collect();
    c.polyline(&pts, color, width);
}

fn dial_point(frac: f32, r: f32) -> (f32, f32) {
    let ang = -FRAC_PI_2 + TAU * frac;
    (DIAL_X + r * ang.cos(), DIAL_Y + r * ang.sin())
}

fn dial(c: &mut Canvas, v: &View, now: u64, color: u32) {
    let t = v.focus;
    let elapsed = t.elapsed_frac(now);
    // Track, then what's left of the block (it empties clockwise, like a clock hand).
    arc(c, 0.0, 1.0, 0x1E1E24, DIAL_W);
    let (from, arc_color) = match t.phase {
        Phase::Ready => (0.0, color),
        Phase::Done => (0.0, color),
        _ => (elapsed, color),
    };
    if 1.0 - from > 0.002 {
        arc(c, from, 1.0, arc_color, DIAL_W);
    }
    // A tick every 5 minutes (every minute on a short block), just inside the ring.
    let every = if t.minutes <= 10 { 1 } else { 5 };
    let total = t.minutes.max(1);
    for i in 0..total.div_ceil(every) {
        let f = (i * every) as f32 / total as f32;
        let (x0, y0) = dial_point(f, DIAL_R - DIAL_W / 2.0 - 10.0);
        let (x1, y1) = dial_point(f, DIAL_R - DIAL_W / 2.0 - (if i == 0 { 26.0 } else { 18.0 }));
        let lit = t.phase == Phase::Ready || f >= elapsed;
        c.polyline(&[(x0, y0), (x1, y1)], if lit { 0x55555F } else { 0x2C2C33 }, 3.0);
    }
    // The hand: where the block is now.
    if matches!(t.phase, Phase::Running | Phase::Paused) && elapsed > 0.0 {
        let (hx, hy) = dial_point(elapsed, DIAL_R);
        c.circle(hx, hy, DIAL_W / 2.0 + 3.0, BG, 1.0);
        c.circle(hx, hy, DIAL_W / 2.0 - 3.0, 0xFFFFFF, 1.0);
    }

    // Inside
    let (cx, cy) = (DIAL_X, DIAL_Y);
    match t.phase {
        Phase::Ready => {
            c.text(F::Bold, "LENGTH", 14.0, cx, cy - 58.0, FAINT, Align::Center);
            c.text(F::Bold, &t.minutes.to_string(), 104.0, cx, cy + 34.0, TEXT, Align::Center);
            c.text(F::Bold, "MINUTES", 15.0, cx, cy + 66.0, MUTED, Align::Center);
        }
        Phase::Running => {
            c.text(F::Bold, "ENDS AT", 14.0, cx, cy - 46.0, FAINT, Align::Center);
            c.text(F::Bold, &crate::focus::clock(t.ends_ms), 66.0, cx, cy + 22.0, TEXT, Align::Center);
            c.text(F::Medium, &format!("{} min block", t.minutes), 17.0, cx, cy + 56.0, MUTED, Align::Center);
        }
        Phase::Paused => {
            c.rrect((cx - 26.0, cy - 50.0, 18.0, 64.0), 5.0, AMBER, 1.0);
            c.rrect((cx + 8.0, cy - 50.0, 18.0, 64.0), 5.0, AMBER, 1.0);
            c.text(F::Bold, "PAUSED", 16.0, cx, cy + 54.0, AMBER, Align::Center);
        }
        Phase::Done => {
            c.circle(cx, cy - 14.0, 52.0, color, 1.0);
            c.polyline(&[(cx - 24.0, cy - 14.0), (cx - 6.0, cy + 5.0), (cx + 26.0, cy - 32.0)], 0x0B2A14, 10.0);
            c.text(F::Bold, "DONE", 16.0, cx, cy + 70.0, color, Align::Center);
        }
    }
}

pub(super) fn draw(c: &mut Canvas, v: &View) {
    let t = v.focus;
    let now = v.now_ms;
    let color = state_color(v, now);
    let focus = v.level != &Level::Top;

    let mp = main_panel();
    c.rrect(mp, 14.0, PANEL_DEEP, 1.0);
    if t.phase == Phase::Done {
        c.rrect(mp, 14.0, color, 0.06);
    }
    dial(c, v, now, color);

    // The state, the countdown, one line under it.
    let (label, detail) = match t.phase {
        Phase::Ready => ("READY", format!("{} MIN BLOCK", t.minutes)),
        Phase::Running => ("FOCUSING", format!("{} MIN BLOCK", t.minutes)),
        Phase::Paused => ("PAUSED", format!("{} MIN BLOCK", t.minutes)),
        Phase::Done => ("BLOCK DONE", format!("{} MIN", t.minutes)),
    };
    c.circle(TX + 7.0, CY + 58.0, 7.0, color, 1.0);
    let lw = c.text(F::Bold, label, 20.0, TX + 24.0, CY + 65.0, color, Align::Left);
    c.text(F::Bold, &format!("·   {detail}"), 20.0, TX + 24.0 + lw + 14.0, CY + 65.0, FAINT, Align::Left);

    let secs = t.left_secs(now);
    let digits = countdown(secs);
    let max_w = BX - 40.0 - TX;
    let mut px = 236.0;
    let w = c.measure(F::Bold, &digits, px);
    if w > max_w {
        px *= max_w / w;
    }
    let digit_color = match t.phase {
        Phase::Paused => 0x8E8E98,
        Phase::Done => color,
        _ => TEXT,
    };
    c.text(F::Bold, &digits, px, TX - 6.0, CY + 268.0, digit_color, Align::Left);

    let minutes_in = t.focused_ms(now) / 60_000;
    let sub = match t.phase {
        Phase::Ready => format!("would end at {}", crate::focus::clock(now + t.total_ms())),
        Phase::Running => format!("started {}   ·   {} in", crate::focus::clock(t.started_ms), plural_min(minutes_in)),
        Phase::Paused => format!("{} in   ·   the clock is stopped", plural_min(minutes_in)),
        Phase::Done => {
            let n = t.day(&crate::focus::day_key(t.done_ms)).blocks;
            format!("finished at {}   ·   block {n} today", crate::focus::clock(t.done_ms))
        }
    };
    c.fit(F::Medium, &sub, 26.0, max_w, TX, CY + 330.0, TEXT_2, Align::Left);
    let how = match t.phase {
        _ if !focus => "press the knob to work the timer from it   ·   or tap the buttons",
        Phase::Ready => "turn for a minute at a time, spin faster for more   ·   press to start",
        Phase::Running => "press to pause   ·   hold to cancel",
        Phase::Paused => "press to resume   ·   hold to cancel",
        Phase::Done => "press to clear   ·   tap Again for another block",
    };
    c.fit(F::Medium, how, 17.0, max_w, TX, CY + CH - 26.0, FAINT, Align::Left);

    // Buttons
    for (i, r) in buttons(t) {
        let (label, symbol, bg, fg) = match (i, t.phase) {
            (0, Phase::Ready) => ("Start", "play.fill", v.accent, 0x1F1300),
            (0, Phase::Running) => ("Pause", "pause.fill", 0x2A2A31, TEXT),
            (0, Phase::Paused) => ("Resume", "play.fill", AMBER, 0x1C1500),
            (0, _) => ("Again", "arrow.clockwise", crate::focus::RING_GREEN, 0x0B2A14),
            (1, _) => ("5", "minus", 0x1C1C21, TEXT),
            (2, _) => ("5", "plus", 0x1C1C21, TEXT),
            (3, Phase::Done) => ("Clear", "checkmark", 0x2A2A31, TEXT),
            _ => ("Cancel", "xmark", 0x2C1719, 0xF4B4B4),
        };
        if i == 1 || i == 2 {
            // The length steppers: a big sign and "5 min" under it.
            let at_limit = (i == 1 && t.minutes <= crate::focus::MIN_MIN) || (i == 2 && t.minutes >= crate::focus::MAX_MIN);
            c.rrect(r, 14.0, bg, 1.0);
            let fg = if at_limit { FAINT } else { fg };
            c.symbol(symbol, 34.0, r.0 + r.2 / 2.0, r.1 + r.3 / 2.0 - 12.0, fg);
            c.text(F::Medium, "5 min", 16.0, r.0 + r.2 / 2.0, r.1 + r.3 / 2.0 + 36.0, if at_limit { FAINT } else { MUTED }, Align::Center);
            continue;
        }
        button(c, r, label, bg, fg, symbol);
        // What a knob press does.
        let pressed = if t.phase == Phase::Done { i == 3 } else { i == 0 };
        if focus && pressed {
            c.outline(r, 14.0, TEXT, 3.0);
        }
    }

    today(c, v, now);
    next(c, v, now);
}

fn today(c: &mut Canvas, v: &View, now: u64) {
    let t = v.focus;
    let r = today_card();
    let (x, y, w, _) = r;
    c.rrect(r, 14.0, PANEL, 1.0);
    c.text(F::Bold, "TODAY", 13.0, x + 24.0, y + 34.0, FAINT, Align::Left);
    let d = t.today(now);
    let bw = c.text(F::Bold, &d.blocks.to_string(), 50.0, x + 24.0, y + 104.0, TEXT, Align::Left);
    c.text(F::Medium, if d.blocks == 1 { "block" } else { "blocks" }, 16.0, x + 24.0, y + 132.0, MUTED, Align::Left);
    let fx = x + 24.0 + bw.max(56.0) + 36.0;
    c.text(F::Bold, &span(d.minutes), 50.0, fx, y + 104.0, TEXT, Align::Left);
    c.text(F::Medium, "focused", 16.0, fx, y + 132.0, MUTED, Align::Left);

    // The last seven days, today on the right.
    let days = crate::focus::last_days(now, 7);
    let tally: Vec<crate::focus::Day> = days.iter().map(|(k, _)| t.day(k)).collect();
    let max = tally.iter().map(|d| d.minutes).max().unwrap_or(0).max(60) as f32;
    let (slot, gx) = (20.0, 7.0);
    let gw = 7.0 * slot + 6.0 * gx;
    let (gx0, gy0, gh) = (x + w - 28.0 - gw, y + 50.0, 72.0);
    for (i, ((_, letter), d)) in days.iter().zip(&tally).enumerate() {
        let bx = gx0 + i as f32 * (slot + gx);
        let is_today = i == 6;
        if d.minutes > 0 {
            let bh = (gh * d.minutes as f32 / max).max(5.0);
            c.rrect((bx, gy0 + gh - bh, slot, bh), 4.0, if is_today { v.accent } else { 0x4A4A55 }, 1.0);
        } else {
            c.rrect((bx, gy0 + gh - 4.0, slot, 4.0), 2.0, LINE, 1.0);
        }
        c.text(F::Bold, letter, 12.0, bx + slot / 2.0, gy0 + gh + 22.0, if is_today { TEXT } else { FAINT }, Align::Center);
    }
    let week_min: u64 = tally.iter().map(|d| d.minutes).sum();
    let week_blocks: u32 = tally.iter().map(|d| d.blocks).sum();
    let week = if week_min == 0 { "nothing yet this week".to_string() } else { format!("last 7 days   {}   ·   {week_blocks} blocks", span(week_min)) };
    c.fit(F::Medium, &week, 15.0, w - 48.0, x + 24.0, y + TODAY_H - 22.0, FAINT, Align::Left);
}

/// The next timed event in the next day, if any.
fn next_event<'a>(v: &View<'a>, now: u64) -> Option<&'a crate::calendar::Event> {
    v.calendar.events.iter().filter(|e| !e.all_day && e.start_ms > now && e.start_ms - now < 24 * 3_600_000).min_by_key(|e| e.start_ms)
}

fn next(c: &mut Canvas, v: &View, now: u64) {
    let t = v.focus;
    let r = next_card();
    let (x, y, w, h) = r;
    c.rrect(r, 14.0, PANEL, 1.0);
    c.symbol("calendar", 14.0, x + 31.0, y + 29.0, FAINT);
    c.text(F::Bold, "UP NEXT", 13.0, x + 46.0, y + 34.0, FAINT, Align::Left);
    let Some(e) = next_event(v, now) else {
        c.text(F::Bold, "Nothing coming up", 28.0, x + 24.0, y + 92.0, TEXT_2, Align::Left);
        let why = match &v.calendar.error {
            Some(err) => err.clone(),
            None if v.calendar.events.is_empty() && !v.calendar.authorized => "calendar not connected".to_string(),
            None => "no meetings in the way".to_string(),
        };
        c.fit(F::Medium, &why, 17.0, w - 48.0, x + 24.0, y + 124.0, MUTED, Align::Left);
        return;
    };
    let tomorrow = crate::focus::day_key(e.start_ms) != crate::focus::day_key(now);
    c.fit(F::Bold, &e.title, 30.0, w - 48.0, x + 24.0, y + 82.0, TEXT, Align::Left);
    let when = if tomorrow { format!("tomorrow {}", crate::focus::clock(e.start_ms)) } else { format!("{}   ·   {}", until(e.start_ms - now), crate::focus::clock(e.start_ms)) };
    c.fit(F::Medium, &when, 21.0, w - 48.0, x + 24.0, y + 116.0, TEXT_2, Align::Left);

    // Does the block (running, or one started now) end before it?
    let end = match t.phase {
        Phase::Running => t.ends_ms,
        Phase::Paused => now + t.left_ms,
        Phase::Ready | Phase::Done => now + t.total_ms(),
    };
    let (ok, line) = if end <= e.start_ms {
        let spare = (e.start_ms - end) / 60_000;
        let line = match t.phase {
            Phase::Running | Phase::Paused if spare == 0 => "your block ends right before it".to_string(),
            Phase::Running | Phase::Paused => format!("your block ends {} before it", plural_min(spare)),
            _ if spare == 0 => format!("a {} min block just fits", t.minutes),
            _ => format!("a {} min block fits, {} to spare", t.minutes, plural_min(spare)),
        };
        (true, line)
    } else {
        let over = (end - e.start_ms).div_ceil(60_000);
        let line = match t.phase {
            Phase::Running | Phase::Paused => format!("your block runs {} into it", plural_min(over)),
            _ => format!("only {} free before it", plural_min((e.start_ms - now) / 60_000)),
        };
        (false, line)
    };
    let (dot, bg, fg) = if ok { (crate::focus::RING_GREEN, 0x13261A, 0xBFEBCB) } else { (AMBER, 0x2E2410, 0xF5DFA0) };
    let lw = c.measure(F::Medium, &line, 17.0).min(w - 48.0 - 40.0);
    let (py, ph) = (y + h - 24.0 - 38.0, 38.0);
    c.rrect((x + 24.0, py, lw + 40.0, ph), ph / 2.0, bg, 1.0);
    c.circle(x + 24.0 + 18.0, py + ph / 2.0, 5.0, dot, 1.0);
    c.clip(F::Medium, &line, 17.0, lw, x + 24.0 + 30.0, py + 25.0, fg);
}

/// Touch: the buttons, and the dial (same as the big button).
pub(super) fn hit(v: &View, x: f32, y: f32) -> Option<Hit> {
    if let Some((i, _)) = buttons(v.focus).into_iter().find(|(_, r)| inside(*r, x, y)) {
        return Some(Hit::Feature(Page::Focus, i));
    }
    let (dx, dy) = (x - DIAL_X, y - DIAL_Y);
    if dx * dx + dy * dy <= (DIAL_R + DIAL_W) * (DIAL_R + DIAL_W) {
        return Some(Hit::Feature(Page::Focus, 0));
    }
    None
}

/// Where Hit::Feature(_, i) is, for the tap flash.
pub(super) fn rect(v: &View, i: usize) -> Option<R> {
    buttons(v.focus).into_iter().find(|(j, _)| *j == i).map(|(_, r)| r)
}

pub(super) fn hints(level: &Level) -> &'static str {
    match level {
        Level::Top => "turn  switch page   ·   press  timer controls   ·   tap  start",
        _ => "turn  length   ·   press  start / pause   ·   hold  cancel",
    }
}

/// The breadcrumb next to the tabs: none (the page is the timer).
pub(super) fn crumb(_v: &View) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(countdown(25 * 60), "25:00");
        assert_eq!(countdown(59), "0:59");
        assert_eq!(countdown(3600 + 5 * 60 + 9), "1:05:09");
        assert_eq!(span(45), "45m");
        assert_eq!(span(75), "1h 15m");
        assert_eq!(until(40 * 60_000), "in 40 min");
        assert_eq!(until(125 * 60_000), "in 2h 05m");
    }

    #[test]
    fn layout_fits() {
        let t = Timer { minutes: 25, ..Default::default() };
        for (_, r) in buttons(&t) {
            assert!(r.0 > TX + 600.0 && r.0 + r.2 <= M + MAIN_W && r.1 >= CY && r.1 + r.3 <= CY + CH, "{r:?}");
        }
        const { assert!(DIAL_Y - DIAL_R - DIAL_W / 2.0 > CY && DIAL_Y + DIAL_R + DIAL_W / 2.0 < CY + CH) };
        const { assert!(SIDE_W > 500.0) };
    }
}
