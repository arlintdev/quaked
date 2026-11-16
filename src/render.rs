//! Draws the board. Everything is laid out on the Quake's native 1920x480 grid; touch
//! reports use the same coordinates, so hit testing shares the layout.

use crate::status::{Session, State};
use fontdue::{Font, FontSettings, Metrics};
use std::cell::RefCell;
use std::collections::HashMap;
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Transform};

pub const W: f32 = 1920.0;
pub const H: f32 = 480.0;

const MARGIN: f32 = 16.0;
const GAP: f32 = 16.0;
const PANEL_W: f32 = 360.0;
const COLS: usize = 4;
const ROWS: usize = 2;
pub const SLOTS: usize = COLS * ROWS;

struct Style {
    bg: u32,
    fg: u32,
    sub: u32,
    label: &'static str,
}

fn style(s: State) -> Style {
    match s {
        State::Working => Style { bg: 0xD97757, fg: 0x1F0E07, sub: 0x4A2416, label: "WORKING" },
        State::Waiting => Style { bg: 0xF2B705, fg: 0x1C1500, sub: 0x5A4500, label: "NEEDS YOU" },
        State::Done => Style { bg: 0x2F9E5B, fg: 0xFFFFFF, sub: 0xDDF7E8, label: "DONE" },
        State::Idle => Style { bg: 0x26262B, fg: 0xD4D4DC, sub: 0x8C8C96, label: "IDLE" },
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    Summary,
    Card(usize),
}

fn panel_rect() -> (f32, f32, f32, f32) {
    (MARGIN, MARGIN, PANEL_W, H - 2.0 * MARGIN)
}

fn card_rect(i: usize) -> (f32, f32, f32, f32) {
    let x0 = MARGIN + PANEL_W + GAP;
    let w_all = W - x0 - MARGIN;
    let h_all = H - 2.0 * MARGIN;
    let cw = (w_all - GAP * (COLS as f32 - 1.0)) / COLS as f32;
    let ch = (h_all - GAP * (ROWS as f32 - 1.0)) / ROWS as f32;
    let (r, c) = (i / COLS, i % COLS);
    (x0 + c as f32 * (cw + GAP), MARGIN + r as f32 * (ch + GAP), cw, ch)
}

pub fn hit(x: f64, y: f64) -> Option<Target> {
    let inside = |(rx, ry, rw, rh): (f32, f32, f32, f32)| {
        let (x, y) = (x as f32, y as f32);
        x >= rx && x <= rx + rw && y >= ry && y <= ry + rh
    };
    if inside(panel_rect()) {
        return Some(Target::Summary);
    }
    (0..SLOTS).find(|&i| inside(card_rect(i))).map(Target::Card)
}

/// Which sessions get a card: start order, but with more sessions than cards the ones
/// that need attention win.
pub fn shown(sessions: &[Session]) -> Vec<Session> {
    if sessions.len() <= SLOTS {
        return sessions.to_vec();
    }
    let mut v = sessions.to_vec();
    v.sort_by(|a, b| a.state.cmp(&b.state).then(a.started_at.cmp(&b.started_at)));
    v.truncate(SLOTS);
    v.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    v
}

pub fn age(ms: u64) -> String {
    let m = ms / 60_000;
    if m < 1 {
        return "now".into();
    }
    if m < 60 {
        return format!("{m}m");
    }
    let h = m / 60;
    if h < 24 {
        return format!("{h}h{:02}", m % 60);
    }
    format!("{}d", h / 24)
}

// ---------------------------------------------------------------- text

pub struct Fonts {
    bold: Font,
    medium: Font,
    cache: RefCell<HashMap<(bool, char, u32), (Metrics, Vec<u8>)>>,
}

fn load_font(candidates: &[(&str, u32)]) -> Font {
    for (path, index) in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let settings = FontSettings { collection_index: *index, scale: 64.0, ..Default::default() };
            if let Ok(f) = Font::from_bytes(bytes, settings) {
                return f;
            }
        }
    }
    panic!("no usable font among {candidates:?}");
}

