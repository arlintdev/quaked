//! Night mode on the board (shown over every page while active): a big dim clock, the
//! date, the weather and the first thing on the calendar, on black. Low-contrast greys,
//! and the whole group drifts a few pixels every minute so nothing burns in.
use super::*;

const BLACK: u32 = 0x000000;
const CLOCK: u32 = 0x7E7E88;
const SOFT: u32 = 0x5A5A63;
const DIM: u32 = 0x46464E;
const RULE: u32 = 0x1A1A1E;

/// Where the group sits this minute: a slow Lissajous wander, ±120 x ±18 px.
fn drift(t: &crate::night::LocalTime) -> (f32, f32) {
    let k = (t.day * 1440 + t.hour * 60 + t.minute) as f32;
    let dx = 90.0 * (k * 0.11).sin() + 30.0 * (k * 0.037).sin();
    let dy = 18.0 * (k * 0.083 + 1.3).sin();
    (dx, dy)
}

pub(super) fn draw(c: &mut Canvas, v: &View) {
    let n = v.night;
    c.rect((0.0, 0.0, W, H), BLACK, 1.0);
    let (dx, dy) = drift(&n.time);

    // Left: the clock and the date.
    let (hm, ampm) = n.time.clock();
    let clock_px = 210.0;
    let cw = c.measure(F::Bold, &hm, clock_px);
    let aw = c.measure(F::Medium, ampm, 38.0);
    let date = n.time.date_line();
    let left_w = (cw + 14.0 + aw).max(c.measure(F::Medium, &date, 34.0));

    // Right: the weather, then the calendar.
    let day_ahead = n.time.hour >= 12;
    let weather = n.weather.as_ref();
    let event_line = match (&n.next, n.calendar_ok) {
        (Some(e), _) => Some((format!("{}  ·  {}", e.day, e.when), e.title.clone())),
        (None, true) => Some((if day_ahead { "Tomorrow" } else { "Today" }.to_string(), "Nothing on the calendar".to_string())),
        (None, false) => None,
    };
    let right_w: f32 = if weather.is_some() || event_line.is_some() { 560.0 } else { 0.0 };
    let gap = if right_w > 0.0 { 140.0 } else { 0.0 };
    let total = left_w + gap + right_w;
    let x0 = (W - total) / 2.0 + dx;
    let base = 288.0 + dy;

    c.text(F::Bold, &hm, clock_px, x0, base, CLOCK, Align::Left);
    c.text(F::Medium, ampm, 38.0, x0 + cw + 14.0, base - clock_px * 0.72 + 38.0, SOFT, Align::Left);
    c.text(F::Medium, &date, 34.0, x0 + 6.0, base + 62.0, SOFT, Align::Left);
    if right_w == 0.0 {
        return;
    }
    let rx = x0 + left_w + gap;
    c.rect((rx - gap / 2.0, base - 150.0, 1.5, 230.0), RULE, 1.0);

    // Both: weather on top, the calendar under it. Just one: level with the clock's middle.
    let mut ry = if event_line.is_some() { base - 128.0 } else { base - 92.0 };
    if let Some(w) = weather {
        let sym = crate::night::weather_symbol(w.code, w.is_day);
        c.symbol(sym, 54.0, rx + 34.0, ry + 30.0, SOFT);
        let tw = c.text(F::Bold, &format!("{:.0}°", w.temp), 64.0, rx + 82.0, ry + 54.0, CLOCK, Align::Left);
        let tx = rx + 82.0 + tw + 22.0;
        let what = crate::night::weather_text(w.code);
        c.clip(F::Medium, what, 24.0, rx + right_w - tx, tx, ry + 22.0, SOFT);
        let (label, d) = if day_ahead { ("Tomorrow", w.days.get(1)) } else { ("Today", w.days.first()) };
        let mut line = match d {
            Some(d) => format!("{label}  {:.0}° / {:.0}°", d.high, d.low),
            None => String::new(),
        };
        if !n.place.is_empty() {
            if !line.is_empty() {
                line.push_str("  ·  ");
            }
            line.push_str(&n.place);
        }
        c.clip(F::Medium, &line, 20.0, rx + right_w - tx, tx, ry + 52.0, DIM);
        if day_ahead {
            if let Some(d) = d.filter(|d| d.code != w.code) {
                // Tomorrow looks different from now: say so.
                let t = crate::night::weather_text(d.code);
                if !t.is_empty() {
                    c.clip(F::Medium, &format!("then {}", t.to_lowercase()), 18.0, rx + right_w - tx, tx, ry + 78.0, DIM);
                }
            }
        }
        ry += 150.0;
    } else {
        ry = base - 58.0;
    }
    if let Some((when, title)) = event_line {
        c.symbol("calendar", 24.0, rx + 34.0, ry - 8.0, DIM);
        c.clip(F::Bold, &when, 19.0, right_w - 70.0, rx + 64.0, ry - 1.0, DIM);
        c.clip(F::Medium, &title, 26.0, right_w - 70.0, rx + 64.0, ry + 34.0, SOFT);
    }
}

/// Touch while it shows: night_swallow() eats every input first, so nothing is hit.
pub(super) fn hit(_v: &View, _x: f32, _y: f32) -> Option<Hit> {
    None
}

/// Where Hit::Feature(_, i) is, for the tap flash (nothing to flash).
pub(super) fn rect(_v: &View, _i: usize) -> Option<R> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drift_moves_a_little_each_minute_and_stays_on_screen() {
        let mut t = crate::night::sample().time;
        let mut last = drift(&t);
        for m in 0..24 * 60 {
            t.hour = m / 60;
            t.minute = m % 60;
            let (dx, dy) = drift(&t);
            assert!(dx.abs() <= 120.0 && dy.abs() <= 18.0);
            if m > 0 {
                assert!((dx - last.0).abs() < 14.0 && (dy - last.1).abs() < 3.0, "minute {m}");
            }
            last = (dx, dy);
        }
    }
}
