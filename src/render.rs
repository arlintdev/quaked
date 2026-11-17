//! Draws the board. Everything is laid out on the Quake's native 1920x480 grid; touch
//! reports use the same coordinates, so hit testing shares the layout functions.
//!
//! Pages (the knob switches them at the top level): AI and Music.
//! Levels: Top (just looking) -> Focus (knob drives the page) -> Detail (one session).

use crate::music::Track;
use crate::status::{Session, State, Step, StepStatus};
use fontdue::{Font, FontSettings, Metrics};
use std::cell::RefCell;
use std::collections::HashMap;
use tiny_skia::{Color, FillRule, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke, Transform};

pub const W: f32 = 1920.0;
pub const H: f32 = 480.0;
const M: f32 = 12.0; // outer margin
const G: f32 = 10.0; // gap between panels
const CY: f32 = 58.0; // content top (below the top bar)
const CH: f32 = H - CY - M; // content height
const DETAIL_LEFT_W: f32 = 520.0;
const DETAIL_ROW_H: f32 = 30.0;
const DETAIL_ROWS_TOP: f32 = CY + 46.0;
pub const DETAIL_ROWS: usize = ((CH - 56.0) / DETAIL_ROW_H) as usize;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Ai,
    Music,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Level {
    Top,
    Focus,
    Detail(String),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    Tab(Page),
    Session(usize),
    Back,
    OpenClaude,
    Prev,
    PlayPause,
    Next,
    Volume(f32),
}

pub struct View<'a> {
    pub page: Page,
    pub level: &'a Level,
    pub sessions: &'a [Session],
    pub sel: usize,
    pub scroll: usize,
    pub track: Option<&'a Track>,
    pub position: f64,
    pub volume: Option<u8>,
    pub problem: Option<&'a str>,
    pub art: Option<&'a Pixmap>,
    pub now_ms: u64,
    pub clock: &'a str,
    pub connected: bool,
    pub flash: Option<Hit>,
    /// Alternates every second; drives the "running" pulse.
    pub tick: bool,
}

// ---------------------------------------------------------------- colors

const BG: u32 = 0x09090B;
const PANEL: u32 = 0x151519;
const PANEL_DEEP: u32 = 0x101013;
const TEXT: u32 = 0xE8E8EE;
const TEXT_2: u32 = 0xB4B4BE;
const MUTED: u32 = 0x8C8C96;
const FAINT: u32 = 0x5A5A64;
const LINE: u32 = 0x2A2A31;
const AMBER: u32 = 0xF2B705;
const ORANGE: u32 = 0xD97757;
const GREEN: u32 = 0x2F9E5B;
const RED: u32 = 0xFF6B6B;
const SPOTIFY: u32 = 0x1DB954;

struct StateStyle {
    accent: u32,
    on_accent: u32,
    label: &'static str,
}

fn state_style(s: State) -> StateStyle {
    match s {
        State::Working => StateStyle { accent: ORANGE, on_accent: 0x1F0E07, label: "WORKING" },
        State::Waiting => StateStyle { accent: AMBER, on_accent: 0x1C1500, label: "NEEDS YOU" },
        State::Done => StateStyle { accent: GREEN, on_accent: 0xFFFFFF, label: "DONE" },
        State::Idle => StateStyle { accent: 0x3A3A42, on_accent: 0xD4D4DC, label: "IDLE" },
    }
}

fn tag_color(step: &Step) -> u32 {
    if step.status == StepStatus::Error {
        return RED;
    }
    match step.tag.as_str() {
        "$" => 0xE6B673,
        "edit" | "write" => 0x7FD1B9,
        "read" | "grep" | "glob" => 0x8AB4F8,
        "web" | "search" => 0xC3A6FF,
        "agent" | "skill" => 0xF28FAD,
        "you" => 0x9FB4FF,
        "needs" => AMBER,
        _ => 0xB0B0BA,
    }
}

// ---------------------------------------------------------------- formatting

/// Minutes-level age for cards: "now", "4m", "1h05", "2d".
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

/// Seconds-level age for timelines: "12s", "4m", "1h05".
fn age_short(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 { format!("{s}s") } else { age(ms) }
}