impl Fonts {
    pub fn load() -> Fonts {
        Fonts {
            bold: load_font(&[
                ("/System/Library/Fonts/HelveticaNeue.ttc", 1),
                ("/System/Library/Fonts/Supplemental/Arial Bold.ttf", 0),
                ("/System/Library/Fonts/SFNS.ttf", 0),
            ]),
            medium: load_font(&[
                ("/System/Library/Fonts/HelveticaNeue.ttc", 10),
                ("/System/Library/Fonts/Supplemental/Arial.ttf", 0),
                ("/System/Library/Fonts/SFNS.ttf", 0),
            ]),
            cache: RefCell::new(HashMap::new()),
        }
    }

    fn font(&self, bold: bool) -> &Font {
        if bold { &self.bold } else { &self.medium }
    }

    fn measure(&self, bold: bool, text: &str, px: f32) -> f32 {
        text.chars().map(|c| self.font(bold).metrics(c, px).advance_width).sum()
    }

    /// Shrinks to 75% first, then ellipsizes, to fit `max_w`.
    fn fit(&self, bold: bool, text: &str, px: f32, max_w: f32) -> (String, f32) {
        let mut size = px;
        while size > px * 0.75 && self.measure(bold, text, size) > max_w {
            size -= 1.0;
        }
        if self.measure(bold, text, size) <= max_w {
            return (text.to_string(), size);
        }
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() {
            chars.pop();
            let s: String = chars.iter().collect::<String>() + "…";
            if self.measure(bold, &s, size) <= max_w {
                return (s, size);
            }
        }
        ("…".into(), size)
    }
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Center,
}

// ---------------------------------------------------------------- canvas

pub struct Canvas<'a> {
    pub pixmap: Pixmap,
    scale: f32,
    fonts: &'a Fonts,
}

fn rgb(c: u32) -> Color {
    Color::from_rgba8((c >> 16) as u8, (c >> 8) as u8, c as u8, 255)
}

impl<'a> Canvas<'a> {
    pub fn new(width: u32, height: u32, fonts: &'a Fonts) -> Canvas<'a> {
        let pixmap = Pixmap::new(width.max(1), height.max(1)).unwrap();
        let scale = (width as f32 / W).min(height as f32 / H);
        Canvas { pixmap, scale, fonts }
    }

    fn rrect(&mut self, (x, y, w, h): (f32, f32, f32, f32), r: f32, color: u32, alpha: f32) {
        let s = self.scale;
        let (x, y, w, h, r) = (x * s, y * s, w * s, h * s, (r * s).min(w / 2.0).min(h / 2.0));
        let mut pb = PathBuilder::new();
        pb.move_to(x + r, y);
        pb.line_to(x + w - r, y);
        pb.quad_to(x + w, y, x + w, y + r);
        pb.line_to(x + w, y + h - r);
        pb.quad_to(x + w, y + h, x + w - r, y + h);
        pb.line_to(x + r, y + h);
        pb.quad_to(x, y + h, x, y + h - r);
        pb.line_to(x, y + r);
        pb.quad_to(x, y, x + r, y);
        pb.close();
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            let mut c = rgb(color);
            c.set_alpha(alpha);
            paint.set_color(c);
            paint.anti_alias = true;
            self.pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        }
    }

    fn rect(&mut self, (x, y, w, h): (f32, f32, f32, f32), color: u32, alpha: f32) {
        let s = self.scale;
        if let Some(r) = Rect::from_xywh(x * s, y * s, w * s, h * s) {
            let mut paint = Paint::default();
            let mut c = rgb(color);
            c.set_alpha(alpha);
            paint.set_color(c);
            self.pixmap.fill_rect(r, &paint, Transform::identity(), None);
        }
    }