fn mmss(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn short_path(p: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match p.strip_prefix(&home) {
        Some(r) if !home.is_empty() => format!("~{r}"),
        _ => p.to_string(),
    }
}

// ---------------------------------------------------------------- fonts & canvas

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum F {
    Bold,
    Medium,
    Regular,
    Mono,
}

pub struct Fonts {
    bold: Font,
    medium: Font,
    regular: Font,
    mono: Font,
    cache: RefCell<HashMap<(F, char, u32), (Metrics, Vec<u8>)>>,
    advances: RefCell<HashMap<(F, char, u32), f32>>,
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
            bold: load_font(&[("/System/Library/Fonts/HelveticaNeue.ttc", 1), ("/System/Library/Fonts/Supplemental/Arial Bold.ttf", 0)]),
            medium: load_font(&[("/System/Library/Fonts/HelveticaNeue.ttc", 10), ("/System/Library/Fonts/Supplemental/Arial.ttf", 0)]),
            regular: load_font(&[("/System/Library/Fonts/HelveticaNeue.ttc", 0), ("/System/Library/Fonts/Supplemental/Arial.ttf", 0)]),
            mono: load_font(&[("/System/Library/Fonts/Menlo.ttc", 0), ("/System/Library/Fonts/SFNSMono.ttf", 0)]),
            cache: RefCell::new(HashMap::new()),
            advances: RefCell::new(HashMap::new()),
        }
    }

    fn font(&self, f: F) -> &Font {
        match f {
            F::Bold => &self.bold,
            F::Medium => &self.medium,
            F::Regular => &self.regular,
            F::Mono => &self.mono,
        }
    }

    fn measure(&self, f: F, text: &str, px: f32) -> f32 {
        let mut adv = self.advances.borrow_mut();
        let size = (px * 10.0) as u32;
        text.chars().map(|c| *adv.entry((f, c, size)).or_insert_with(|| self.font(f).metrics(c, px).advance_width)).sum()
    }

    fn ellipsize(&self, f: F, text: &str, px: f32, max_w: f32) -> String {
        if self.measure(f, text, px) <= max_w {
            return text.to_string();
        }
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() {
            chars.pop();
            let s: String = chars.iter().collect::<String>().trim_end().to_string() + "…";
            if self.measure(f, &s, px) <= max_w {
                return s;
            }
        }
        "…".into()
    }

    /// Greedy word wrap into at most `max_lines`, ellipsizing the last line.
    fn wrap(&self, f: F, text: &str, px: f32, max_w: f32, max_lines: usize) -> Vec<String> {
        if max_lines == 0 {
            return Vec::new();
        }
        // Words wider than a line are split into pieces that fit.
        let mut tokens: Vec<String> = Vec::new();
        for w in text.split_whitespace() {
            if self.measure(f, w, px) <= max_w {
                tokens.push(w.to_string());
                continue;
            }
            let mut part = String::new();
            for ch in w.chars() {
                if !part.is_empty() && self.measure(f, &format!("{part}{ch}"), px) > max_w {
                    tokens.push(std::mem::take(&mut part));
                }
                part.push(ch);
            }
            if !part.is_empty() {
                tokens.push(part);
            }
        }
        let mut lines: Vec<String> = Vec::new();
        let mut cur = String::new();
        for (i, t) in tokens.iter().enumerate() {
            let cand = if cur.is_empty() { t.clone() } else { format!("{cur} {t}") };
            if self.measure(f, &cand, px) <= max_w {
                cur = cand;
                continue;
            }
            lines.push(std::mem::take(&mut cur));
            if lines.len() == max_lines {
                let rest = tokens[i..].join(" ");
                let last = lines.pop().unwrap_or_default();
                lines.push(self.ellipsize(f, &format!("{last} {rest}"), px, max_w));
                return lines;
            }
            cur = t.clone();
        }
        if !cur.is_empty() && lines.len() < max_lines {
            lines.push(cur);
        }
        lines
    }
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Center,
    Right,
}

type R = (f32, f32, f32, f32);

fn inside((rx, ry, rw, rh): R, x: f32, y: f32) -> bool {
    x >= rx && x <= rx + rw && y >= ry && y <= ry + rh
}

fn rgb(c: u32) -> Color {
    Color::from_rgba8((c >> 16) as u8, (c >> 8) as u8, c as u8, 255)
}

pub struct Canvas<'a> {
    pub pixmap: Pixmap,
    scale: f32,
    fonts: &'a Fonts,
}

fn rrect_path((x, y, w, h): R, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
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
    pb.finish()
}

impl<'a> Canvas<'a> {
    pub fn new(width: u32, height: u32, fonts: &'a Fonts) -> Canvas<'a> {
        let pixmap = Pixmap::new(width.max(1), height.max(1)).unwrap();
        let scale = (width as f32 / W).min(height as f32 / H);
        Canvas { pixmap, scale, fonts }
    }

    fn sr(&self, (x, y, w, h): R) -> R {
        let s = self.scale;
        (x * s, y * s, w * s, h * s)
    }

    fn paint(color: u32, alpha: f32) -> Paint<'static> {
        let mut p = Paint::default();
        let mut c = rgb(color);
        c.set_alpha(alpha);
        p.set_color(c);
        p.anti_alias = true;
        p
    }

    fn rrect(&mut self, r: R, radius: f32, color: u32, alpha: f32) {
        if let Some(path) = rrect_path(self.sr(r), radius * self.scale) {
            self.pixmap.fill_path(&path, &Self::paint(color, alpha), FillRule::Winding, Transform::identity(), None);
        }
    }

    fn outline(&mut self, r: R, radius: f32, color: u32, width: f32) {
        let half = width / 2.0;
        let r = (r.0 + half, r.1 + half, r.2 - width, r.3 - width);
        if let Some(path) = rrect_path(self.sr(r), radius * self.scale) {
            let stroke = Stroke { width: width * self.scale, ..Default::default() };
            self.pixmap.stroke_path(&path, &Self::paint(color, 1.0), &stroke, Transform::identity(), None);
        }
    }

    fn rect(&mut self, r: R, color: u32, alpha: f32) {
        let (x, y, w, h) = self.sr(r);
        if let Some(r) = Rect::from_xywh(x, y, w, h) {
            self.pixmap.fill_rect(r, &Self::paint(color, alpha), Transform::identity(), None);
        }
    }

    fn circle(&mut self, cx: f32, cy: f32, r: f32, color: u32, alpha: f32) {
        let s = self.scale;
        if let Some(path) = PathBuilder::from_circle(cx * s, cy * s, r * s) {
            self.pixmap.fill_path(&path, &Self::paint(color, alpha), FillRule::Winding, Transform::identity(), None);
        }
    }

    fn poly(&mut self, pts: &[(f32, f32)], color: u32) {
        let s = self.scale;
        let mut pb = PathBuilder::new();
        pb.move_to(pts[0].0 * s, pts[0].1 * s);
        for p in &pts[1..] {
            pb.line_to(p.0 * s, p.1 * s);
        }
        pb.close();
        if let Some(path) = pb.finish() {
            self.pixmap.fill_path(&path, &Self::paint(color, 1.0), FillRule::Winding, Transform::identity(), None);
        }
    }

    fn measure(&self, f: F, text: &str, px: f32) -> f32 {
        self.fonts.measure(f, text, px)
    }