    fn circle(&mut self, cx: f32, cy: f32, r: f32, color: u32) {
        let s = self.scale;
        if let Some(path) = PathBuilder::from_circle(cx * s, cy * s, r * s) {
            let mut paint = Paint::default();
            paint.set_color(rgb(color));
            paint.anti_alias = true;
            self.pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        }
    }

    /// Draws `text` with its baseline at `y` (design units). Returns the drawn width.
    fn text(&mut self, bold: bool, text: &str, px: f32, x: f32, y: f32, color: u32, align: Align) -> f32 {
        let s = self.scale;
        let fonts = self.fonts;
        let size = px * s;
        let width = fonts.measure(bold, text, size);
        let mut pen = match align {
            Align::Left => x * s,
            Align::Center => x * s - width / 2.0,
        };
        let base = y * s;
        let (cr, cg, cb) = ((color >> 16) as u8 as f32, (color >> 8) as u8 as f32, color as u8 as f32);
        let pw = self.pixmap.width() as i32;
        let ph = self.pixmap.height() as i32;
        for ch in text.chars() {
            let key = (bold, ch, (size * 10.0) as u32);
            let mut cache = fonts.cache.borrow_mut();
            let (m, bitmap) = cache.entry(key).or_insert_with(|| fonts.font(bold).rasterize(ch, size));
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = (base - m.ymin as f32 - m.height as f32).round() as i32;
            let data = self.pixmap.data_mut();
            for row in 0..m.height as i32 {
                let py = gy + row;
                if py < 0 || py >= ph {
                    continue;
                }
                for col in 0..m.width as i32 {
                    let px_ = gx + col;
                    if px_ < 0 || px_ >= pw {
                        continue;
                    }
                    let a = bitmap[(row * m.width as i32 + col) as usize] as f32 / 255.0;
                    if a <= 0.0 {
                        continue;
                    }
                    let i = ((py * pw + px_) * 4) as usize;
                    data[i] = (cr * a + data[i] as f32 * (1.0 - a)) as u8;
                    data[i + 1] = (cg * a + data[i + 1] as f32 * (1.0 - a)) as u8;
                    data[i + 2] = (cb * a + data[i + 2] as f32 * (1.0 - a)) as u8;
                    data[i + 3] = 255;
                }
            }
            pen += m.advance_width;
        }
        width / s
    }

    fn fitted(&mut self, bold: bool, text: &str, px: f32, max_w: f32, x: f32, y: f32, color: u32, align: Align) {
        let (t, size) = self.fonts.fit(bold, text, px * self.scale, max_w * self.scale);
        self.text(bold, &t, size / self.scale, x, y, color, align);
    }

    /// Brief highlight over a tapped area.
    pub fn flash(&mut self, t: Target) {
        let r = match t {
            Target::Summary => panel_rect(),
            Target::Card(i) => card_rect(i),
        };
        self.rrect(r, 22.0, 0xFFFFFF, 0.28);
    }

    /// Copies the frame into a 0RGB buffer (softbuffer's format).
    pub fn copy_to(&self, out: &mut [u32]) {
        for (dst, px) in out.iter_mut().zip(self.pixmap.data().chunks_exact(4)) {
            *dst = (px[0] as u32) << 16 | (px[1] as u32) << 8 | px[2] as u32;
        }
    }
}

// ---------------------------------------------------------------- board

pub struct Board<'a> {
    pub sessions: &'a [Session],
    pub now_ms: u64,
    pub clock: String,
    pub connected: bool,
}

pub fn draw(c: &mut Canvas, b: &Board) {
    c.pixmap.fill(rgb(0x0B0B0E));
    let mut counts: HashMap<State, usize> = HashMap::new();
    for s in b.sessions {
        *counts.entry(s.state).or_default() += 1;
    }
    let n = |s: State| counts.get(&s).copied().unwrap_or(0);

    // Summary panel
    let panel = panel_rect();
    let tint = if n(State::Waiting) > 0 { 0x2A2200 } else if n(State::Working) > 0 { 0x24140E } else { 0x17171B };
    c.rrect(panel, 24.0, tint, 1.0);
    let (px, py, pw, ph) = panel;
    c.text(true, "CLAUDE", 26.0, px + 30.0, py + 50.0, 0xE8E8EE, Align::Left);
    if !b.connected {
        c.text(false, "device offline", 22.0, px + 150.0, py + 50.0, 0x8C8C96, Align::Left);
    }
    let rows = [(State::Waiting, "need you"), (State::Working, "working"), (State::Done, "done")];
    for (i, (st, label)) in rows.iter().enumerate() {
        let y = py + 140.0 + i as f32 * 92.0;
        let k = n(*st);
        let on = k > 0;
        c.circle(px + 46.0, y - 22.0, 14.0, if on { style(*st).bg } else { 0x3A3A42 });
        let w = c.text(true, &k.to_string(), 64.0, px + 76.0, y, if on { 0xFFFFFF } else { 0x5A5A64 }, Align::Left);
        c.text(false, label, 30.0, px + 76.0 + w + 14.0, y - 2.0, if on { 0xE8E8EE } else { 0x5A5A64 }, Align::Left);
    }
    c.text(true, &b.clock, 34.0, px + 30.0, py + ph - 26.0, 0x8C8C96, Align::Left);
    let hidden = b.sessions.len().saturating_sub(SLOTS);
    if hidden > 0 {
        let t = format!("+{hidden} more");
        c.text(false, &t, 24.0, px + pw - 30.0 - c.fonts.measure(false, &t, 24.0), py + ph - 30.0, 0xC9C9D1, Align::Left);
    }

    // Session cards
    let cards = shown(b.sessions);
    if cards.is_empty() {
        let (x0, y0, _, _) = card_rect(0);
        let (x1, y1, w1, h1) = card_rect(SLOTS - 1);
        c.rrect((x0, y0, x1 + w1 - x0, y1 + h1 - y0), 24.0, 0x121216, 1.0);
        c.text(false, "No Claude sessions running", 34.0, (x0 + x1 + w1) / 2.0, (y0 + y1 + h1) / 2.0 + 12.0, 0x5A5A64, Align::Center);
        return;
    }
    for i in 0..SLOTS {
        let r = card_rect(i);
        let Some(s) = cards.get(i) else {
            c.rrect(r, 22.0, 0x121216, 1.0);
            continue;
        };
        let st = style(s.state);
        let (x, y, w, h) = r;
        c.rrect(r, 22.0, st.bg, 1.0);
        c.fitted(true, &s.name, 38.0, w - 48.0, x + 24.0, y + 54.0, st.fg, Align::Left);
        c.rect((x + 24.0, y + 72.0, w - 48.0, 3.0), st.sub, 0.6);
        c.fitted(true, st.label, 58.0, w - 40.0, x + w / 2.0, y + h * 0.69, st.fg, Align::Center);
        let elapsed = age(b.now_ms.saturating_sub(s.since));
        let foot = match s.state {
            State::Working if !s.detail.is_empty() => format!("{} · {}", s.detail, elapsed),
            State::Done if elapsed == "now" => "just now".to_string(),
            State::Done | State::Idle => format!("{elapsed} ago"),
            _ => elapsed,
        };
        c.fitted(false, &foot, 28.0, w - 48.0, x + w / 2.0, y + h - 24.0, st.sub, Align::Center);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_testing() {
        assert_eq!(hit(100.0, 240.0), Some(Target::Summary));
        let (x, y, w, h) = card_rect(5);
        assert_eq!(hit((x + w / 2.0) as f64, (y + h / 2.0) as f64), Some(Target::Card(5)));
        assert_eq!(hit(5.0, 5.0), None);
    }

    #[test]
    fn ages() {
        assert_eq!(age(30_000), "now");
        assert_eq!(age(4 * 60_000), "4m");
        assert_eq!(age(75 * 60_000), "1h15");
        assert_eq!(age(50 * 3_600_000), "2d");
    }
}