    /// Draws `text` with its baseline at `y`. Returns the drawn width (design units).
    fn text(&mut self, f: F, text: &str, px: f32, x: f32, y: f32, color: u32, align: Align) -> f32 {
        let s = self.scale;
        let fonts = self.fonts;
        let size = px * s;
        let width = fonts.measure(f, text, size);
        let mut pen = match align {
            Align::Left => x * s,
            Align::Center => x * s - width / 2.0,
            Align::Right => x * s - width,
        };
        let base = y * s;
        let (cr, cg, cb) = ((color >> 16) as u8 as f32, (color >> 8) as u8 as f32, color as u8 as f32);
        let pw = self.pixmap.width() as i32;
        let ph = self.pixmap.height() as i32;
        let data = self.pixmap.data_mut();
        let mut cache = fonts.cache.borrow_mut();
        for ch in text.chars() {
            let key = (f, ch, (size * 10.0) as u32);
            let (m, bitmap) = cache.entry(key).or_insert_with(|| fonts.font(f).rasterize(ch, size));
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = (base - m.ymin as f32 - m.height as f32).round() as i32;
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

    /// Like `text`, but shrinks (to 80%) and then ellipsizes to fit `max_w`.
    #[allow(clippy::too_many_arguments)]
    fn fit(&mut self, f: F, text: &str, px: f32, max_w: f32, x: f32, y: f32, color: u32, align: Align) -> f32 {
        let mut size = px;
        while size > px * 0.8 && self.measure(f, text, size) > max_w {
            size -= 1.0;
        }
        let t = self.fonts.ellipsize(f, text, size, max_w);
        self.text(f, &t, size, x, y, color, align)
    }

    /// Left-aligned text cut with an ellipsis to `max_w`, never shrunk.
    #[allow(clippy::too_many_arguments)]
    fn clip(&mut self, f: F, text: &str, px: f32, max_w: f32, x: f32, y: f32, color: u32) -> f32 {
        let t = self.fonts.ellipsize(f, text, px, max_w);
        self.text(f, &t, px, x, y, color, Align::Left)
    }

    #[allow(clippy::too_many_arguments)]
    fn paragraph(&mut self, f: F, text: &str, px: f32, x: f32, y: f32, max_w: f32, max_lines: usize, line_h: f32, color: u32) -> f32 {
        let lines = self.fonts.wrap(f, text, px, max_w, max_lines);
        for (i, l) in lines.iter().enumerate() {
            self.text(f, l, px, x, y + i as f32 * line_h, color, Align::Left);
        }
        lines.len() as f32 * line_h
    }

    fn image(&mut self, img: &Pixmap, r: R, radius: f32) {
        let (x, y, w, h) = self.sr(r);
        let (sx, sy) = (w / img.width() as f32, h / img.height() as f32);
        let k = sx.max(sy); // cover
        let (dw, dh) = (img.width() as f32 * k, img.height() as f32 * k);
        let t = Transform::from_row(k, 0.0, 0.0, k, x + (w - dw) / 2.0, y + (h - dh) / 2.0);
        let mut mask = Mask::new(self.pixmap.width(), self.pixmap.height());
        if let (Some(m), Some(path)) = (mask.as_mut(), rrect_path((x, y, w, h), radius * self.scale)) {
            m.fill_path(&path, FillRule::Winding, true, Transform::identity());
        }
        let paint = PixmapPaint { quality: tiny_skia::FilterQuality::Bicubic, ..Default::default() };
        self.pixmap.draw_pixmap(0, 0, img.as_ref(), &paint, t, mask.as_ref());
    }

    fn pill(&mut self, x: f32, y: f32, label: &str, px: f32, bg: u32, fg: u32) -> f32 {
        let w = self.measure(F::Bold, label, px) + px * 1.4;
        let h = px * 1.8;
        self.rrect((x, y, w, h), h / 2.0, bg, 1.0);
        self.text(F::Bold, label, px, x + w / 2.0, y + h * 0.69, fg, Align::Center);
        w
    }

    /// Copies the frame into a 0RGB buffer (softbuffer's format).
    pub fn copy_to(&self, out: &mut [u32]) {
        for (dst, px) in out.iter_mut().zip(self.pixmap.data().chunks_exact(4)) {
            *dst = (px[0] as u32) << 16 | (px[1] as u32) << 8 | px[2] as u32;
        }
    }
}

// ---------------------------------------------------------------- layout

fn tab_rect(p: Page) -> R {
    match p {
        Page::Ai => (M, 9.0, 84.0, 34.0),
        Page::Music => (M + 92.0, 9.0, 120.0, 34.0),
    }
}

fn columns(n: usize) -> usize {
    match n {
        0..=3 => 3,
        4 => 4,
        _ => 5,
    }
}

/// First visible session and column count for the overview.
pub fn window(n: usize, sel: usize) -> (usize, usize) {
    let cols = columns(n);
    let first = if n <= cols { 0 } else { sel.saturating_sub(cols - 1).min(n - cols) };
    (first, cols)
}

fn col_rect(slot: usize, cols: usize) -> R {
    let w = (W - 2.0 * M - (cols as f32 - 1.0) * G) / cols as f32;
    (M + slot as f32 * (w + G), CY, w, CH)
}

fn detail_left() -> R {
    (M, CY, DETAIL_LEFT_W, CH)
}

fn detail_right() -> R {
    (M + DETAIL_LEFT_W + G, CY, W - 2.0 * M - DETAIL_LEFT_W - G, CH)
}

fn back_button() -> R {
    (M + 24.0, CY + CH - 52.0, 124.0, 38.0)
}

fn open_button() -> R {
    (M + 24.0 + 134.0, CY + CH - 52.0, 206.0, 38.0)
}

const ART: R = (M, CY, CH, CH);
const MX: f32 = M + CH + 28.0; // music info column
const BTN_Y: f32 = CY + CH - 112.0;
fn prev_button() -> R {
    (MX, BTN_Y, 140.0, 104.0)
}
fn play_button() -> R {
    (MX + 152.0, BTN_Y, 184.0, 104.0)
}
fn next_button() -> R {
    (MX + 348.0, BTN_Y, 140.0, 104.0)
}
const VOL_X: f32 = 1360.0;
fn volume_bar() -> R {
    (VOL_X, BTN_Y + 62.0, W - M - VOL_X - 24.0, 14.0)
}
fn volume_hit() -> R {
    (VOL_X - 10.0, BTN_Y + 10.0, W - M - VOL_X + 10.0, 94.0)
}

pub fn hit(v: &View, x: f64, y: f64) -> Option<Hit> {
    let (x, y) = (x as f32, y as f32);
    for p in [Page::Ai, Page::Music] {
        if inside(tab_rect(p), x, y) {
            return Some(Hit::Tab(p));
        }
    }
    match (v.page, v.level) {
        (Page::Ai, Level::Detail(_)) => {
            if inside(back_button(), x, y) {
                Some(Hit::Back)
            } else if inside(open_button(), x, y) {
                Some(Hit::OpenClaude)
            } else {
                None
            }
        }
        (Page::Ai, _) => {
            let (first, cols) = window(v.sessions.len(), v.sel);
            (0..cols).find(|&slot| inside(col_rect(slot, cols), x, y)).map(|slot| first + slot).filter(|&i| i < v.sessions.len()).map(Hit::Session)
        }
        (Page::Music, _) => {
            if inside(prev_button(), x, y) {
                Some(Hit::Prev)
            } else if inside(play_button(), x, y) || inside(ART, x, y) {
                Some(Hit::PlayPause)
            } else if inside(next_button(), x, y) {
                Some(Hit::Next)
            } else if inside(volume_hit(), x, y) {
                let (bx, _, bw, _) = volume_bar();
                Some(Hit::Volume(((x - bx) / bw).clamp(0.0, 1.0)))
            } else {
                None
            }
        }
    }
}

fn hit_rect(v: &View, h: Hit) -> Option<R> {
    Some(match h {
        Hit::Tab(p) => tab_rect(p),
        Hit::Session(i) => {
            let (first, cols) = window(v.sessions.len(), v.sel);
            col_rect(i.checked_sub(first)?, cols)
        }
        Hit::Back => back_button(),
        Hit::OpenClaude => open_button(),
        Hit::Prev => prev_button(),
        Hit::PlayPause => play_button(),
        Hit::Next => next_button(),
        Hit::Volume(_) => volume_hit(),
    })
}

// ---------------------------------------------------------------- drawing

pub fn draw(c: &mut Canvas, v: &View) {
    c.pixmap.fill(rgb(BG));
    top_bar(c, v);
    match (v.page, v.level) {
        (Page::Ai, Level::Detail(id)) => match v.sessions.iter().find(|s| &s.id == id) {
            Some(s) => ai_detail(c, v, s),
            None => ai_overview(c, v),
        },
        (Page::Ai, _) => ai_overview(c, v),
        (Page::Music, _) => music(c, v),
    }
    if let Some(r) = v.flash.and_then(|h| hit_rect(v, h)) {
        c.rrect(r, 14.0, 0xFFFFFF, 0.22);
    }
}

fn hints(v: &View) -> &'static str {
    match (v.page, v.level) {
        (Page::Ai, Level::Top) => "turn  switch page   ·   press  pick a session",
        (Page::Music, Level::Top) => "turn  switch page   ·   press  music controls",
        (Page::Ai, Level::Focus) => "turn  choose   ·   press  open   ·   double-click  back",
        (Page::Ai, Level::Detail(_)) => "turn  scroll   ·   press  open in Claude   ·   double-click  back",
        (Page::Music, _) => "turn  volume   ·   press  play / pause   ·   double-click  back",
    }
}

fn top_bar(c: &mut Canvas, v: &View) {
    let waiting = v.sessions.iter().filter(|s| s.state == State::Waiting).count();
    for p in [Page::Ai, Page::Music] {
        let r = tab_rect(p);
        let active = v.page == p;
        c.rrect(r, 17.0, if active { TEXT } else { 0x1C1C21 }, 1.0);
        let label = if p == Page::Ai { "AI" } else { "MUSIC" };
        c.text(F::Bold, label, 17.0, r.0 + r.2 / 2.0, r.1 + 23.5, if active { 0x111114 } else { MUTED }, Align::Center);
        if active && v.level != &Level::Top {
            c.outline((r.0 - 3.0, r.1 - 3.0, r.2 + 6.0, r.3 + 6.0), 20.0, ORANGE, 2.0);
        }
    }
    if waiting > 0 {
        let r = tab_rect(Page::Ai);
        c.circle(r.0 + r.2 - 4.0, r.1 + 4.0, 11.0, AMBER, 1.0);
        c.text(F::Bold, &waiting.to_string(), 14.0, r.0 + r.2 - 4.0, r.1 + 9.0, 0x1C1500, Align::Center);
    }

    // Breadcrumb
    let mut x = M + 232.0;
    let crumb = match (v.page, v.level) {
        (Page::Ai, Level::Detail(id)) => v.sessions.iter().find(|s| &s.id == id).map(|s| format!("›  {}", s.name)),
        (Page::Ai, Level::Focus) => v.sessions.get(v.sel).map(|s| format!("›  choose  ·  {}", s.name)),
        (Page::Music, Level::Focus) => Some("›  controls".into()),
        _ => None,
    };
    if let Some(cb) = crumb {
        x += c.fit(F::Bold, &cb, 20.0, 360.0, x, 33.0, TEXT, Align::Left) + 24.0;
    }
    let mut hx = x.max(M + 232.0);
    for (i, part) in hints(v).split("   ·   ").enumerate() {
        if i > 0 {
            hx += c.text(F::Medium, "·", 15.0, hx + 10.0, 32.0, FAINT, Align::Left) + 20.0;
        }
        let (key, rest) = part.split_once("  ").unwrap_or((part, ""));
        hx += c.text(F::Bold, key, 15.0, hx, 32.0, MUTED, Align::Left) + 6.0;
        hx += c.text(F::Medium, rest, 15.0, hx, 32.0, FAINT, Align::Left);
    }

    // Right side: device / counts / now playing / clock
    let mut rx = W - M;
    rx -= c.text(F::Bold, v.clock, 22.0, rx, 34.0, 0xC9C9D1, Align::Right) + 24.0;
    let counts = [(State::Waiting, AMBER), (State::Working, ORANGE), (State::Done, GREEN)];
    for (st, col) in counts.iter().rev() {
        let n = v.sessions.iter().filter(|s| s.state == *st).count();
        if n == 0 {
            continue;
        }
        let w = c.text(F::Bold, &n.to_string(), 18.0, rx, 33.0, TEXT, Align::Right);
        c.circle(rx - w - 11.0, 27.0, 6.0, *col, 1.0);
        rx -= w + 30.0;
    }
    if !v.connected {
        rx -= c.text(F::Medium, "device offline", 15.0, rx, 32.0, RED, Align::Right) + 20.0;
    }
    if v.page == Page::Ai {
        if let Some(t) = v.track.filter(|t| t.playing) {
            let label = format!("{} — {}", t.title, t.artist);
            let w = c.measure(F::Medium, &label, 15.0).min(340.0);
            c.fit(F::Medium, &label, 15.0, 340.0, rx - w, 32.0, MUTED, Align::Left);
            let ix = rx - w - 18.0;
            c.poly(&[(ix, 21.0), (ix + 10.0, 27.0), (ix, 33.0)], SPOTIFY);
        }
    }
    c.rect((M, CY - 6.0, W - 2.0 * M, 1.0), LINE, 1.0);
}

fn step_line(c: &mut Canvas, v: &View, step: &Step, x: f32, y: f32, w: f32, px: f32, tag_w: f32) {
    let col = tag_color(step);
    if step.status == StepStatus::Running {
        c.circle(x + 4.0, y - px * 0.35, 4.0, ORANGE, if v.tick { 1.0 } else { 0.35 });
    }
    c.clip(F::Mono, &step.tag, px, tag_w - 16.0, x + 14.0, y, col);
    let text_col = match step.status {
        StepStatus::Error => 0xFF9B9B,
        StepStatus::Info => TEXT_2,
        _ => 0xD0D0D8,
    };
    let text = if step.text.is_empty() { "—" } else { step.text.as_str() };
    c.clip(F::Mono, text, px, w - tag_w - 4.0, x + tag_w, y, text_col);
}

fn ai_overview(c: &mut Canvas, v: &View) {
    let n = v.sessions.len();
    if n == 0 {
        c.rrect((M, CY, W - 2.0 * M, CH), 16.0, PANEL_DEEP, 1.0);
        c.text(F::Medium, "No Claude sessions running", 34.0, W / 2.0, CY + CH / 2.0, FAINT, Align::Center);
        c.text(F::Regular, "Sessions show up here as soon as you send a prompt.", 20.0, W / 2.0, CY + CH / 2.0 + 40.0, FAINT, Align::Center);
        return;
    }
    let (first, cols) = window(n, v.sel);
    for slot in 0..cols {
        let r = col_rect(slot, cols);
        let i = first + slot;
        let Some(s) = v.sessions.get(i) else {
            c.rrect(r, 14.0, PANEL_DEEP, 1.0);
            continue;
        };
        session_card(c, v, s, r);
        if v.level == &Level::Focus && i == v.sel {
            c.outline(r, 14.0, TEXT, 3.0);
        }
    }
    // Off-screen sessions
    let left = first;
    let right = n.saturating_sub(first + cols);
    if left > 0 {
        let urgent = v.sessions[..first].iter().any(|s| s.state == State::Waiting);
        let r = col_rect(0, cols);
        c.pill(r.0 + 8.0, r.1 + r.3 - 40.0, &format!("‹ {left}"), 16.0, if urgent { AMBER } else { 0x2A2A31 }, if urgent { 0x1C1500 } else { TEXT });
    }
    if right > 0 {
        let urgent = v.sessions[first + cols..].iter().any(|s| s.state == State::Waiting);
        let r = col_rect(cols - 1, cols);
        let label = format!("{right} ›");
        let w = c.measure(F::Bold, &label, 16.0) + 16.0 * 1.4;
        c.pill(r.0 + r.2 - w - 8.0, r.1 + r.3 - 40.0, &label, 16.0, if urgent { AMBER } else { 0x2A2A31 }, if urgent { 0x1C1500 } else { TEXT });
    }
}

fn session_card(c: &mut Canvas, v: &View, s: &Session, r: R) {
    let st = state_style(s.state);
    let (x, y, w, h) = r;
    let waiting = s.state == State::Waiting;
    c.rrect(r, 14.0, if waiting { 0x231C06 } else { PANEL }, 1.0);
    if waiting {
        c.outline(r, 14.0, AMBER, 2.0);
    }
    c.rrect((x, y, 6.0, h), 3.0, st.accent, 1.0);
    let ix = x + 20.0;
    let iw = w - 36.0;
    let elapsed = age(v.now_ms.saturating_sub(s.since));
    let ew = c.text(F::Medium, &elapsed, 17.0, x + w - 16.0, y + 34.0, MUTED, Align::Right);
    c.fit(F::Bold, &s.name, 26.0, iw - ew - 12.0, ix, y + 35.0, TEXT, Align::Left);
    let pw = c.pill(ix, y + 48.0, st.label, 13.0, st.accent, st.on_accent);
    let steps = if s.step_count == 1 { "1 step".to_string() } else { format!("{} steps", s.step_count) };
    c.text(F::Medium, &steps, 15.0, ix + pw + 10.0, y + 66.0, MUTED, Align::Left);

    let mut cy = y + 98.0;
    if !s.prompt.is_empty() {
        c.text(F::Bold, "›", 18.0, ix, cy, FAINT, Align::Left);
        cy += c.paragraph(F::Regular, &s.prompt, 17.0, ix + 14.0, cy, iw - 14.0, 2, 22.0, TEXT_2);
    }
    if waiting && !s.notice.is_empty() {
        cy += 6.0;
        cy += c.paragraph(F::Bold, &s.notice, 17.0, ix, cy, iw, 2, 22.0, AMBER);
    } else if matches!(s.state, State::Done | State::Idle) && !s.last_message.is_empty() {
        cy += 6.0;
        cy += c.paragraph(F::Regular, &s.last_message, 16.0, ix, cy, iw, 3, 21.0, 0x9FD9B5);
    }
    cy += 4.0;
    c.rect((ix, cy - 10.0, iw, 1.0), LINE, 1.0);
    cy += 14.0;
    let line_h = 24.0;
    let room = ((y + h - 12.0 - cy) / line_h).floor().max(0.0) as usize;
    for step in s.steps.iter().rev().filter(|st| st.status != StepStatus::Info).take(room) {
        step_line(c, v, step, ix - 4.0, cy, iw + 4.0, 14.0, 76.0);
        cy += line_h;
    }
}

fn ai_detail(c: &mut Canvas, v: &View, s: &Session) {
    let st = state_style(s.state);
    let lr = detail_left();
    let (x, y, w, h) = lr;
    let waiting = s.state == State::Waiting;
    c.rrect(lr, 14.0, if waiting { 0x231C06 } else { PANEL }, 1.0);
    c.rrect((x, y, 6.0, h), 3.0, st.accent, 1.0);
    let ix = x + 24.0;
    let iw = w - 44.0;
    c.fit(F::Bold, &s.name, 34.0, iw, ix, y + 44.0, TEXT, Align::Left);
    let pw = c.pill(ix, y + 58.0, st.label, 14.0, st.accent, st.on_accent);
    let meta = format!(
        "{}  ·  {} steps  ·  started {} ago",
        age(v.now_ms.saturating_sub(s.since)),
        s.step_count,
        age(v.now_ms.saturating_sub(s.started_at))
    );
    c.fit(F::Medium, &meta, 16.0, iw - pw - 12.0, ix + pw + 12.0, y + 78.0, MUTED, Align::Left);
    c.fit(F::Mono, &short_path(&s.cwd), 14.0, iw, ix, y + 110.0, FAINT, Align::Left);

    let mut cy = y + 144.0;
    c.text(F::Bold, "PROMPT", 12.0, ix, cy, FAINT, Align::Left);
    cy += 22.0;
    let prompt = if s.prompt.is_empty() { "—" } else { s.prompt.as_str() };
    cy += c.paragraph(F::Regular, prompt, 18.0, ix, cy, iw, 4, 23.0, TEXT_2);
    cy += 12.0;
    let (label, body, col) = if waiting && !s.notice.is_empty() {
        ("NEEDS YOU", s.notice.as_str(), AMBER)
    } else if !s.last_message.is_empty() {
        let label = if s.state == State::Working { "LAST REPLY" } else { "CLAUDE" };
        (label, s.last_message.as_str(), if s.state == State::Working { TEXT_2 } else { 0x9FD9B5 })
    } else {
        ("", "", 0)
    };
    if !label.is_empty() {
        c.text(F::Bold, label, 12.0, ix, cy, if waiting { AMBER } else { FAINT }, Align::Left);
        cy += 22.0;
        let lines = ((back_button().1 - 14.0 - cy) / 22.0).floor().max(1.0) as usize;
        c.paragraph(if waiting { F::Bold } else { F::Regular }, body, 17.0, ix, cy, iw, lines.min(5), 22.0, col);
    }
    for (r, label) in [(back_button(), "‹  Back"), (open_button(), "Open in Claude")] {
        c.rrect(r, 10.0, 0x26262D, 1.0);
        c.text(F::Medium, label, 17.0, r.0 + r.2 / 2.0, r.1 + 25.0, TEXT, Align::Center);
    }

    // Timeline
    let rr = detail_right();
    let (rx, ry, rw, rh) = rr;
    c.rrect(rr, 14.0, PANEL_DEEP, 1.0);
    c.text(F::Bold, "STEPS", 12.0, rx + 24.0, ry + 28.0, FAINT, Align::Left);
    c.text(F::Medium, "newest first", 13.0, rx + rw - 24.0, ry + 28.0, FAINT, Align::Right);
    let steps: Vec<&Step> = s.steps.iter().rev().collect();
    if steps.is_empty() {
        c.text(F::Medium, "No steps yet", 22.0, rx + rw / 2.0, ry + rh / 2.0, FAINT, Align::Center);
        return;
    }
    let max_scroll = steps.len().saturating_sub(DETAIL_ROWS);
    let scroll = v.scroll.min(max_scroll);
    let time_x = rx + 24.0;
    for (row, step) in steps.iter().skip(scroll).take(DETAIL_ROWS).enumerate() {
        let by = DETAIL_ROWS_TOP + row as f32 * DETAIL_ROW_H + 20.0;
        if row % 2 == 1 {
            c.rect((rx + 12.0, by - 20.0, rw - 24.0, DETAIL_ROW_H), 0xFFFFFF, 0.025);
        }
        c.text(F::Mono, &age_short(v.now_ms.saturating_sub(step.ts)), 13.0, time_x + 50.0, by, FAINT, Align::Right);
        step_line(c, v, step, time_x + 64.0, by, rw - 24.0 - 64.0 - 24.0, 15.0, 92.0);
    }
    if max_scroll > 0 {
        let track_h = DETAIL_ROWS as f32 * DETAIL_ROW_H;
        let thumb_h = (track_h * DETAIL_ROWS as f32 / steps.len() as f32).max(24.0);
        let thumb_y = DETAIL_ROWS_TOP + (track_h - thumb_h) * scroll as f32 / max_scroll as f32;
        c.rrect((rx + rw - 10.0, DETAIL_ROWS_TOP, 4.0, track_h), 2.0, LINE, 1.0);
        c.rrect((rx + rw - 10.0, thumb_y, 4.0, thumb_h), 2.0, MUTED, 1.0);
    }
}

fn music(c: &mut Canvas, v: &View) {
    let focus = v.level == &Level::Focus;
    match v.art {
        Some(img) => c.image(img, ART, 14.0),
        None => {
            c.rrect(ART, 14.0, PANEL, 1.0);
            let (ax, ay, aw, ah) = ART;
            let (cx, cy) = (ax + aw / 2.0, ay + ah / 2.0);
            c.circle(cx - 30.0, cy + 46.0, 26.0, 0x2A2A31, 1.0);
            c.rect((cx - 8.0, cy - 70.0, 12.0, 118.0), 0x2A2A31, 1.0);
            c.poly(&[(cx - 8.0, cy - 70.0), (cx + 50.0, cy - 50.0), (cx + 50.0, cy - 30.0), (cx - 8.0, cy - 50.0)], 0x2A2A31);
        }
    }
    let iw = W - M - MX;
    let Some(t) = v.track else {
        c.text(F::Bold, "Nothing playing", 44.0, MX, CY + 120.0, TEXT_2, Align::Left);
        let msg = v.problem.unwrap_or("Start Spotify or Music and it will show up here.");
        c.paragraph(F::Regular, msg, 22.0, MX, CY + 166.0, iw, 3, 28.0, if v.problem.is_some() { AMBER } else { MUTED });
        return;
    };
    let state = if t.playing { "PLAYING" } else { "PAUSED" };
    let src = format!("{}  ·  {}", t.app.to_uppercase(), state);
    c.text(F::Bold, &src, 15.0, MX, CY + 34.0, if t.playing { SPOTIFY } else { MUTED }, Align::Left);
    c.fit(F::Bold, &t.title, 50.0, iw, MX, CY + 92.0, TEXT, Align::Left);
    c.fit(F::Medium, &t.artist, 32.0, iw, MX, CY + 138.0, 0xD4D4DC, Align::Left);
    c.fit(F::Regular, &t.album, 22.0, iw, MX, CY + 174.0, MUTED, Align::Left);

    // Progress
    let (px, py, pw) = (MX, CY + 200.0, iw - 24.0);
    c.rrect((px, py, pw, 8.0), 4.0, 0x2A2A31, 1.0);
    if t.duration > 0.0 {
        let frac = (v.position / t.duration).clamp(0.0, 1.0) as f32;
        c.rrect((px, py, (pw * frac).max(8.0), 8.0), 4.0, if t.playing { SPOTIFY } else { MUTED }, 1.0);
        c.text(F::Mono, &mmss(v.position), 16.0, px, py + 32.0, MUTED, Align::Left);
        c.text(F::Mono, &mmss(t.duration), 16.0, px + pw, py + 32.0, MUTED, Align::Right);
    }

    // Transport
    for (r, which) in [(prev_button(), 0), (play_button(), 1), (next_button(), 2)] {
        let highlight = focus && which == 1;
        c.rrect(r, 16.0, if highlight { 0x2E2E36 } else { 0x1C1C21 }, 1.0);
        if highlight {
            c.outline(r, 16.0, ORANGE, 2.0);
        }
        let (cx, cy) = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
        match which {
            0 => {
                c.rect((cx - 24.0, cy - 18.0, 6.0, 36.0), TEXT, 1.0);
                c.poly(&[(cx + 20.0, cy - 18.0), (cx + 20.0, cy + 18.0), (cx - 14.0, cy)], TEXT);
            }
            2 => {
                c.poly(&[(cx - 20.0, cy - 18.0), (cx - 20.0, cy + 18.0), (cx + 14.0, cy)], TEXT);
                c.rect((cx + 18.0, cy - 18.0, 6.0, 36.0), TEXT, 1.0);
            }
            _ if t.playing => {
                c.rect((cx - 16.0, cy - 22.0, 11.0, 44.0), TEXT, 1.0);
                c.rect((cx + 5.0, cy - 22.0, 11.0, 44.0), TEXT, 1.0);
            }
            _ => c.poly(&[(cx - 14.0, cy - 24.0), (cx - 14.0, cy + 24.0), (cx + 24.0, cy)], TEXT),
        }
    }

    // Volume
    let vb = volume_bar();
    let vol = v.volume.unwrap_or(0);
    let vol_label = format!("{} VOLUME", t.app.to_uppercase());
    c.text(F::Bold, &vol_label, 13.0, vb.0, vb.1 - 26.0, if focus { ORANGE } else { FAINT }, Align::Left);
    let vol_text = v.volume.map(|x| format!("{x}%")).unwrap_or_else(|| "—".into());
    c.text(F::Bold, &vol_text, 30.0, vb.0 + vb.2, vb.1 - 18.0, TEXT, Align::Right);
    c.rrect(vb, 7.0, 0x2A2A31, 1.0);
    c.rrect((vb.0, vb.1, (vb.2 * vol as f32 / 100.0).max(14.0), vb.3), 7.0, if focus { ORANGE } else { 0xC9C9D1 }, 1.0);
    if let Some(p) = v.problem {
        c.fit(F::Medium, p, 15.0, iw, MX, CY + CH - 2.0, AMBER, Align::Left);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages() {
        assert_eq!(age(30_000), "now");
        assert_eq!(age(4 * 60_000), "4m");
        assert_eq!(age(75 * 60_000), "1h15");
        assert_eq!(age_short(12_000), "12s");
        assert_eq!(mmss(86.4), "1:26");
    }

    #[test]
    fn overview_window_follows_selection() {
        assert_eq!(window(2, 0), (0, 3));
        assert_eq!(window(8, 0), (0, 5));
        assert_eq!(window(8, 6), (2, 5));
        assert_eq!(window(8, 7), (3, 5));
    }
}
