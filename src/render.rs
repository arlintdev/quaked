//! Draws the board. Everything is laid out on the Quake's native 1920x480 grid; touch
//! reports use the same coordinates, so hit testing shares the layout functions.
//!
//! Pages (the knob switches them at the top level): AI and Music.
//! Levels: Top (just looking) -> Focus (knob drives the page) -> Detail (one session).

use crate::audio;
use crate::config::{Action, PageId, Shortcut};
use crate::{digin, docker, github, league, ops};
use crate::music::Track;
use crate::status::{Session, State, Step, StepStatus};
use crate::symbols::Symbols;
use fontdue::{Font, FontSettings, Metrics};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use tiny_skia::{Color, FillRule, LineCap, LineJoin, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke, Transform};

// One file per feature (each draws its own page / overlay; they share this module's helpers).
mod calendar;
mod call;
mod deploy;
mod focus;
mod health;
mod night;
pub(crate) mod stocks;
mod paste;
mod viz;
mod windows;

pub const W: f32 = 1920.0;
pub const H: f32 = 480.0;
const M: f32 = 12.0; // outer margin
const G: f32 = 10.0; // gap between panels
const CY: f32 = 58.0; // content top (below the top bar)
const CH: f32 = H - CY - M; // content height
const DETAIL_LEFT_W: f32 = 520.0;
const DETAIL_ROW_H: f32 = 30.0;
const DETAIL_ROWS_TOP: f32 = CY + 46.0;
/// The status box under a session's tasks and steps.
const STATUS_H: f32 = 50.0;
pub const DETAIL_ROWS: usize = ((CH - STATUS_H - G - 56.0) / DETAIL_ROW_H) as usize;

pub type Page = PageId;

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
    Shortcut(usize),
    Clip(usize),
    StopAll,
    MicMute,
    LeagueAuto,
    LeagueAccept,
    LeagueDecline,
    LeagueFind,
    LeagueCancel,
    Container(usize),
    DockerAction(&'static str),
    DockerBack,
    Warp,
    Host(usize),
    Site(usize),
    /// Index into `github::Status::all()` (your PRs, then review requests).
    Pr(usize),
    /// A SETTINGS card; for the sliders, where along the bar it was touched.
    Setting(usize, Option<f32>),
    /// A dashboard tile: go to that page.
    Pane(Page),
    /// A card in Claude's question (options, then Done for multi-select, then "Answer in Claude").
    AskCard(usize),
    /// A Digin row: issues, then inbox, then pages (one index across the three lists). On an
    /// open item: its action `i`, or `DG_BACK`.
    DiginItem(usize),
    /// Item `usize` on a feature's page (or overlay: Page::Dash for call / night).
    Feature(Page, usize),
}

/// Claude's current question on the board.
pub struct AskView<'a> {
    pub session: &'a str,
    pub machine: &'a str,
    pub header: &'a str,
    pub question: &'a str,
    /// (label, description)
    pub options: &'a [(String, String)],
    pub picked: &'a [bool],
    pub multi: bool,
    pub sel: usize,
    /// Which question of how many (Claude can ask up to four at once).
    pub index: usize,
    pub count: usize,
    pub secs_left: u64,
    /// More questions waiting after this one (from other sessions).
    pub queued: usize,
    /// A permission prompt: (tag like "$" or "edit", what exactly, Claude's description).
    pub detail: Option<(&'a str, &'a str, &'a str)>,
}

/// Cards on screen: the options, Done (multi-select), then "Answer in Claude".
pub fn ask_cards(options: usize, multi: bool) -> usize {
    options + usize::from(multi) + 1
}

fn ask_card_rect(i: usize, n: usize) -> R {
    let top = 188.0;
    let w = (W - 2.0 * M - (n as f32 - 1.0) * G) / n as f32;
    (M + i as f32 * (w + G), top, w, H - top - 40.0)
}

/// What the SETTINGS page shows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SettingsView {
    /// Screen luminance, 10-255.
    pub screen: u8,
    pub ring: u8,
    pub idle: u64,
    pub idle_home: bool,
    pub ring_on: bool,
    pub alert: bool,
    pub blink: bool,
    pub jump: bool,
    pub buzzer: bool,
    pub mic: bool,
    pub auto_accept: bool,
    pub snap: bool,
}

pub struct View<'a> {
    pub pages: &'a [Page],
    pub page: Page,
    /// The current page's ring color, used as its accent on screen.
    pub accent: u32,
    pub shortcuts: &'a [Shortcut],
    pub sounds: &'a audio::Status,
    /// Selected tile on the Shortcuts / Sounds pages, or container on the Docker page.
    pub tile: usize,
    pub league: &'a league::Status,
    pub league_cfg: &'a crate::config::League,
    pub docker: &'a docker::Status,
    /// On OPS, `tile` 0 is the WARP toggle, then hosts, then sites. On CODE it walks
    /// your PRs, then review requests.
    pub ops: &'a ops::Status,
    pub code: &'a github::Status,
    pub digin: &'a digin::Status,
    pub settings: SettingsView,
    /// Page colors, for the dashboard tiles.
    pub ring: &'a crate::config::Ring,
    /// Recent samples for the dashboard's graphs.
    pub history: &'a History,
    /// A question from Claude, shown over everything.
    pub ask: Option<AskView<'a>>,
    // Features (see render/<feature>.rs and app/<feature>.rs).
    pub call: &'a crate::call::Status,
    pub focus: &'a crate::focus::Status,
    pub deploy: &'a crate::deploy::Status,
    pub paste: &'a crate::clipboard::Status,
    pub windows: &'a crate::windows::Status,
    pub calendar: &'a crate::calendar::Status,
    pub health: &'a crate::health::Status,
    pub stocks: &'a crate::stocks::Status,
    pub night: &'a crate::night::Status,
    pub viz: &'a crate::viz::Status,
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
    /// Something new: (why, its page's color, its page). Shown in the top bar.
    pub notice: Option<(&'a str, u32, Page)>,
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
            mono: load_font(&[("/System/Library/Fonts/SFNSMono.ttf", 0), ("/System/Library/Fonts/Menlo.ttc", 0)]),
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
    symbols: &'a Symbols,
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
    pub fn new(width: u32, height: u32, fonts: &'a Fonts, symbols: &'a Symbols) -> Canvas<'a> {
        let pixmap = Pixmap::new(width.max(1), height.max(1)).unwrap();
        let scale = (width as f32 / W).min(height as f32 / H);
        Canvas { pixmap, scale, fonts, symbols }
    }

    /// Draws an SF Symbol centered on (cx, cy), tinted. Returns false if the name is unknown.
    fn symbol(&mut self, name: &str, px: f32, cx: f32, cy: f32, color: u32) -> bool {
        let Some(mask) = self.symbols.get(name, px * self.scale) else { return false };
        let (cr, cg, cb) = ((color >> 16) as u8 as f32, (color >> 8) as u8 as f32, color as u8 as f32);
        let x0 = (cx * self.scale - mask.w as f32 / 2.0).round() as i32;
        let y0 = (cy * self.scale - mask.h as f32 / 2.0).round() as i32;
        let pw = self.pixmap.width() as i32;
        let ph = self.pixmap.height() as i32;
        let data = self.pixmap.data_mut();
        for y in 0..mask.h as i32 {
            let py = y0 + y;
            if py < 0 || py >= ph {
                continue;
            }
            for x in 0..mask.w as i32 {
                let px_ = x0 + x;
                if px_ < 0 || px_ >= pw {
                    continue;
                }
                let a = mask.alpha[(y * mask.w as i32 + x) as usize] as f32 / 255.0;
                if a <= 0.0 {
                    continue;
                }
                let i = ((py * pw + px_) * 4) as usize;
                data[i] = (cr * a + data[i] as f32 * (1.0 - a)) as u8;
                data[i + 1] = (cg * a + data[i + 1] as f32 * (1.0 - a)) as u8;
                data[i + 2] = (cb * a + data[i + 2] as f32 * (1.0 - a)) as u8;
            }
        }
        true
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

    fn poly_alpha(&mut self, pts: &[(f32, f32)], color: u32, alpha: f32) {
        let s = self.scale;
        let mut pb = PathBuilder::new();
        pb.move_to(pts[0].0 * s, pts[0].1 * s);
        for p in &pts[1..] {
            pb.line_to(p.0 * s, p.1 * s);
        }
        pb.close();
        if let Some(path) = pb.finish() {
            self.pixmap.fill_path(&path, &Self::paint(color, alpha), FillRule::Winding, Transform::identity(), None);
        }
    }

    fn polyline(&mut self, pts: &[(f32, f32)], color: u32, width: f32) {
        if pts.len() < 2 {
            return;
        }
        let s = self.scale;
        let mut pb = PathBuilder::new();
        pb.move_to(pts[0].0 * s, pts[0].1 * s);
        for p in &pts[1..] {
            pb.line_to(p.0 * s, p.1 * s);
        }
        if let Some(path) = pb.finish() {
            let stroke = Stroke { width: width * s, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
            self.pixmap.stroke_path(&path, &Self::paint(color, 1.0), &stroke, Transform::identity(), None);
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

/// Every tab is an icon; the current page's tab also says its name.
const TAB_ICON_W: f32 = 52.0;

/// Room the name takes in the current page's tab (Bold 15).
fn tab_label_w(p: Page) -> f32 {
    match p {
        Page::Ai => 22.0,
        Page::Ops | Page::Calendar => 40.0,
        Page::Deploy => 42.0,
        Page::Dash | Page::Code => 50.0,
        Page::Digin => 52.0,
        Page::Paste => 56.0,
        Page::Music | Page::Focus => 60.0,
        Page::League | Page::Health => 72.0,
        Page::Stocks => 72.0,
        Page::Soundboard | Page::Docker => 76.0,
        Page::Windows => 92.0,
        Page::Settings => 96.0,
        Page::Shortcuts => 110.0,
    }
}

fn tab_width(p: Page, active: Page) -> f32 {
    if p == active {
        TAB_ICON_W - 8.0 + tab_label_w(p) + 18.0
    } else {
        TAB_ICON_W
    }
}

fn tab_rect(pages: &[Page], active: Page, p: Page) -> R {
    let mut x = M;
    for q in pages {
        if *q == p {
            return (x, 9.0, tab_width(p, active), 34.0);
        }
        x += tab_width(*q, active) + 8.0;
    }
    (-1000.0, 0.0, 0.0, 0.0)
}

fn tabs_end(pages: &[Page], active: Page) -> f32 {
    M + pages.iter().map(|p| tab_width(*p, active) + 8.0).sum::<f32>()
}

// Shortcuts: 2 rows x 8 tiles.
pub const SC_COLS: usize = 8;
pub const SC_ROWS: usize = 2;
fn shortcut_rect(i: usize) -> R {
    let w = (W - 2.0 * M - (SC_COLS as f32 - 1.0) * G) / SC_COLS as f32;
    let h = (CH - G) / SC_ROWS as f32;
    let (r, c) = (i / SC_COLS, i % SC_COLS);
    (M + c as f32 * (w + G), CY + r as f32 * (h + G), w, h)
}

// Sounds: a status strip, then 2 rows x 6 clip tiles.
pub const CLIP_COLS: usize = 6;
pub const CLIP_ROWS: usize = 2;
const STRIP_H: f32 = 50.0;
fn stop_all_button() -> R {
    (W - M - 150.0, CY, 150.0, STRIP_H - 6.0)
}
fn mic_button() -> R {
    (W - M - 150.0 - 10.0 - 190.0, CY, 190.0, STRIP_H - 6.0)
}
fn clip_rect(slot: usize) -> R {
    let top = CY + STRIP_H + 6.0;
    let w = (W - 2.0 * M - (CLIP_COLS as f32 - 1.0) * G) / CLIP_COLS as f32;
    let h = (CY + CH - top - G) / CLIP_ROWS as f32;
    let (r, c) = (slot / CLIP_COLS, slot % CLIP_COLS);
    (M + c as f32 * (w + G), top + r as f32 * (h + G), w, h)
}
// League
const LEAGUE_LEFT_W: f32 = 430.0;
fn league_auto_button() -> R {
    (M + 24.0, CY + CH - 132.0, LEAGUE_LEFT_W - 48.0, 70.0)
}
const LEAGUE_DUOS_W: f32 = 560.0;
fn league_main() -> R {
    (M + LEAGUE_LEFT_W + G, CY, W - 2.0 * M - LEAGUE_LEFT_W - LEAGUE_DUOS_W - 2.0 * G, CH)
}
/// Your record with the people you play with.
fn league_duos() -> R {
    (W - M - LEAGUE_DUOS_W, CY, LEAGUE_DUOS_W, CH)
}
fn league_primary() -> R {
    let (x, y, _, h) = league_main();
    (x + 32.0, y + h - 100.0, 280.0, 72.0)
}
fn league_secondary() -> R {
    let (x, y, _, h) = league_main();
    (x + 32.0 + 296.0, y + h - 100.0, 240.0, 72.0)
}

// Docker
const DOCKER_STRIP_H: f32 = 48.0;
fn container_rect(slot: usize, cols: usize) -> R {
    let (x, _, w, _) = col_rect(slot, cols);
    (x, CY + DOCKER_STRIP_H + 6.0, w, CH - DOCKER_STRIP_H - 6.0)
}
fn docker_actions() -> [(R, &'static str); 3] {
    let y = CY + CH - 58.0;
    [((M + 24.0, y, 150.0, 42.0), "primary"), ((M + 184.0, y, 150.0, 42.0), "restart"), ((M + 344.0, y, 150.0, 42.0), "pause")]
}
fn docker_back() -> R {
    (M + 24.0, CY + CH - 112.0, 120.0, 42.0)
}
// Ops: network strip, host cards on the left, sites on the right.
const OPS_STRIP_H: f32 = 48.0;
const OPS_TOP: f32 = CY + OPS_STRIP_H + 6.0;
const SITES_W: f32 = 560.0;
const SITE_ROW_H: f32 = 50.0;
pub const SITE_ROWS: usize = ((CY + CH - OPS_TOP - 48.0) / SITE_ROW_H) as usize;
fn warp_button() -> R {
    (M, CY, 214.0, OPS_STRIP_H - 6.0)
}
fn host_rect(slot: usize, cols: usize) -> R {
    let area = W - 2.0 * M - SITES_W - G;
    let w = (area - (cols as f32 - 1.0) * G) / cols as f32;
    (M + slot as f32 * (w + G), OPS_TOP, w, CY + CH - OPS_TOP)
}
fn sites_panel() -> R {
    (W - M - SITES_W, OPS_TOP, SITES_W, CY + CH - OPS_TOP)
}
fn site_rect(slot: usize) -> R {
    let (x, y, w, _) = sites_panel();
    (x + 8.0, y + 40.0 + slot as f32 * SITE_ROW_H, w - 16.0, SITE_ROW_H)
}
/// First visible host and the number of card columns; hosts start at tile 1.
fn host_window(n: usize, tile: usize) -> (usize, usize) {
    let cols = n.clamp(3, 4);
    (list_window(n, tile.saturating_sub(1).min(n.saturating_sub(1)), cols), cols)
}
fn site_window(o: &ops::Status, tile: usize) -> usize {
    list_window(o.sites.len(), tile.saturating_sub(1 + o.hosts.len()), SITE_ROWS)
}

// Code: your PRs on the left, review requests on the right.
const CODE_STRIP_H: f32 = 48.0;
const CODE_TOP: f32 = CY + CODE_STRIP_H + 6.0;
const REVIEWS_W: f32 = 640.0;
const PR_ROW_H: f32 = 44.0;
pub const PR_ROWS: usize = ((CY + CH - CODE_TOP - 48.0) / PR_ROW_H) as usize;
fn mine_panel() -> R {
    (M, CODE_TOP, W - 2.0 * M - REVIEWS_W - G, CY + CH - CODE_TOP)
}
fn reviews_panel() -> R {
    (W - M - REVIEWS_W, CODE_TOP, REVIEWS_W, CY + CH - CODE_TOP)
}
fn pr_rect(panel: R, slot: usize) -> R {
    (panel.0 + 8.0, panel.1 + 40.0 + slot as f32 * PR_ROW_H, panel.2 - 16.0, PR_ROW_H)
}
/// (panel, first visible row, index offset into `all()`) for both lists.
fn pr_lists(g: &github::Status, tile: usize) -> [(R, usize, usize, usize); 2] {
    let m = g.mine.len();
    [
        (mine_panel(), list_window(m, tile.min(m.saturating_sub(1)), PR_ROWS), 0, m),
        (reviews_panel(), list_window(g.reviews.len(), tile.saturating_sub(m), PR_ROWS), m, g.reviews.len()),
    ]
}

// Dashboard: a tile per page (all but itself and Settings), in page order, two rows.
/// Shortcuts has no live status, so it gets no tile (Digin takes its place).
pub fn dash_pages(pages: &[Page]) -> Vec<Page> {
    pages.iter().copied().filter(|p| !matches!(p, Page::Dash | Page::Settings | Page::Shortcuts | Page::Focus | Page::Deploy | Page::Paste | Page::Windows | Page::Calendar | Page::Health | Page::Stocks)).collect()
}
fn dash_rect(i: usize, n: usize) -> R {
    let cols = n.div_ceil(2).max(1);
    let rows = if n > cols { 2 } else { 1 };
    let w = (W - 2.0 * M - (cols as f32 - 1.0) * G) / cols as f32;
    let h = (CH - (rows as f32 - 1.0) * G) / rows as f32;
    (M + (i % cols) as f32 * (w + G), CY + (i / cols) as f32 * (h + G), w, h)
}
pub fn page_symbol(p: Page) -> &'static str {
    match p {
        Page::Dash => "square.grid.2x2.fill",
        Page::Ai => "sparkles",
        Page::Music => "music.note",
        Page::Shortcuts => "square.grid.3x3.fill",
        Page::Soundboard => "waveform",
        Page::League => "gamecontroller.fill",
        Page::Docker => "shippingbox.fill",
        Page::Ops => "server.rack",
        Page::Code => "arrow.triangle.pull",
        Page::Digin => "bubble.left.and.text.bubble.right.fill",
        Page::Focus => "timer",
        Page::Deploy => "paperplane.fill",
        Page::Paste => "doc.on.clipboard.fill",
        Page::Windows => "macwindow.on.rectangle",
        Page::Calendar => "calendar",
        Page::Health => "waveform.path.ecg",
        Page::Stocks => "chart.line.uptrend.xyaxis",
        Page::Settings => "gearshape.fill",
    }
}

// Settings: three sliders on top, nine switches below.
pub const SETTING_COUNT: usize = 12;
pub const SETTING_SLIDERS: usize = 3;
pub const SCREEN_MIN: u8 = 10;
/// Idle timeout choices, in seconds.
pub const IDLE_STEPS: [u64; 9] = [15, 30, 45, 60, 90, 120, 180, 300, 600];
const SET_ROW_H: f32 = (CH - G) / 2.0;
fn setting_rect(i: usize) -> R {
    if i < SETTING_SLIDERS {
        let w = (W - 2.0 * M - 2.0 * G) / 3.0;
        (M + i as f32 * (w + G), CY, w, SET_ROW_H)
    } else {
        let j = (i - SETTING_SLIDERS) as f32;
        let w = (W - 2.0 * M - 8.0 * G) / 9.0;
        (M + j * (w + G), CY + SET_ROW_H + G, w, SET_ROW_H)
    }
}
fn setting_bar(i: usize) -> R {
    let (x, y, w, h) = setting_rect(i);
    (x + 28.0, y + h - 44.0, w - 56.0, 14.0)
}
/// Touching the lower part of a slider card sets the value there.
fn setting_bar_hit(i: usize) -> R {
    let (x, y, w, h) = setting_rect(i);
    (x, y + h - 86.0, w, 86.0)
}
pub fn idle_index(secs: u64) -> usize {
    IDLE_STEPS.iter().enumerate().min_by_key(|(_, s)| s.abs_diff(secs)).map(|(i, _)| i).unwrap_or(2)
}
pub fn setting_name(i: usize) -> &'static str {
    ["Screen brightness", "Ring brightness", "Idle after", "Home when idle", "Ring light", "Alert pulse", "Blink on new", "Jump to new", "Buzzer", "Quake mic", "Auto-accept", "Snap to League"].get(i).copied().unwrap_or("")
}

// Digin: accounts and chat summary on a strip, then issues | inbox | pages.
const DG_STRIP_H: f32 = 44.0;
const DG_TOP: f32 = CY + DG_STRIP_H + 6.0;
const DG_ROW_H: f32 = 44.0;
pub const DG_ROWS: usize = ((CY + CH - DG_TOP - 44.0) / DG_ROW_H) as usize;
fn digin_cols() -> [R; 3] {
    let total = W - 2.0 * M - 2.0 * G;
    let (a, b) = (total * 0.38, total * 0.36);
    let h = CY + CH - DG_TOP;
    [(M, DG_TOP, a, h), (M + a + G, DG_TOP, b, h), (M + a + b + 2.0 * G, DG_TOP, total - a - b, h)]
}
fn digin_row(col: R, slot: usize) -> R {
    (col.0 + 8.0, col.1 + 40.0 + slot as f32 * DG_ROW_H, col.2 - 16.0, DG_ROW_H)
}
/// (column, first visible row, offset into the one index, length) for each list.
fn digin_lists(d: &digin::Status, tile: usize) -> [(R, usize, usize, usize); 3] {
    let [ci, cn, cp] = digin_cols();
    let (ni, nn, np) = (d.issues.len(), d.inbox.len(), d.pages.len());
    let local = |offset: usize, n: usize| tile.saturating_sub(offset).min(n.saturating_sub(1));
    [
        (ci, list_window(ni, local(0, ni), DG_ROWS), 0, ni),
        (cn, list_window(nn, local(ni, nn), DG_ROWS), ni, nn),
        (cp, list_window(np, local(ni + nn, np), DG_ROWS), ni + nn, np),
    ]
}
// An open Digin item: what it is on the left, its text on the right, its actions along the
// bottom ("‹ Back" for touch, then the issue's statuses or "Mark read", "Open in Digin" last).
const DG_BAR_H: f32 = 60.0;
const DG_BAR_Y: f32 = CY + CH - DG_BAR_H;
const DG_HEAD_W: f32 = 560.0;
const DG_OPEN_W: f32 = 236.0;
/// `Hit::DiginItem` on an open item: action `i`, or this for "‹ Back".
pub const DG_BACK: usize = usize::MAX;
fn dg_head() -> R {
    (M, CY, DG_HEAD_W, DG_BAR_Y - G - CY)
}
fn dg_text() -> R {
    (M + DG_HEAD_W + G, CY, W - 2.0 * M - DG_HEAD_W - G, DG_BAR_Y - G - CY)
}
/// Where the text itself goes inside its panel.
fn dg_text_area() -> R {
    let (x, y, w, h) = dg_text();
    (x + 26.0, y + 48.0, w - 64.0, h - 48.0 - 16.0)
}
fn dg_back() -> R {
    (M, DG_BAR_Y + 6.0, 112.0, DG_BAR_H - 6.0)
}
/// The action bar: one rect per action, "Open in Digin" (always last) at the far right.
fn dg_bar(n: usize) -> Vec<R> {
    let (y, h) = (DG_BAR_Y + 6.0, DG_BAR_H - 6.0);
    let open_x = W - M - DG_OPEN_W;
    let others = n.saturating_sub(1);
    let left = M + 112.0 + 2.0 * G;
    let room = open_x - 3.0 * G - left;
    let w = if others == 0 { 0.0 } else { ((room - (others as f32 - 1.0) * G) / others as f32).min(250.0) };
    let mut out: Vec<R> = (0..others).map(|i| (left + i as f32 * (w + G), y, w, h)).collect();
    if n > 0 {
        out.push((open_x, y, DG_OPEN_W, h));
    }
    out
}
/// The open item, if the board is showing it.
fn dg_open<'a>(v: &'a View) -> Option<&'a digin::Detail> {
    match v.level {
        Level::Detail(key) => v.digin.detail.as_ref().filter(|d| &d.key == key),
        _ => None,
    }
}

/// The link for a Digin row in the one index.
pub fn digin_url(d: &digin::Status, i: usize) -> Option<String> {
    let (ni, nn) = (d.issues.len(), d.inbox.len());
    if i < ni {
        d.issues.get(i).map(|x| x.url.clone())
    } else if i < ni + nn {
        d.inbox.get(i - ni).map(|x| x.url.clone())
    } else {
        d.pages.get(i - ni - nn).map(|x| x.url.clone())
    }
}

/// First row of a scrolling list so that `sel` stays visible.
fn list_window(n: usize, sel: usize, rows: usize) -> usize {
    if n <= rows {
        0
    } else {
        sel.saturating_sub(rows - 1).min(n - rows)
    }
}

pub const LOG_ROW_H: f32 = 21.0;
pub const LOG_ROWS: usize = ((CH - 56.0) / LOG_ROW_H) as usize;

/// What the Docker detail buttons do for a container in this state.
pub fn docker_button_action(state: &str, which: &str) -> &'static str {
    match which {
        "primary" => if state == "running" || state == "paused" || state == "restarting" { "stop" } else { "start" },
        "restart" => "restart",
        _ => if state == "paused" { "unpause" } else { "pause" },
    }
}

/// First clip shown, in whole rows, so that `sel` is visible.
pub fn clip_window(n: usize, sel: usize) -> usize {
    let per = CLIP_COLS * CLIP_ROWS;
    if n <= per {
        return 0;
    }
    let row = sel / CLIP_COLS;
    let first_row = row.saturating_sub(CLIP_ROWS - 1);
    (first_row * CLIP_COLS).min(n.div_ceil(CLIP_COLS).saturating_sub(CLIP_ROWS) * CLIP_COLS)
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
    (M + DETAIL_LEFT_W + G, CY, W - 2.0 * M - DETAIL_LEFT_W - G, CH - STATUS_H - G)
}

fn detail_status() -> R {
    (M + DETAIL_LEFT_W + G, CY + CH - STATUS_H, W - 2.0 * M - DETAIL_LEFT_W - G, STATUS_H)
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
    if let Some(a) = &v.ask {
        let n = ask_cards(a.options.len(), a.multi);
        return (0..n).find(|&i| inside(ask_card_rect(i, n), x, y)).map(Hit::AskCard);
    }
    if v.call.active {
        return call::hit(v, x, y);
    }
    if v.night.active {
        return night::hit(v, x, y);
    }
    for p in v.pages {
        if inside(tab_rect(v.pages, v.page, *p), x, y) {
            return Some(Hit::Tab(*p));
        }
    }
    match (v.page, v.level) {
        (Page::League, _) => {
            let l = v.league;
            if inside(league_auto_button(), x, y) {
                return Some(Hit::LeagueAuto);
            }
            match l.phase.as_str() {
                "ReadyCheck" if inside(league_primary(), x, y) => Some(Hit::LeagueAccept),
                "ReadyCheck" if inside(league_secondary(), x, y) => Some(Hit::LeagueDecline),
                "Lobby" if inside(league_primary(), x, y) => Some(Hit::LeagueFind),
                "Matchmaking" if inside(league_primary(), x, y) => Some(Hit::LeagueCancel),
                _ => None,
            }
        }
        (Page::Docker, Level::Detail(id)) => {
            if inside(docker_back(), x, y) {
                return Some(Hit::DockerBack);
            }
            let state = v.docker.containers.iter().find(|c| &c.id == id).map(|c| c.state.as_str()).unwrap_or("");
            docker_actions().into_iter().find(|(r, _)| inside(*r, x, y)).map(|(_, which)| Hit::DockerAction(docker_button_action(state, which)))
        }
        (Page::Docker, _) => {
            let n = v.docker.containers.len();
            let (first, cols) = window(n, v.tile);
            (0..cols).find(|&slot| inside(container_rect(slot, cols), x, y)).map(|slot| first + slot).filter(|&i| i < n).map(Hit::Container)
        }
        (Page::Ops, _) => {
            if inside(warp_button(), x, y) {
                return Some(Hit::Warp);
            }
            let o = v.ops;
            let (first, cols) = host_window(o.hosts.len(), v.tile);
            if let Some(i) = (0..cols).find(|&s| inside(host_rect(s, cols), x, y)).map(|s| first + s).filter(|&i| i < o.hosts.len()) {
                return Some(Hit::Host(i));
            }
            let first = site_window(o, v.tile);
            (0..SITE_ROWS).find(|&s| inside(site_rect(s), x, y)).map(|s| first + s).filter(|&i| i < o.sites.len()).map(Hit::Site)
        }
        (Page::Code, _) => pr_lists(v.code, v.tile).into_iter().find_map(|(panel, first, offset, n)| {
            (0..PR_ROWS).find(|&s| inside(pr_rect(panel, s), x, y)).map(|s| first + s).filter(|&i| i < n).map(|i| Hit::Pr(offset + i))
        }),
        (Page::Focus, _) => focus::hit(v, x, y),
        (Page::Deploy, _) => deploy::hit(v, x, y),
        (Page::Paste, _) => paste::hit(v, x, y),
        (Page::Windows, _) => windows::hit(v, x, y),
        (Page::Calendar, _) => calendar::hit(v, x, y),
        (Page::Health, _) => health::hit(v, x, y),
        (Page::Stocks, _) => stocks::hit(v, x, y),
        (Page::Digin, Level::Detail(_)) if dg_open(v).is_some() => {
            let d = dg_open(v)?;
            if inside(dg_back(), x, y) {
                return Some(Hit::DiginItem(DG_BACK));
            }
            dg_bar(d.body.actions().len()).into_iter().position(|r| inside(r, x, y)).map(Hit::DiginItem)
        }
        (Page::Digin, _) => digin_lists(v.digin, v.tile).into_iter().find_map(|(col, first, offset, n)| {
            (0..DG_ROWS).find(|&s| inside(digin_row(col, s), x, y)).map(|s| first + s).filter(|&i| i < n).map(|i| Hit::DiginItem(offset + i))
        }),
        (Page::Dash, _) => {
            let tiles = dash_pages(v.pages);
            (0..tiles.len()).find(|&i| inside(dash_rect(i, tiles.len()), x, y)).map(|i| Hit::Pane(tiles[i]))
        }
        (Page::Settings, _) => (0..SETTING_COUNT).find(|&i| inside(setting_rect(i), x, y)).map(|i| {
            let frac = (i < SETTING_SLIDERS && inside(setting_bar_hit(i), x, y)).then(|| {
                let (bx, _, bw, _) = setting_bar(i);
                ((x - bx) / bw).clamp(0.0, 1.0)
            });
            Hit::Setting(i, frac)
        }),
        (Page::Shortcuts, _) => (0..v.shortcuts.len().min(SC_COLS * SC_ROWS)).find(|&i| inside(shortcut_rect(i), x, y)).map(Hit::Shortcut),
        (Page::Soundboard, _) => {
            if inside(stop_all_button(), x, y) {
                return Some(Hit::StopAll);
            }
            if inside(mic_button(), x, y) {
                return Some(Hit::MicMute);
            }
            let first = clip_window(v.sounds.clips.len(), v.tile);
            (0..CLIP_COLS * CLIP_ROWS).find(|&s| inside(clip_rect(s), x, y)).map(|s| first + s).filter(|&i| i < v.sounds.clips.len()).map(Hit::Clip)
        }
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
        Hit::Tab(p) => tab_rect(v.pages, v.page, p),
        Hit::Shortcut(i) => shortcut_rect(i),
        Hit::Clip(i) => clip_rect(i.checked_sub(clip_window(v.sounds.clips.len(), v.tile))?),
        Hit::StopAll => stop_all_button(),
        Hit::MicMute => mic_button(),
        Hit::LeagueAuto => league_auto_button(),
        Hit::LeagueAccept | Hit::LeagueFind | Hit::LeagueCancel => league_primary(),
        Hit::LeagueDecline => league_secondary(),
        Hit::Container(i) => {
            let (first, cols) = window(v.docker.containers.len(), v.tile);
            container_rect(i.checked_sub(first)?, cols)
        }
        Hit::DockerAction(_) => return None,
        Hit::DockerBack => docker_back(),
        Hit::Warp => warp_button(),
        Hit::Host(i) => {
            let (first, cols) = host_window(v.ops.hosts.len(), v.tile);
            host_rect(i.checked_sub(first)?, cols)
        }
        Hit::Site(i) => site_rect(i.checked_sub(site_window(v.ops, v.tile))?),
        Hit::Pr(i) => {
            let (panel, first, offset, n) = pr_lists(v.code, v.tile).into_iter().find(|(_, _, off, n)| i >= *off && i < off + n)?;
            let _ = n;
            pr_rect(panel, (i - offset).checked_sub(first)?)
        }
        Hit::Setting(i, _) => setting_rect(i),
        Hit::Feature(p, i) => return feature_rect(v, p, i),
        Hit::DiginItem(i) if dg_open(v).is_some() => match i {
            DG_BACK => dg_back(),
            _ => *dg_bar(dg_open(v)?.body.actions().len()).get(i)?,
        },
        Hit::DiginItem(i) => {
            let (col, first, offset, _) = digin_lists(v.digin, v.tile).into_iter().find(|(_, _, off, n)| i >= *off && i < off + n)?;
            digin_row(col, (i - offset).checked_sub(first)?)
        }
        Hit::AskCard(i) => {
            let a = v.ask.as_ref()?;
            ask_card_rect(i, ask_cards(a.options.len(), a.multi))
        }
        Hit::Pane(p) => {
            let tiles = dash_pages(v.pages);
            dash_rect(tiles.iter().position(|t| *t == p)?, tiles.len())
        }
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

/// Where a feature's item is (for the tap flash).
fn feature_rect(v: &View, p: Page, i: usize) -> Option<R> {
    if v.call.active {
        return call::rect(v, i);
    }
    if v.night.active {
        return night::rect(v, i);
    }
    match p {
        Page::Focus => focus::rect(v, i),
        Page::Deploy => deploy::rect(v, i),
        Page::Paste => paste::rect(v, i),
        Page::Windows => windows::rect(v, i),
        Page::Calendar => calendar::rect(v, i),
        Page::Health => health::rect(v, i),
        Page::Stocks => stocks::rect(v, i),
        _ => None,
    }
}

// ---------------------------------------------------------------- drawing

pub fn draw(c: &mut Canvas, v: &View) {
    c.pixmap.fill(rgb(BG));
    if let Some(a) = &v.ask {
        ask_overlay(c, v, a);
        if let Some(r) = v.flash.and_then(|h| hit_rect(v, h)) {
            c.rrect(r, 16.0, 0xFFFFFF, 0.22);
        }
        return;
    }
    if v.call.active {
        call::draw(c, v);
        if let Some(r) = v.flash.and_then(|h| hit_rect(v, h)) {
            c.rrect(r, 16.0, 0xFFFFFF, 0.22);
        }
        return;
    }
    if v.night.active {
        night::draw(c, v);
        return;
    }
    top_bar(c, v);
    match (v.page, v.level) {
        (Page::Ai, Level::Detail(id)) => match v.sessions.iter().find(|s| &s.id == id) {
            Some(s) => ai_detail(c, v, s),
            None => ai_overview(c, v),
        },
        (Page::Ai, _) => ai_overview(c, v),
        (Page::Music, _) => music(c, v),
        (Page::Shortcuts, _) => shortcuts(c, v),
        (Page::Soundboard, _) => soundboard(c, v),
        (Page::League, _) => league_page(c, v),
        (Page::Docker, Level::Detail(id)) => match v.docker.containers.iter().find(|ct| &ct.id == id) {
            Some(ct) => docker_detail(c, v, ct),
            None => docker_page(c, v),
        },
        (Page::Docker, _) => docker_page(c, v),
        (Page::Ops, _) => ops_page(c, v),
        (Page::Code, _) => code_page(c, v),
        (Page::Settings, _) => settings_page(c, v),
        (Page::Dash, _) => dash_page(c, v),
        (Page::Digin, _) => digin_page(c, v),
        (Page::Focus, _) => focus::draw(c, v),
        (Page::Deploy, _) => deploy::draw(c, v),
        (Page::Paste, _) => paste::draw(c, v),
        (Page::Windows, _) => windows::draw(c, v),
        (Page::Calendar, _) => calendar::draw(c, v),
        (Page::Health, _) => health::draw(c, v),
        (Page::Stocks, _) => stocks::draw(c, v),
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
        (Page::Shortcuts, Level::Top) => "turn  switch page   ·   press  pick a shortcut   ·   tap  run",
        (Page::Shortcuts, _) => "turn  choose   ·   press  run   ·   double-click  back",
        (Page::Soundboard, Level::Top) => "turn  switch page   ·   press  pick a clip   ·   tap  play",
        (Page::Soundboard, _) => "turn  choose   ·   press  play / stop   ·   double-click  back",
        (Page::League, Level::Top) => "turn  switch page   ·   press  controls",
        (Page::League, _) => "press  accept / find / cancel   ·   double-click  back",
        (Page::Docker, Level::Top) => "turn  switch page   ·   press  pick a container",
        (Page::Docker, Level::Focus) => "turn  choose   ·   press  open   ·   double-click  back",
        (Page::Docker, Level::Detail(_)) => "turn  scroll logs   ·   press  refresh   ·   double-click  back",
        (Page::Ops, Level::Top) => "turn  switch page   ·   press  pick a host or site   ·   tap  open",
        (Page::Ops, _) => "turn  choose   ·   press  open / toggle   ·   double-click  back",
        (Page::Code, Level::Top) => "turn  switch page   ·   press  pick a pull request   ·   tap  open",
        (Page::Code, _) => "turn  choose   ·   press  open in browser   ·   double-click  back",
        (Page::Focus, l) => focus::hints(l),
        (Page::Deploy, l) => deploy::hints(l),
        (Page::Paste, l) => paste::hints(l),
        (Page::Windows, l) => windows::hints(l),
        (Page::Calendar, l) => calendar::hints(l),
        (Page::Health, l) => health::hints(l),
        (Page::Stocks, l) => stocks::hints(l),
        (Page::Digin, Level::Top) => "turn  switch page   ·   press  pick an item   ·   tap  open",
        (Page::Digin, Level::Detail(_)) => digin_hints(v),
        (Page::Digin, _) => "turn  choose   ·   press  open   ·   double-click  back",
        (Page::Dash, Level::Top) => "turn  switch page   ·   press  pick a pane   ·   tap  open",
        (Page::Dash, _) => "turn  choose   ·   press  open   ·   double-click  back",
        (Page::Settings, Level::Top) => "turn  switch page   ·   press  change settings",
        (Page::Settings, Level::Focus) => "turn  choose   ·   press  adjust / switch   ·   double-click  back",
        (Page::Settings, Level::Detail(_)) => "turn  change   ·   press  done",
    }
}

fn top_bar(c: &mut Canvas, v: &View) {
    let waiting = v.sessions.iter().filter(|s| s.state == State::Waiting).count();
    for p in v.pages {
        let p = *p;
        let r = tab_rect(v.pages, v.page, p);
        let active = v.page == p;
        c.rrect(r, 17.0, if active { TEXT } else { 0x1C1C21 }, 1.0);
        let fg = if active { 0x111114 } else { MUTED };
        if active {
            c.symbol(page_symbol(p), 18.0, r.0 + 24.0, r.1 + r.3 / 2.0, fg);
            c.fit(F::Bold, p.label(), 15.0, tab_label_w(p) + 6.0, r.0 + 42.0, r.1 + 23.0, fg, Align::Left);
        } else {
            c.symbol(page_symbol(p), 18.0, r.0 + r.2 / 2.0, r.1 + r.3 / 2.0, fg);
        }
        if active {
            c.rrect((r.0 + 14.0, r.1 + r.3 + 3.0, r.2 - 28.0, 3.0), 1.5, v.accent, 1.0);
        }
        if active && v.level != &Level::Top {
            c.outline((r.0 - 3.0, r.1 - 3.0, r.2 + 6.0, r.3 + 6.0), 20.0, v.accent, 2.0);
        }
    }
    if waiting > 0 && v.pages.contains(&Page::Ai) {
        let r = tab_rect(v.pages, v.page, Page::Ai);
        c.circle(r.0 + r.2 - 4.0, r.1 + 4.0, 11.0, AMBER, 1.0);
        c.text(F::Bold, &waiting.to_string(), 14.0, r.0 + r.2 - 4.0, r.1 + 9.0, 0x1C1500, Align::Center);
    }

    // Breadcrumb
    let crumb_x = tabs_end(v.pages, v.page) + 12.0;
    let mut x = crumb_x;
    let crumb = match (v.page, v.level) {
        (Page::Ai, Level::Detail(id)) => v.sessions.iter().find(|s| &s.id == id).map(|s| format!("›  {}", s.name)),
        (Page::Ai, Level::Focus) => v.sessions.get(v.sel).map(|s| format!("›  choose  ·  {}", s.name)),
        (Page::Music, Level::Focus) => Some("›  controls".into()),
        (Page::Shortcuts, Level::Focus) => v.shortcuts.get(v.tile).map(|s| format!("›  {}", s.title)),
        (Page::Soundboard, Level::Focus) => v.sounds.clips.get(v.tile).map(|s| format!("›  {}", s.title)),
        (Page::Docker, Level::Focus) => v.docker.containers.get(v.tile).map(|c| format!("›  choose  ·  {}", c.name)),
        (Page::Docker, Level::Detail(id)) => v.docker.containers.iter().find(|c| &c.id == id).map(|c| format!("›  {}", c.name)),
        (Page::Ops, Level::Focus) => ops_item_name(v.ops, v.tile).map(|n| format!("›  {n}")),
        (Page::Code, Level::Focus) => v.code.all().get(v.tile).map(|p| format!("›  {}", pr_ref(p))),
        (Page::Dash, Level::Focus) => dash_pages(v.pages).get(v.tile).map(|p| format!("›  {}", p.label())),
        (Page::Digin, Level::Detail(_)) => dg_open(v).map(|d| match &d.body {
            digin::Body::Issue(x) => format!("›  {}", x.key),
            digin::Body::Note(_) => "›  inbox".to_string(),
            digin::Body::Page(p) => format!("›  {}", p.title),
        }),
        (Page::Digin, Level::Focus) => {
            let d = v.digin;
            let (ni, nn) = (d.issues.len(), d.inbox.len());
            if v.tile < ni {
                d.issues.get(v.tile).map(|x| format!("›  {}", x.key))
            } else if v.tile < ni + nn {
                Some("›  inbox".to_string())
            } else {
                d.pages.get(v.tile - ni - nn).map(|p| format!("›  {}", p.title))
            }
        }
        (Page::Focus, _) => focus::crumb(v),
        (Page::Deploy, _) => deploy::crumb(v),
        (Page::Paste, _) => paste::crumb(v),
        (Page::Windows, _) => windows::crumb(v),
        (Page::Calendar, _) => calendar::crumb(v),
        (Page::Health, _) => health::crumb(v),
        (Page::Stocks, _) => stocks::crumb(v),
        (Page::Settings, Level::Focus) => Some(format!("›  {}", setting_name(v.tile))),
        (Page::Settings, Level::Detail(_)) => Some(format!("›  {}", setting_name(v.tile))),
        _ => None,
    };
    // Right side first (clock, counts, now playing), so the hints know how much room is left.
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
    // Keep at least ~420px for the breadcrumb and hints.
    let room_for_track = rx - (crumb_x + 420.0);
    if v.page != Page::Music && v.notice.is_none() && room_for_track > 140.0 {
        if let Some(t) = v.track.filter(|t| t.playing) {
            let label = format!("{} — {}", t.title, t.artist);
            let max = room_for_track.min(320.0);
            let w = c.measure(F::Medium, &label, 15.0).min(max);
            c.fit(F::Medium, &label, 15.0, max, rx - w, 32.0, MUTED, Align::Left);
            let ix = rx - w - 18.0;
            c.poly(&[(ix, 21.0), (ix + 10.0, 27.0), (ix, 33.0)], SPOTIFY);
            rx = ix - 16.0;
        }
    }
    let limit = rx - 16.0;

    if let Some(cb) = crumb {
        x += c.fit(F::Bold, &cb, 20.0, (limit - x).clamp(80.0, 360.0), x, 33.0, TEXT, Align::Left) + 24.0;
    }
    if let Some((why, color, page)) = v.notice {
        let r = tab_rect(v.pages, v.page, page);
        c.circle(r.0 + r.2 - 4.0, r.1 + 4.0, 7.0, color, 1.0);
        let nx = x.max(crumb_x);
        let pw = c.pill(nx, 15.0, "NEW", 13.0, color, 0x101013);
        c.fit(F::Bold, why, 17.0, (limit - nx - pw - 12.0).max(40.0), nx + pw + 12.0, 33.0, TEXT, Align::Left);
        c.rect((M, CY - 6.0, W - 2.0 * M, 1.0), LINE, 1.0);
        return;
    }
    let mut hx = x.max(crumb_x);
    for (i, part) in hints(v).split("   ·   ").enumerate() {
        let (key, rest) = part.split_once("  ").unwrap_or((part, ""));
        let need = if i > 0 { 26.0 } else { 0.0 } + c.measure(F::Bold, key, 15.0) + 6.0 + c.measure(F::Medium, rest, 15.0);
        if hx + need > limit {
            break;
        }
        if i > 0 {
            hx += c.text(F::Medium, "·", 15.0, hx + 10.0, 32.0, FAINT, Align::Left) + 20.0;
        }
        hx += c.text(F::Bold, key, 15.0, hx, 32.0, MUTED, Align::Left) + 6.0;
        hx += c.text(F::Medium, rest, 15.0, hx, 32.0, FAINT, Align::Left);
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
    let done = s.todos.iter().filter(|t| t.state == crate::status::TodoState::Completed).count();
    let steps = if !s.todos.is_empty() {
        format!("{done} of {} done", s.todos.len())
    } else if s.step_count == 1 {
        "1 step".to_string()
    } else {
        format!("{} steps", s.step_count)
    };
    let sw = c.text(F::Medium, &steps, 15.0, ix + pw + 10.0, y + 66.0, MUTED, Align::Left);
    if !s.machine.is_empty() {
        // Synced from another Mac
        let mx = ix + pw + sw + 22.0;
        c.symbol("laptopcomputer", 15.0, mx + 9.0, y + 61.0, 0x8AB4F8);
        c.fit(F::Medium, &s.machine, 15.0, (x + w - 16.0 - mx - 24.0).max(30.0), mx + 24.0, y + 66.0, 0x8AB4F8, Align::Left);
    }

    let mut cy = y + 98.0;
    let prompt = crate::status::typed_prompt(&s.prompt);
    if !prompt.is_empty() {
        c.text(F::Bold, "›", 18.0, ix, cy, FAINT, Align::Left);
        cy += c.paragraph(F::Regular, &prompt, 17.0, ix + 14.0, cy, iw - 14.0, 2, 22.0, TEXT_2);
    }
    if waiting && !s.notice.is_empty() {
        cy += 6.0;
        cy += c.paragraph(F::Bold, &s.notice, 17.0, ix, cy, iw, 2, 22.0, AMBER);
    } else if s.state == State::Working && !s.comment.is_empty() {
        // What Claude is saying as it works.
        cy += 6.0;
        c.symbol("sparkle", 13.0, ix + 6.0, cy - 6.0, ORANGE);
        cy += c.paragraph(F::Medium, &s.comment, 16.0, ix + 18.0, cy, iw - 18.0, 3, 21.0, 0xF0C9B2);
    } else if matches!(s.state, State::Done | State::Idle) && !s.last_message.is_empty() {
        cy += 6.0;
        cy += c.paragraph(F::Regular, &s.last_message, 16.0, ix, cy, iw, 3, 21.0, 0x9FD9B5);
    }
    cy += 4.0;
    c.rect((ix, cy - 10.0, iw, 1.0), LINE, 1.0);
    cy += 14.0;
    let line_h = 24.0;
    // While it works, the status line sits in its own strip at the bottom of the card.
    let strip = (s.state == State::Working).then(|| (x + 14.0, y + h - 48.0, w - 26.0, 38.0));
    if let Some(r) = strip {
        c.rrect(r, 10.0, PANEL_DEEP, 1.0);
        activity_line(c, v, s, r.0 + 12.0, r.1 + r.3 / 2.0 + 6.0, r.2 - 24.0);
    }
    let bottom = strip.map_or(y + h - 12.0, |r| r.1 - 8.0);
    let mut room = ((bottom - cy) / line_h).floor().max(0.0) as usize;
    if !s.todos.is_empty() {
        // Claude's task list: from just before the item in progress, so what's next shows.
        let shown = room.saturating_sub(1).max(1).min(s.todos.len());
        let first_open = s.todos.iter().position(|t| t.state != crate::status::TodoState::Completed).unwrap_or(s.todos.len());
        let start = first_open.saturating_sub(1).min(s.todos.len() - shown);
        for t in s.todos.iter().skip(start).take(shown) {
            todo_line(c, t, ix, cy, iw, s.state == State::Working);
            cy += line_h;
        }
        room = room.saturating_sub(shown);
        // The command running now, under the list.
        if room > 0 {
            if let Some(step) = s.steps.iter().rev().find(|st| st.status == StepStatus::Running) {
                step_line(c, v, step, ix - 4.0, cy, iw + 4.0, 14.0, 76.0);
            }
        }
        return;
    }
    for step in s.steps.iter().rev().filter(|st| st.status != StepStatus::Info).take(room) {
        step_line(c, v, step, ix - 4.0, cy, iw + 4.0, 14.0, 76.0);
        cy += line_h;
    }
}

/// What a working session is doing, the way the desktop app puts it ("Running a command"),
/// and how long this turn has run ("6m 55s").
fn activity(s: &Session, now: u64) -> Option<(String, String)> {
    if s.state != State::Working {
        return None;
    }
    let verb = match s.steps.iter().rev().find(|st| st.status == StepStatus::Running).map(|st| st.tag.as_str()) {
        Some("$") => "Running a command".to_string(),
        Some("edit") => "Editing a file".into(),
        Some("write") => "Writing a file".into(),
        Some("read") => "Reading a file".into(),
        Some("grep") | Some("glob") => "Searching the code".into(),
        Some("web") => "Reading a web page".into(),
        Some("search") => "Searching the web".into(),
        Some("agent") => "Running an agent".into(),
        Some("skill") => "Using a skill".into(),
        Some(t) => format!("Using {t}"),
        None => "Thinking".into(),
    };
    let turn_start = s.steps.iter().rev().find(|st| st.tag == "you").map(|st| st.ts).unwrap_or(s.since);
    Some((verb, turn_time(now.saturating_sub(turn_start))))
}

/// "45s", "6m 55s", "1h 04m".
fn turn_time(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// The activity line: three dots that pulse in turn, what it's doing, the turn's time.
fn activity_line(c: &mut Canvas, v: &View, s: &Session, x: f32, y: f32, w: f32) {
    let Some((verb, time)) = activity(s, v.now_ms) else { return };
    let beat = (v.now_ms / 400 % 3) as usize;
    for k in 0..3 {
        let (dx, dy) = [(0.0, -4.0), (6.0, -4.0), (3.0, 2.0)][k];
        c.circle(x + 6.0 + dx, y - 6.0 + dy, 2.4, ORANGE, if k == beat { 1.0 } else { 0.45 });
    }
    let vw = c.clip(F::Medium, &verb, 16.0, w - 120.0, x + 24.0, y, TEXT_2);
    c.text(F::Medium, &time, 16.0, x + 24.0 + vw + 10.0, y, MUTED, Align::Left);
}

/// The status box: what the session is doing now and for how long (with the command it's
/// running), what it needs from you, or when it finished.
fn status_box(c: &mut Canvas, v: &View, s: &Session, r: R) {
    let (x, y, w, h) = r;
    let waiting = s.state == State::Waiting;
    c.rrect(r, 14.0, if waiting { 0x231C06 } else { PANEL_DEEP }, 1.0);
    let base = y + h / 2.0 + 6.0;
    match s.state {
        State::Working => {
            let split = (w * 0.42).min(440.0);
            activity_line(c, v, s, x + 18.0, base, split - 18.0);
            if let Some(step) = s.steps.iter().rev().find(|st| st.status == StepStatus::Running) {
                step_line(c, v, step, x + split, base, w - split - 18.0, 14.0, 76.0);
            }
        }
        State::Waiting => {
            c.symbol("hand.raised.fill", 16.0, x + 28.0, base - 6.0, AMBER);
            let lw = c.text(F::Bold, "Needs you", 16.0, x + 46.0, base, AMBER, Align::Left);
            let why = if s.notice.is_empty() { "Claude is waiting for your answer" } else { s.notice.as_str() };
            c.clip(F::Medium, why, 16.0, w - lw - 90.0, x + 46.0 + lw + 14.0, base, TEXT_2);
            let ago = age(v.now_ms.saturating_sub(s.since));
            c.text(F::Medium, &ago, 15.0, x + w - 18.0, base, MUTED, Align::Right);
        }
        State::Done | State::Idle => {
            let (sym, col, word) = if s.state == State::Done { ("checkmark.circle.fill", GREEN, "Done") } else { ("moon.zzz.fill", MUTED, "Idle") };
            c.symbol(sym, 16.0, x + 28.0, base - 6.0, col);
            let lw = c.text(F::Bold, word, 16.0, x + 46.0, base, col, Align::Left);
            let ago = v.now_ms.saturating_sub(s.since);
            let when = if ago < 60_000 { "just now".to_string() } else { format!("{} ago", age(ago)) };
            let turn = s.steps.iter().rev().find(|st| st.tag == "you").map(|st| turn_time(s.since.saturating_sub(st.ts)));
            let line = match turn {
                Some(t) if s.state == State::Done => format!("finished {when}  ·  took {t}"),
                _ => format!("since {when}"),
            };
            c.clip(F::Medium, &line, 16.0, w - lw - 80.0, x + 46.0 + lw + 14.0, base, TEXT_2);
        }
    }
}

/// One item of a session's task list: a check when done (dimmed, struck through), a
/// filled dot while in progress, an empty ring still to do.
fn todo_line(c: &mut Canvas, t: &crate::status::Todo, x: f32, y: f32, w: f32, working: bool) {
    use crate::status::TodoState;
    let cy = y - 6.0;
    let (fg, font) = match t.state {
        TodoState::Completed => {
            c.symbol("checkmark.circle.fill", 15.0, x + 8.0, cy, GREEN);
            (FAINT, F::Regular)
        }
        TodoState::InProgress => {
            c.symbol("circle", 15.0, x + 8.0, cy, ORANGE);
            c.circle(x + 8.0, cy, 4.0, if working { ORANGE } else { MUTED }, 1.0);
            (TEXT, F::Bold)
        }
        TodoState::Pending => {
            c.symbol("circle", 15.0, x + 8.0, cy, FAINT);
            (TEXT_2, F::Regular)
        }
    };
    let tw = c.clip(font, &t.text, 16.0, w - 26.0, x + 24.0, y, fg);
    if t.state == TodoState::Completed {
        c.rect((x + 24.0, cy + 1.0, tw, 1.0), FAINT, 0.8);
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
    let path = if s.machine.is_empty() { short_path(&s.cwd) } else { format!("{}  ·  {}", s.machine, short_path(&s.cwd)) };
    c.fit(F::Mono, &path, 14.0, iw, ix, y + 110.0, if s.machine.is_empty() { FAINT } else { 0x8AB4F8 }, Align::Left);

    let mut cy = y + 144.0;
    c.text(F::Bold, "PROMPT", 12.0, ix, cy, FAINT, Align::Left);
    cy += 22.0;
    let typed = crate::status::typed_prompt(&s.prompt);
    let prompt = if typed.is_empty() { "—" } else { typed.as_str() };
    cy += c.paragraph(F::Regular, prompt, 18.0, ix, cy, iw, 4, 23.0, TEXT_2);
    cy += 12.0;
    let (label, body, col) = if waiting && !s.notice.is_empty() {
        ("NEEDS YOU", s.notice.as_str(), AMBER)
    } else if s.state == State::Working && !s.comment.is_empty() {
        ("CLAUDE IS SAYING", s.comment.as_str(), 0xF0C9B2)
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
    let open_label = if s.machine.is_empty() { "Open in Claude".to_string() } else { format!("On {}", s.machine) };
    for (r, label, fg) in [(back_button(), "‹  Back".to_string(), TEXT), (open_button(), open_label, if s.machine.is_empty() { TEXT } else { MUTED })] {
        c.rrect(r, 10.0, 0x26262D, 1.0);
        c.fit(F::Medium, &label, 17.0, r.2 - 16.0, r.0 + r.2 / 2.0, r.1 + 25.0, fg, Align::Center);
    }

    // Claude's task list (when it has one) beside the timeline.
    let full = detail_right();
    let rr = if s.todos.is_empty() {
        full
    } else {
        let tw = 470.0;
        let tr = (full.0, full.1, tw, full.3);
        c.rrect(tr, 14.0, PANEL_DEEP, 1.0);
        let done = s.todos.iter().filter(|t| t.state == crate::status::TodoState::Completed).count();
        c.text(F::Bold, "TASKS", 12.0, tr.0 + 24.0, tr.1 + 28.0, FAINT, Align::Left);
        c.text(F::Medium, &format!("{done} of {}", s.todos.len()), 13.0, tr.0 + tw - 24.0, tr.1 + 28.0, FAINT, Align::Right);
        let rows = ((tr.3 - 60.0) / 30.0).floor().max(1.0) as usize;
        let first_open = s.todos.iter().position(|t| t.state != crate::status::TodoState::Completed).unwrap_or(s.todos.len());
        let start = first_open.saturating_sub(2).min(s.todos.len().saturating_sub(rows));
        for (k, t) in s.todos.iter().skip(start).take(rows).enumerate() {
            todo_line(c, t, tr.0 + 22.0, tr.1 + 66.0 + k as f32 * 30.0, tw - 44.0, s.state == State::Working);
        }
        (full.0 + tw + G, full.1, full.2 - tw - G, full.3)
    };
    status_box(c, v, s, detail_status());
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
    viz::draw_behind(c, v, (M, CY, W - 2.0 * M, CH));
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

fn keys_label(a: &Action) -> String {
    match a {
        Action::Keys { keys } => keys.clone(),
        Action::Open { target } => target.rsplit('/').next().unwrap_or(target).trim_end_matches(".app").to_string(),
        Action::Shortcut { name } => format!("shortcut: {name}"),
        Action::Shell { command } => command.clone(),
        Action::MicMute => "soundboard mic".into(),
        Action::Media { command } => command.clone(),
        Action::Clip { title } => format!("clip: {title}"),
        Action::Page { page } => format!("page: {}", page.label().to_lowercase()),
    }
}

fn shortcuts(c: &mut Canvas, v: &View) {
    let focus = v.level == &Level::Focus;
    for i in 0..SC_COLS * SC_ROWS {
        let r = shortcut_rect(i);
        let Some(sc) = v.shortcuts.get(i) else {
            c.rrect(r, 16.0, PANEL_DEEP, 1.0);
            continue;
        };
        let (x, y, w, h) = r;
        let col = crate::config::hex(&sc.color, TEXT);
        let active = matches!(sc.action, Action::MicMute) && v.sounds.mic_muted;
        c.rrect(r, 16.0, if active { 0x3A1414 } else { PANEL }, 1.0);
        if !c.symbol(&sc.symbol, 58.0, x + w / 2.0, y + h * 0.40, col) {
            let initial: String = sc.title.chars().take(1).collect::<String>().to_uppercase();
            c.circle(x + w / 2.0, y + h * 0.40, 34.0, col, 0.18);
            c.text(F::Bold, &initial, 36.0, x + w / 2.0, y + h * 0.40 + 13.0, col, Align::Center);
        }
        c.fit(F::Bold, &sc.title, 20.0, w - 20.0, x + w / 2.0, y + h - 40.0, TEXT, Align::Center);
        let label = keys_label(&sc.action);
        let lw = c.measure(F::Mono, &label, 13.0).min(w - 24.0);
        c.clip(F::Mono, &label, 13.0, w - 24.0, x + (w - lw) / 2.0, y + h - 16.0, FAINT);
        if focus && i == v.tile {
            c.outline(r, 16.0, v.accent, 3.0);
        }
    }
    if v.shortcuts.is_empty() {
        c.text(F::Medium, "No shortcuts yet: add some in Quaked Settings", 26.0, W / 2.0, CY + CH / 2.0, FAINT, Align::Center);
    }
}

fn soundboard(c: &mut Canvas, v: &View) {
    let s = v.sounds;
    let focus = v.level == &Level::Focus;
    // Status strip
    let (label, detail, bg, fg) = if let Some(e) = &s.error {
        ("CHECK SETUP", e.clone(), AMBER, 0x1C1500)
    } else if s.live {
        let who = s
            .listener
            .as_deref()
            .map(|l| crate::symbols::running_app_name(l).unwrap_or_else(|| l.rsplit('.').next().unwrap_or(l).to_string()))
            .unwrap_or_else(|| "always on".into());
        ("LIVE ON MIC", format!("{who} hears your mic + clips"), GREEN, 0xFFFFFF)
    } else {
        ("NOT ON A CALL", "clips play only for you; pick BlackHole 2ch as your mic in the call app".to_string(), 0x2A2A31, TEXT)
    };
    let pw = c.pill(M, CY + 8.0, label, 15.0, bg, fg);
    c.fit(F::Medium, &detail, 18.0, mic_button().0 - M - pw - 40.0, M + pw + 16.0, CY + 30.0, if s.error.is_some() { AMBER } else { TEXT_2 }, Align::Left);
    let mr = mic_button();
    c.rrect(mr, 12.0, if s.mic_muted { 0x4A1A1A } else { 0x1C1C21 }, 1.0);
    let mic_sym = if s.mic_muted { "mic.slash.fill" } else { "mic.fill" };
    c.symbol(mic_sym, 20.0, mr.0 + 26.0, mr.1 + mr.3 / 2.0, if s.mic_muted { RED } else { TEXT });
    c.text(F::Medium, if s.mic_muted { "Mic muted" } else { "Mic on" }, 17.0, mr.0 + 48.0, mr.1 + 28.0, TEXT, Align::Left);
    let sr = stop_all_button();
    c.rrect(sr, 12.0, 0x1C1C21, 1.0);
    c.symbol("stop.fill", 18.0, sr.0 + 26.0, sr.1 + sr.3 / 2.0, TEXT);
    c.text(F::Medium, "Stop all", 17.0, sr.0 + 46.0, sr.1 + 28.0, TEXT, Align::Left);

    if s.clips.is_empty() {
        let top = CY + STRIP_H + 6.0;
        c.rrect((M, top, W - 2.0 * M, CY + CH - top), 16.0, PANEL_DEEP, 1.0);
        c.text(F::Medium, "No clips yet", 30.0, W / 2.0, top + 140.0, TEXT_2, Align::Center);
        c.text(F::Regular, "Drop audio files into ~/Music/Quaked Soundboard, or add them in Quaked Settings.", 20.0, W / 2.0, top + 178.0, FAINT, Align::Center);
        return;
    }
    let first = clip_window(s.clips.len(), v.tile);
    for slot in 0..CLIP_COLS * CLIP_ROWS {
        let r = clip_rect(slot);
        let i = first + slot;
        let Some(clip) = s.clips.get(i) else {
            c.rrect(r, 14.0, PANEL_DEEP, 1.0);
            continue;
        };
        let (x, y, w, h) = r;
        let col = crate::config::hex(&clip.color, 0xB36BFF);
        let playing = s.playing.contains(&i);
        c.rrect(r, 14.0, PANEL, 1.0);
        if playing {
            c.rrect(r, 14.0, col, 0.28);
            c.outline(r, 14.0, col, 2.0);
        }
        c.rrect((x, y, 6.0, h), 3.0, col, 1.0);
        let sym = if clip.symbol.is_empty() { if playing { "speaker.wave.3.fill" } else { "waveform" } } else { clip.symbol.as_str() };
        c.symbol(sym, 34.0, x + 46.0, y + 44.0, col);
        if playing {
            c.pill(x + w - 100.0, y + 18.0, "PLAYING", 12.0, col, 0x111114);
        }
        c.paragraph(F::Bold, &clip.title, 23.0, x + 22.0, y + 98.0, w - 40.0, 2, 28.0, TEXT);
        if focus && i == v.tile {
            c.outline(r, 14.0, TEXT, 3.0);
        }
    }
    let hidden = s.clips.len().saturating_sub(first + CLIP_COLS * CLIP_ROWS);
    if hidden > 0 {
        let r = clip_rect(CLIP_COLS * CLIP_ROWS - 1);
        let label = format!("{hidden} more ›");
        let w = c.measure(F::Bold, &label, 15.0) + 15.0 * 1.4;
        c.pill(r.0 + r.2 - w - 10.0, r.1 + r.3 - 38.0, &label, 15.0, 0x2A2A31, TEXT);
    }
}

fn bytes(n: u64) -> String {
    let f = n as f64;
    if f >= 1_099_511_627_776.0 {
        format!("{:.1} TB", f / 1_099_511_627_776.0)
    } else if f >= 1e9 {
        format!("{:.1} GB", f / 1_073_741_824.0)
    } else if f >= 1e6 {
        format!("{:.0} MB", f / 1_048_576.0)
    } else {
        format!("{:.0} KB", f / 1024.0)
    }
}

fn button(c: &mut Canvas, r: R, label: &str, bg: u32, fg: u32, symbol: &str) {
    c.rrect(r, 14.0, bg, 1.0);
    let w = c.measure(F::Bold, label, 22.0) + if symbol.is_empty() { 0.0 } else { 40.0 };
    let x0 = r.0 + (r.2 - w) / 2.0;
    if !symbol.is_empty() {
        c.symbol(symbol, 22.0, x0 + 14.0, r.1 + r.3 / 2.0, fg);
    }
    c.text(F::Bold, label, 22.0, x0 + if symbol.is_empty() { 0.0 } else { 40.0 }, r.1 + r.3 / 2.0 + 8.0, fg, Align::Left);
}

const GOLD: u32 = 0xC89B3C;

fn position_name(p: &str) -> &str {
    match p {
        "TOP" | "top" => "Top",
        "JUNGLE" | "jungle" => "Jungle",
        "MIDDLE" | "middle" => "Mid",
        "BOTTOM" | "bottom" => "Bot",
        "UTILITY" | "utility" => "Support",
        "" => "",
        other => other,
    }
}

fn league_page(c: &mut Canvas, v: &View) {
    let l = v.league;
    let (mode_label, accent, mode_sym) = mode_theme(l.mode);
    let focus = v.level == &Level::Focus;
    // Left: who, client, auto-accept
    let left = (M, CY, LEAGUE_LEFT_W, CH);
    c.rrect(left, 14.0, PANEL, 1.0);
    c.rrect((M, CY, 6.0, CH), 3.0, accent, 1.0);
    let ix = M + 28.0;
    c.symbol(mode_sym, 14.0, ix + 7.0, CY + 31.0, accent);
    c.text(F::Bold, mode_label, 13.0, ix + 22.0, CY + 36.0, if l.mode == league::Mode::None { FAINT } else { accent }, Align::Left);
    match &l.summoner {
        Some((name, level)) => {
            c.fit(F::Bold, name, 32.0, LEAGUE_LEFT_W - 56.0, ix, CY + 80.0, TEXT, Align::Left);
            c.text(F::Medium, &format!("level {level}"), 18.0, ix, CY + 108.0, MUTED, Align::Left);
        }
        None => {
            c.text(F::Bold, if l.running { "Signed in" } else { "Not running" }, 30.0, ix, CY + 80.0, TEXT_2, Align::Left);
        }
    }
    let (pl, pbg, pfg) = if l.running { ("CLIENT OPEN", GREEN, 0xFFFFFF) } else { ("CLIENT CLOSED", 0x2A2A31, MUTED) };
    c.pill(ix, CY + 128.0, pl, 13.0, pbg, pfg);
    // Your record in this kind of game (ARAM and Mayhem together).
    let (mw_, ml_) = l.mode_record;
    if l.mode != league::Mode::None && mw_ + ml_ > 0 {
        let kind = match l.mode.family() {
            league::Mode::Aram => "ARAM",
            league::Mode::Rift => "Rift",
            league::Mode::Arena => "Arena",
            _ => "this mode",
        };
        c.text(F::Bold, &format!("{kind}  {mw_}W {ml_}L"), 20.0, ix, CY + 196.0, TEXT_2, Align::Left);
        let rate = mw_ as f32 / (mw_ + ml_) as f32;
        bar(c, ix, CY + 208.0, LEAGUE_LEFT_W - 140.0, rate, accent);
        c.text(F::Medium, &format!("{:.0}%", rate * 100.0), 15.0, ix + LEAGUE_LEFT_W - 56.0, CY + 216.0, MUTED, Align::Right);
    }
    let on = v.league_cfg.auto_accept;
    let ar = league_auto_button();
    c.rrect(ar, 16.0, 0x1C1C21, 1.0);
    if on {
        c.rrect(ar, 16.0, accent, 0.16);
    }
    if on {
        c.outline(ar, 16.0, accent, 2.0);
    }
    // switch
    let (sx, sy) = (ar.0 + 22.0, ar.1 + ar.3 / 2.0 - 15.0);
    c.rrect((sx, sy, 56.0, 30.0), 15.0, if on { accent } else { 0x3A3A42 }, 1.0);
    c.circle(if on { sx + 41.0 } else { sx + 15.0 }, sy + 15.0, 12.0, 0xFFFFFF, 1.0);
    c.text(F::Bold, if on { "Auto-accept on" } else { "Auto-accept off" }, 22.0, sx + 74.0, ar.1 + 32.0, TEXT, Align::Left);
    let sub = if on { format!("accepts after {:.1}s", v.league_cfg.accept_delay_ms as f32 / 1000.0) } else { "tap to turn on".to_string() };
    c.text(F::Medium, &sub, 15.0, sx + 74.0, ar.1 + 54.0, MUTED, Align::Left);
    if v.league_cfg.snap {
        c.text(F::Medium, "jumps here when the queue pops", 15.0, ix, CY + CH - 26.0, FAINT, Align::Left);
    }

    match &l.live {
        Some(g) => scoreboard(c, v, g, accent),
        None => duos_panel(c, v),
    }

    // Main: the phase
    let mr = league_main();
    let (mx, my, mw, mh) = mr;
    let ready = l.phase == "ReadyCheck";
    c.rrect(mr, 14.0, if ready { 0x2E2410 } else { PANEL_DEEP }, 1.0);
    if ready {
        c.outline(mr, 14.0, accent, 3.0);
    }
    let tx = mx + 32.0;
    if !l.running {
        c.text(F::Bold, "League isn't running", 40.0, tx, my + 96.0, TEXT_2, Align::Left);
        c.paragraph(F::Regular, "Start the League client. This page follows your queue, accepts the match for you and jumps here when one pops.", 22.0, tx, my + 140.0, mw - 64.0, 3, 30.0, MUTED);
        return;
    }
    let queue = l.queue.clone().unwrap_or_default();
    if let Some(g) = &l.live {
        return live_game(c, v, g, mr, accent);
    }
    // After a game: how it went, until you queue again.
    if let Some(p) = l.post_game.as_ref().filter(|_| !matches!(l.phase.as_str(), "Matchmaking" | "ReadyCheck" | "ChampSelect")) {
        return post_game_card(c, v, p, mr, focus);
    }
    session_line(c, l, tx, my + mh - 24.0, mw - 64.0);
    match l.phase.as_str() {
        "Lobby" => {
            c.text(F::Bold, "IN LOBBY", 18.0, tx, my + 46.0, accent, Align::Left);
            c.fit(F::Bold, if queue.is_empty() { "Ready to queue" } else { &queue }, 48.0, mw - 64.0, tx, my + 110.0, TEXT, Align::Left);
            button(c, league_primary(), "Find match", accent, 0x1A1408, "magnifyingglass");
            if focus {
                c.outline(league_primary(), 14.0, TEXT, 3.0);
            }
        }
        "Matchmaking" => {
            c.text(F::Bold, "IN QUEUE", 18.0, tx, my + 46.0, accent, Align::Left);
            let (t, est) = l.search.unwrap_or((0.0, 0.0));
            c.text(F::Bold, &mmss(t), 110.0, tx, my + 160.0, TEXT, Align::Left);
            let detail = if est > 0.0 { format!("estimated {}   ·   {queue}", mmss(est)) } else { queue.clone() };
            c.fit(F::Medium, &detail, 22.0, mw - 64.0, tx, my + 200.0, MUTED, Align::Left);
            button(c, league_primary(), "Cancel", 0x2A2A31, TEXT, "xmark");
            if focus {
                c.outline(league_primary(), 14.0, TEXT, 3.0);
            }
        }
        "ReadyCheck" => {
            let rc = l.ready.clone().unwrap_or_default();
            c.text(F::Bold, "MATCH FOUND", 76.0, tx, my + 110.0, accent, Align::Left);
            let (label, col) = match rc.response.as_str() {
                "Accepted" => (if l.accepted_by_quaked { "ACCEPTED  ·  by quaked".to_string() } else { "ACCEPTED".to_string() }, GREEN),
                "Declined" => ("DECLINED".to_string(), RED),
                _ if v.league_cfg.auto_accept => ("accepting…".to_string(), TEXT),
                _ => (format!("{}s left", (12.0 - rc.timer).max(0.0).round()), TEXT),
            };
            c.text(F::Bold, &label, 34.0, tx, my + 166.0, col, Align::Left);
            if !queue.is_empty() {
                c.fit(F::Medium, &queue, 20.0, mw - 64.0, tx, my + 202.0, MUTED, Align::Left);
            }
            if rc.response != "Accepted" {
                button(c, league_primary(), "Accept", GREEN, 0xFFFFFF, "checkmark");
                if focus {
                    c.outline(league_primary(), 14.0, TEXT, 3.0);
                }
            }
            if rc.response != "Declined" {
                button(c, league_secondary(), "Decline", 0x4A1A1A, 0xFFD0D0, "xmark");
            }
        }
        "ChampSelect" => {
            let ch = l.champ.clone().unwrap_or_default();
            let phase = match ch.phase.as_str() {
                "PLANNING" => "PLANNING",
                "BAN_PICK" => "PICKS & BANS",
                "FINALIZATION" => "FINALIZING",
                "GAME_STARTING" => "STARTING",
                other => other,
            };
            let hw = c.text(F::Bold, &format!("CHAMP SELECT  ·  {phase}"), 18.0, tx, my + 46.0, accent, Align::Left);
            // Your position and the queue, top right.
            let mut line = Vec::new();
            if !ch.position.is_empty() {
                line.push(position_name(&ch.position).to_string());
            }
            if !queue.is_empty() {
                line.push(queue.clone());
            }
            c.fit(F::Medium, &line.join("  ·  "), 18.0, mw - 64.0 - hw - 32.0, mx + mw - 32.0, my + 46.0, MUTED, Align::Right);
            // The countdown on the left, your record on the champion beside it.
            let secs = (ch.time_left_ms / 1000.0).max(0.0).ceil();
            c.fit(F::Bold, &format!("{secs:.0}s"), 120.0, 200.0, tx, my + 226.0, TEXT, Align::Left);
            champ_card(c, v, &ch, (tx + 222.0, my + 68.0, mw - 64.0 - 222.0, 232.0), accent);
        }
        "InProgress" | "GameStart" | "Reconnect" => {
            c.text(F::Bold, "IN GAME", 18.0, tx, my + 46.0, accent, Align::Left);
            c.text(F::Bold, if l.phase == "Reconnect" { "Reconnect needed" } else { "Good luck" }, 64.0, tx, my + 130.0, TEXT, Align::Left);
        }
        "WaitingForStats" | "PreEndOfGame" | "EndOfGame" => {
            c.text(F::Bold, "GAME OVER", 18.0, tx, my + 46.0, accent, Align::Left);
            c.text(F::Bold, "Back to the lobby when you're ready", 40.0, tx, my + 116.0, TEXT_2, Align::Left);
        }
        other => {
            c.text(F::Bold, "CLIENT", 18.0, tx, my + 46.0, accent, Align::Left);
            c.text(F::Bold, if other.is_empty() || other == "None" { "Home screen" } else { other }, 48.0, tx, my + 110.0, TEXT_2, Align::Left);
        }
    }
    let _ = mh;
}

/// (label, accent, symbol) for the kind of game you're playing.
fn mode_theme(m: league::Mode) -> (&'static str, u32, &'static str) {
    match m {
        league::Mode::Aram => ("ARAM  ·  HOWLING ABYSS", 0x7FB8FF, "snowflake"),
        league::Mode::Mayhem => ("ARAM: MAYHEM", 0xC084FC, "sparkles"),
        league::Mode::Rift => ("SUMMONER'S RIFT", GOLD, "map.fill"),
        league::Mode::Arena => ("ARENA", 0xFF7A59, "trophy.fill"),
        league::Mode::Other => ("LEAGUE OF LEGENDS", GOLD, "gamecontroller.fill"),
        league::Mode::None => ("LEAGUE OF LEGENDS", GOLD, "gamecontroller.fill"),
    }
}

fn dragon_color(kind: &str) -> u32 {
    match kind {
        "Infernal" => 0xFF6B3D,
        "Ocean" => 0x3DB8FF,
        "Mountain" => 0xB08850,
        "Cloud" => 0xBFE3F2,
        "Hextech" => 0x4FD1C5,
        "Chemtech" => 0x9ACD32,
        "Elder" => 0xF0E6D2,
        _ => MUTED,
    }
}

/// "This session 3W 2L ●●○●● · won 2 in a row" (a dot per game, oldest first).
fn session_line(c: &mut Canvas, l: &league::Status, x: f32, y: f32, w: f32) {
    let (sw, sl) = l.session;
    let col = if l.streak <= -2 { LOSS } else { MUTED };
    let mut cx = x;
    if sw + sl > 0 {
        cx += c.clip(F::Medium, &format!("This session {sw}W {sl}L"), 17.0, w, cx, y, col) + 12.0;
        let games = &l.session_games[l.session_games.len().saturating_sub(12)..];
        if !games.is_empty() && cx + games.len() as f32 * 14.0 <= x + w {
            for (i, win) in games.iter().enumerate() {
                c.circle(cx + 5.0 + i as f32 * 14.0, y - 6.0, 5.0, if *win { GREEN } else { LOSS }, 1.0);
            }
            cx += games.len() as f32 * 14.0 + 8.0;
        }
    }
    let streak = match l.streak {
        n if n >= 2 => format!("won {n} in a row"),
        n if n <= -2 => format!("lost {} in a row", -n),
        _ => String::new(),
    };
    if !streak.is_empty() && x + w - cx > 40.0 {
        let text = if cx > x { format!("·  {streak}") } else { streak };
        c.clip(F::Medium, &text, 17.0, x + w - cx, cx, y, col);
    }
}

const LOSS: u32 = 0xE5534B;

/// "ARAM" for records (Mayhem counts with ARAM).
fn family_name(m: league::Mode) -> &'static str {
    match m.family() {
        league::Mode::Aram | league::Mode::Mayhem => "ARAM",
        league::Mode::Rift => "Rift",
        league::Mode::Arena => "Arena",
        league::Mode::Other => "this mode",
        league::Mode::None => "all modes",
    }
}

/// 38450 -> "38,450"
fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

fn rate_color((w, l): (u32, u32)) -> u32 {
    if w >= l { GREEN } else { LOSS }
}

/// "ARAM 21W 15L ▬▬ 58%" on one line; `bar_w` 0 leaves the bar out. Returns its width;
/// with `draw` false it only measures.
#[allow(clippy::too_many_arguments)]
fn record_seg(c: &mut Canvas, x: f32, y: f32, label: &str, rec: (u32, u32), label_col: u32, bar_w: f32, draw: bool) -> f32 {
    let (w_, l_) = rec;
    let value = format!("{w_}W {l_}L");
    let rate = if w_ + l_ > 0 { w_ as f32 / (w_ + l_) as f32 } else { 0.0 };
    let pct = format!("{:.0}%", rate * 100.0);
    let lw = c.measure(F::Bold, label, 14.0) + 10.0;
    let vw = c.measure(F::Bold, &value, 20.0) + 12.0;
    let bw = if bar_w > 0.0 { bar_w + 10.0 } else { 0.0 };
    let pw = c.measure(F::Bold, &pct, 15.0);
    if draw {
        c.text(F::Bold, label, 14.0, x, y, label_col, Align::Left);
        c.text(F::Bold, &value, 20.0, x + lw, y, TEXT, Align::Left);
        if bar_w > 0.0 {
            bar(c, x + lw + vw, y - 10.0, bar_w, rate, rate_color(rec));
        }
        c.text(F::Bold, &pct, 15.0, x + lw + vw + bw, y, rate_color(rec), Align::Left);
    }
    lw + vw + bw + pw
}

/// Champ select: your record on the champion you're showing (or a nudge to pick one).
fn champ_card(c: &mut Canvas, v: &View, ch: &league::ChampSelect, r: R, accent: u32) {
    let l = v.league;
    let (x, y, w, _h) = r;
    let ix = x + 26.0;
    let iw = w - 52.0;
    if ch.champion.is_empty() {
        c.outline(r, 14.0, LINE, 2.0);
        c.text(F::Bold, "Pick a champion", 38.0, ix, y + 76.0, TEXT_2, Align::Left);
        c.paragraph(F::Medium, "Hover or lock one in and your record on it shows here.", 19.0, ix, y + 118.0, iw, 2, 26.0, FAINT);
        return;
    }
    c.rrect(r, 14.0, 0x1A1A20, 1.0);
    let rec = l.champ_record.clone().unwrap_or_default();
    let games = rec.games();
    let mut meta_w = 0.0;
    if games > 0 {
        let last = if rec.last > 0 && v.now_ms > rec.last {
            let m = (v.now_ms - rec.last) / 60_000;
            let when = if m < 60 { format!("{}m", m.max(1)) } else if m < 48 * 60 { format!("{}h", m / 60) } else { format!("{}d", m / 1440) };
            format!("  ·  last {when} ago")
        } else {
            String::new()
        };
        meta_w = c.text(F::Medium, &format!("{} on it{last}", plural(games as usize, "game", "games")), 17.0, x + w - 26.0, y + 54.0, MUTED, Align::Right) + 24.0;
    }
    c.fit(F::Bold, &ch.champion, 50.0, iw - meta_w, ix, y + 70.0, TEXT, Align::Left);
    if games == 0 {
        c.symbol("sparkles", 24.0, ix + 13.0, y + 125.0, accent);
        c.fit(F::Bold, &format!("First game on {}", ch.champion), 30.0, iw - 38.0, ix + 38.0, y + 136.0, accent, Align::Left);
        c.text(F::Medium, "win or lose, this one starts your record", 18.0, ix, y + 180.0, FAINT, Align::Left);
        return;
    }
    // This kind of game, every mode, and the last few games on it.
    let known_mode = !matches!(l.mode, league::Mode::None | league::Mode::Other);
    let kind = family_name(l.mode).to_uppercase();
    let mut cols: Vec<(String, (u32, u32), u32, u32)> = Vec::new();
    if known_mode {
        cols.push((format!("IN {kind}"), rec.mode, accent, TEXT));
    }
    cols.push(("ALL MODES".to_string(), rec.all, FAINT, if known_mode { TEXT_2 } else { TEXT }));
    let colw = iw / 3.0;
    for (k, (label, wl, label_col, value_col)) in cols.iter().enumerate() {
        let bx = ix + k as f32 * colw;
        c.text(F::Bold, label, 14.0, bx, y + 124.0, *label_col, Align::Left);
        if wl.0 + wl.1 == 0 {
            c.text(F::Medium, "no games yet", 22.0, bx, y + 166.0, FAINT, Align::Left);
            continue;
        }
        c.fit(F::Bold, &format!("{}W {}L", wl.0, wl.1), 40.0, colw - 20.0, bx, y + 170.0, *value_col, Align::Left);
        let rate = wl.0 as f32 / (wl.0 + wl.1) as f32;
        bar(c, bx, y + 188.0, colw - 84.0, rate, rate_color(*wl));
        c.text(F::Bold, &format!("{:.0}%", rate * 100.0), 17.0, bx + colw - 28.0, y + 197.0, rate_color(*wl), Align::Right);
    }
    let bx = ix + cols.len() as f32 * colw;
    if cols.len() < 3 && !rec.recent.is_empty() {
        let label = if known_mode { format!("LAST {} IN {kind}", rec.recent.len()) } else { format!("LAST {}", rec.recent.len()) };
        c.text(F::Bold, &label, 14.0, bx, y + 124.0, FAINT, Align::Left);
        for (i, win) in rec.recent.iter().enumerate() {
            let cx = bx + 13.0 + i as f32 * 32.0;
            c.circle(cx, y + 156.0, 13.0, if *win { GREEN } else { LOSS }, 1.0);
            c.text(F::Bold, if *win { "W" } else { "L" }, 13.0, cx, y + 160.5, 0xFFFFFF, Align::Center);
        }
    }
}

/// The middle of the pane after a game: how it went, then your records with it counted.
/// In the lobby the Find match button stays where it always is.
fn post_game_card(c: &mut Canvas, v: &View, p: &league::PostGame, r: R, focus: bool) {
    let l = v.league;
    let (x, y, w, _h) = r;
    let accent = mode_theme(p.mode).1;
    let lobby = l.phase == "Lobby";
    let over = matches!(l.phase.as_str(), "WaitingForStats" | "PreEndOfGame" | "EndOfGame");
    let (result, col) = if p.remake {
        ("REMAKE".to_string(), TEXT_2)
    } else if p.placement > 0 {
        (format!("{} PLACE", ordinal(p.placement).to_uppercase()), if p.win { GREEN } else { LOSS })
    } else if p.win {
        ("VICTORY".to_string(), GREEN)
    } else {
        ("DEFEAT".to_string(), LOSS)
    };
    c.rrect(r, 14.0, col, 0.07);
    c.outline(r, 14.0, if p.remake { LINE } else if p.win { 0x1F4A33 } else { 0x5A2328 }, 2.0);
    let tx = x + 32.0;
    let tw = w - 64.0;
    let rx = x + w - 32.0;
    let name = if p.mode_name.is_empty() { mode_theme(p.mode).0.to_string() } else { p.mode_name.to_uppercase() };
    c.fit(F::Bold, &format!("{}  ·  {name}  ·  {}", if over { "GAME OVER" } else { "LAST GAME" }, mmss(p.duration)), 18.0, tw, tx, y + 46.0, accent, Align::Left);
    // The result and your champion; K/D/A on the right, like the score in game.
    let kda_w = c.text(F::Bold, &format!("{} / {} / {}", p.kills, p.deaths, p.assists), 58.0, rx, y + 112.0, TEXT, Align::Right);
    let ratio = if p.deaths == 0 { "perfect kda".to_string() } else { format!("{:.1} kda", (p.kills + p.assists) as f32 / p.deaths as f32) };
    let kp = (p.team_kills > 0).then(|| format!("{:.0}%", ((p.kills + p.assists) as f32 / p.team_kills as f32 * 100.0).min(100.0)));
    let rift = p.mode.family() == league::Mode::Rift;
    let kda_label = match (&kp, rift) {
        (Some(kp), true) => format!("k / d / a  ·  {ratio}  ·  {kp} kill participation"),
        _ => format!("k / d / a  ·  {ratio}"),
    };
    c.text(F::Medium, &kda_label, 14.0, rx, y + 136.0, FAINT, Align::Right);
    c.fit(F::Bold, &result, 72.0, tw - kda_w - 32.0, tx, y + 120.0, col, Align::Left);
    let champ_w = c.fit(F::Bold, &p.champion, 28.0, tw / 2.0, tx, y + 162.0, TEXT, Align::Left);
    if p.level > 0 {
        c.text(F::Medium, &format!("level {}", p.level), 20.0, tx + champ_w + 14.0, y + 162.0, MUTED, Align::Left);
    }
    // Your numbers: what matters in this mode.
    let dmg_note = match p.damage_rank {
        0 => String::new(),
        1 => "most on your team".to_string(),
        n => format!("{} on your team", ordinal(n)),
    };
    let mut stats: Vec<(String, String, String)> = vec![(thousands(p.damage), "damage to champions".into(), dmg_note), (thousands(p.gold), "gold".into(), String::new())];
    if rift {
        let per_min = if p.duration > 60.0 { format!("  ·  {:.1} / min", p.cs as f64 / (p.duration / 60.0)) } else { String::new() };
        stats.push((p.cs.to_string(), format!("cs{per_min}"), String::new()));
        stats.push((p.vision.to_string(), "vision".into(), String::new()));
    } else if let Some(kp) = kp {
        stats.push((kp, "kill participation".into(), String::new()));
    }
    stats.push((p.level.to_string(), "level".into(), String::new()));
    let sw = tw / stats.len() as f32;
    for (k, (val, label, note)) in stats.iter().enumerate() {
        let sx = tx + k as f32 * sw;
        c.fit(F::Bold, val, 34.0, sw - 16.0, sx, y + 216.0, TEXT, Align::Left);
        c.clip(F::Medium, label, 14.0, sw - 12.0, sx, y + 238.0, FAINT);
        if !note.is_empty() {
            c.clip(F::Bold, note, 13.0, sw - 12.0, sx, y + 257.0, accent);
        }
    }
    c.rect((tx, y + 274.0, tw, 1.0), LINE, 1.0);
    // Your records now: this kind of game and the champion, the session, the people.
    let (lx, lw) = if lobby { (tx + 300.0, tw - 300.0) } else { (tx, tw) };
    if lobby {
        button(c, league_primary(), "Find match", accent, 0x1A1408, "magnifyingglass");
        if focus {
            c.outline(league_primary(), 14.0, TEXT, 3.0);
        }
    }
    let mut ly = y + 308.0;
    let kind = family_name(p.mode).to_uppercase();
    let champ_label = if p.champion.is_empty() { String::new() } else { format!("{} IN {kind}", p.champion.to_uppercase()) };
    let gap = 30.0;
    let full = record_seg(c, 0.0, 0.0, &kind, p.mode_record, accent, 70.0, false) + if champ_label.is_empty() { 0.0 } else { gap + record_seg(c, 0.0, 0.0, &champ_label, p.champ_record, accent, 70.0, false) };
    let bar_w = if full <= lw { 70.0 } else { 0.0 };
    let mw_ = record_seg(c, lx, ly, &kind, p.mode_record, accent, bar_w, true);
    if !champ_label.is_empty() && p.champ_record.0 + p.champ_record.1 > 0 {
        let cw = record_seg(c, 0.0, 0.0, &champ_label, p.champ_record, accent, bar_w, false);
        if mw_ + gap + cw <= lw {
            record_seg(c, lx + mw_ + gap, ly, &champ_label, p.champ_record, accent, bar_w, true);
        }
    }
    ly += 34.0;
    if l.session.0 + l.session.1 > 0 || l.streak.abs() >= 2 {
        session_line(c, l, lx, ly, lw);
        ly += 34.0;
    }
    // "with Sam now 20–12  ·  Priya now 11–12"
    if !p.mates.is_empty() {
        let mut cx = lx + c.text(F::Medium, "with", 17.0, lx, ly, MUTED, Align::Left) + 10.0;
        for (i, d) in p.mates.iter().enumerate() {
            let rec = (d.wins, d.games - d.wins);
            let name = if d.name.is_empty() { "teammate" } else { d.name.as_str() };
            let need = c.measure(F::Bold, name, 18.0) + c.measure(F::Medium, " now ", 17.0) + c.measure(F::Bold, "00–00", 18.0) + if i > 0 { 28.0 } else { 0.0 } + if d.friend { 22.0 } else { 0.0 };
            if cx + need > lx + lw {
                let more = p.mates.len() - i;
                c.text(F::Medium, &format!("+{more}"), 17.0, cx, ly, MUTED, Align::Left);
                break;
            }
            if i > 0 {
                cx += c.text(F::Medium, "  ·  ", 17.0, cx, ly, FAINT, Align::Left);
            }
            if d.friend {
                c.symbol("person.2.fill", 12.0, cx + 9.0, ly - 6.0, GOLD);
                cx += 22.0;
            }
            cx += c.text(F::Bold, name, 18.0, cx, ly, TEXT, Align::Left);
            cx += c.text(F::Medium, " now ", 17.0, cx, ly, MUTED, Align::Left);
            cx += c.text(F::Bold, &format!("{}–{}", rec.0, rec.1), 18.0, cx, ly, rate_color(rec), Align::Left);
        }
    }
}

/// The middle of the League pane while a game runs (from the game's live data).
fn live_game(c: &mut Canvas, v: &View, g: &league::LiveGame, r: R, accent: u32) {
    let (x, y, w, h) = r;
    let tx = x + 32.0;
    let tw = w - 64.0;
    let fmt_gold = |gold: f64| {
        let n = gold.max(0.0) as u64;
        if n >= 1000 { format!("{},{:03}", n / 1000, n % 1000) } else { n.to_string() }
    };
    let dead = g.dead && g.result.is_none();
    if dead {
        c.rrect(r, 14.0, 0x2A1214, 1.0);
        c.outline(r, 14.0, 0x5A2328, 2.0);
        let aram = v.league.mode.family() == league::Mode::Aram;
        c.text(F::Bold, if aram { "SHOP NOW  ·  RESPAWNING IN" } else { "RESPAWNING IN" }, 18.0, tx, y + 46.0, 0xFF8A8A, Align::Left);
        let secs = g.respawn.max(0.0).ceil();
        c.text(F::Bold, &mmss(secs), 120.0, tx, y + 170.0, TEXT, Align::Left);
        if !g.killed_by.is_empty() {
            c.text(F::Medium, &format!("killed by {}", g.killed_by), 24.0, tx, y + 214.0, MUTED, Align::Left);
        }
        // In ARAM this is the only time you can buy: make the gold the headline.
        let gold_line = if aram { format!("{} gold  ·  shop before you're back", fmt_gold(g.gold)) } else { format!("{} gold to spend", fmt_gold(g.gold)) };
        c.text(F::Bold, &gold_line, if aram { 34.0 } else { 30.0 }, tx, y + 262.0, GOLD, Align::Left);
    } else {
        // Clock and score.
        let header = match g.result.as_deref() {
            Some("Win") => ("VICTORY".to_string(), GREEN),
            Some(_) => ("DEFEAT".to_string(), 0xE5534B),
            None => (format!("IN GAME  ·  {}", if g.champion.is_empty() { g.mode.clone() } else { g.champion.to_uppercase() }), accent),
        };
        c.text(F::Bold, &header.0, 18.0, tx, y + 42.0, header.1, Align::Left);
        c.text(F::Bold, &mmss(g.time), 58.0, tx, y + 104.0, TEXT, Align::Left);
        let rx = x + w - 32.0;
        let ew = c.text(F::Bold, &g.enemy_kills.to_string(), 58.0, rx, y + 104.0, 0xE5534B, Align::Right);
        let dw = c.text(F::Bold, "–", 40.0, rx - ew - 14.0, y + 98.0, FAINT, Align::Right);
        let aw = c.text(F::Bold, &g.ally_kills.to_string(), 58.0, rx - ew - dw - 28.0, y + 104.0, GREEN, Align::Right);
        c.text(F::Medium, "your team  ·  enemy", 13.0, rx, y + 124.0, FAINT, Align::Right);
        let _ = aw;
        // Your stats: what matters in this mode.
        let mode = v.league.mode;
        let kda = (format!("{} / {} / {}", g.kills, g.deaths, g.assists), "k / d / a".to_string());
        let kp = if g.ally_kills > 0 { format!("{:.0}%", (g.kills + g.assists) as f32 / g.ally_kills as f32 * 100.0) } else { "–".into() };
        let stats: Vec<(String, String)> = match mode.family() {
            league::Mode::Aram => vec![kda, (kp, "kill participation".into()), (fmt_gold(g.gold), "gold".into()), (g.level.to_string(), "level".into())],
            league::Mode::Rift => {
                let per_min = if g.time > 60.0 { format!("{:.1} cs / min", g.cs as f64 / (g.time / 60.0)) } else { "cs".to_string() };
                vec![kda, (g.cs.to_string(), per_min), (fmt_gold(g.gold), "gold".into()), (g.level.to_string(), "level".into()), (format!("{:.0}", g.ward_score), "vision".into())]
            }
            _ => vec![kda, (g.level.to_string(), "level".into()), (fmt_gold(g.gold), "gold".into()), (kp, "kill participation".into())],
        };
        let sw = tw / stats.len() as f32;
        for (k, (val, label)) in stats.iter().enumerate() {
            let sx = tx + k as f32 * sw;
            c.fit(F::Bold, val, 36.0, sw - 16.0, sx, y + 182.0, TEXT, Align::Left);
            c.text(F::Medium, label, 14.0, sx, y + 204.0, FAINT, Align::Left);
        }
    }
    // Objectives (Rift) or towers (ARAM), per side.
    let mut strip_h = 0.0;
    if !dead {
        let sy = y + 240.0;
        let mut sx = tx;
        let pair = |c: &mut Canvas, sx: &mut f32, label: &str, (a, b): (u32, u32)| {
            *sx += c.text(F::Medium, label, 14.0, *sx, sy, FAINT, Align::Left) + 8.0;
            *sx += c.text(F::Bold, &a.to_string(), 17.0, *sx, sy, GREEN, Align::Left) + 4.0;
            *sx += c.text(F::Bold, "–", 14.0, *sx, sy, FAINT, Align::Left) + 4.0;
            *sx += c.text(F::Bold, &b.to_string(), 17.0, *sx, sy, 0xE5534B, Align::Left) + 22.0;
        };
        match v.league.mode.family() {
            league::Mode::Rift => {
                // Dragons as colored pips: yours, then theirs.
                sx += c.text(F::Medium, "drakes", 14.0, sx, sy, FAINT, Align::Left) + 10.0;
                for (k, d) in g.dragons.0.iter().enumerate() {
                    c.circle(sx + 7.0 + k as f32 * 18.0, sy - 5.0, 6.5, dragon_color(d), 1.0);
                }
                sx += (g.dragons.0.len().max(1) as f32) * 18.0 + 6.0;
                sx += c.text(F::Bold, "vs", 13.0, sx, sy, FAINT, Align::Left) + 10.0;
                for (k, d) in g.dragons.1.iter().enumerate() {
                    c.circle(sx + 7.0 + k as f32 * 18.0, sy - 5.0, 6.5, dragon_color(d), 0.85);
                    c.outline((sx + k as f32 * 18.0, sy - 12.0, 14.0, 14.0), 7.0, 0xE5534B, 1.5);
                }
                sx += (g.dragons.1.len().max(1) as f32) * 18.0 + 20.0;
                pair(c, &mut sx, "grubs", g.grubs);
                pair(c, &mut sx, "herald", g.heralds);
                pair(c, &mut sx, "baron", g.barons);
                pair(c, &mut sx, "towers", g.turrets);
            }
            _ => {
                pair(c, &mut sx, "towers", g.turrets);
                pair(c, &mut sx, "inhibitors", g.inhibs);
            }
        }
        strip_h = 18.0;
    }
    // Event feed, newest first.
    let top = if dead { y + 296.0 } else { y + 232.0 + strip_h + 8.0 };
    let rows = ((y + h - 14.0 - top) / 30.0).floor().max(0.0) as usize;
    if g.events.is_empty() {
        if !dead {
            c.text(F::Medium, "objectives, kills and towers show up here", 15.0, tx, top + 22.0, FAINT, Align::Left);
        }
        return;
    }
    c.rect((tx, top, tw, 1.0), LINE, 1.0);
    for (k, (t, text, side)) in g.events.iter().rev().take(rows).enumerate() {
        let ry = top + 24.0 + k as f32 * 30.0;
        let col = match side {
            1 => GREEN,
            -1 => 0xE5534B,
            _ => FAINT,
        };
        c.text(F::Mono, &mmss(*t), 14.0, tx, ry, FAINT, Align::Left);
        c.circle(tx + 64.0, ry - 5.0, 4.5, col, 1.0);
        c.clip(F::Medium, text, 18.0, tw - 84.0, tx + 78.0, ry, if k == 0 { TEXT } else { TEXT_2 });
    }
    let _ = v;
}

/// The right side during a game: both teams as the Tab screen shows them, with your record
/// next to friends and people you've played with.
fn scoreboard(c: &mut Canvas, v: &View, g: &league::LiveGame, accent: u32) {
    let l = v.league;
    let r = league_duos();
    let (x, y, w, h) = r;
    c.rrect(r, 14.0, PANEL, 1.0);
    let ix = x + 18.0;
    let iw = w - 36.0;
    c.text(F::Bold, "SCOREBOARD", 13.0, ix, y + 30.0, FAINT, Align::Left);
    let colw = (iw - 16.0) / 2.0;
    let known = |name: &str| l.duos.iter().chain(l.with_now.iter()).find(|d| d.games > 0 && !name.is_empty() && d.name.eq_ignore_ascii_case(name));
    for (k, (label, col)) in [("YOUR TEAM", GREEN), ("ENEMY", 0xE5534B)].iter().enumerate() {
        let cx = ix + k as f32 * (colw + 16.0);
        c.text(F::Bold, label, 11.0, cx, y + 56.0, *col, Align::Left);
        let team: Vec<&league::LivePlayer> = g.players.iter().filter(|p| p.mine == (k == 0)).collect();
        for (i, p) in team.iter().take(5).enumerate() {
            let ry = y + 66.0 + i as f32 * 62.0;
            let row = (cx - 6.0, ry, colw + 12.0, 56.0);
            if p.me {
                c.rrect(row, 10.0, 0x22222A, 1.0);
                c.outline(row, 10.0, accent, 1.5);
            }
            let fg = if p.dead { FAINT } else { TEXT };
            c.clip(F::Bold, &p.champion, 17.0, colw - 70.0, cx, ry + 22.0, fg);
            c.text(F::Mono, &format!("{}", p.level), 13.0, cx + colw, ry + 21.0, MUTED, Align::Right);
            let kda = format!("{}/{}/{}", p.kills, p.deaths, p.assists);
            c.text(F::Mono, &kda, 15.0, cx, ry + 45.0, if p.dead { FAINT } else { TEXT_2 }, Align::Left);
            if p.dead {
                c.text(F::Bold, &format!("{:.0}s", p.respawn.max(0.0).ceil()), 15.0, cx + colw, ry + 45.0, 0xFF8A8A, Align::Right);
            } else if let Some(d) = known(&p.name).filter(|_| p.mine && !p.me) {
                c.symbol("person.2.fill", 11.0, cx + colw - 62.0, ry + 40.0, accent);
                c.text(F::Bold, &format!("{}–{}", d.wins, d.games - d.wins), 14.0, cx + colw, ry + 45.0, accent, Align::Right);
            }
        }
    }
    let _ = h;
}

/// One person's record with you: name, W–L, a win-rate bar.
fn duo_row(c: &mut Canvas, x: f32, y: f32, w: f32, d: &league::Duo, now: bool) {
    let mid = y + 20.0;
    if d.friend {
        c.symbol("person.2.fill", 13.0, x + 10.0, mid, GOLD);
    }
    let name = if d.name.is_empty() { "teammate" } else { d.name.as_str() };
    c.clip(F::Bold, name, 18.0, w - 250.0, x + 26.0, mid + 6.0, if now { TEXT } else { TEXT_2 });
    if d.games == 0 {
        c.text(F::Medium, "first game together", 14.0, x + w, mid + 5.0, FAINT, Align::Right);
        return;
    }
    let rate = d.wins as f32 / d.games as f32;
    let col = if rate >= 0.5 { GREEN } else { 0xE5534B };
    c.text(F::Medium, &format!("{:.0}%", rate * 100.0), 14.0, x + w, mid + 5.0, col, Align::Right);
    bar(c, x + w - 150.0, mid - 4.0, 96.0, rate, col);
    c.text(F::Bold, &format!("{}W {}L", d.wins, d.games - d.wins), 16.0, x + w - 164.0, mid + 6.0, TEXT, Align::Right);
}

fn duos_panel(c: &mut Canvas, v: &View) {
    let l = v.league;
    let r = league_duos();
    let (x, y, w, h) = r;
    c.rrect(r, 14.0, PANEL, 1.0);
    let ix = x + 22.0;
    let iw = w - 44.0;
    c.text(F::Bold, "PLAYED WITH", 13.0, ix, y + 30.0, FAINT, Align::Left);
    let (wins, losses) = l.record;
    if wins + losses > 0 {
        let rate = wins as f32 / (wins + losses) as f32 * 100.0;
        c.text(F::Medium, &format!("your record  {wins}W {losses}L  ·  {rate:.0}%"), 14.0, ix + iw, y + 30.0, MUTED, Align::Right);
    }
    let mut ry = y + 44.0;
    let in_game = !l.with_now.is_empty();
    // Only people you know: names of strangers are hidden in game, and they'd just be noise.
    let known_now: Vec<&league::Duo> = l.with_now.iter().filter(|d| d.games > 0 || d.friend).collect();
    let strangers = l.with_now.len() - known_now.len();
    if in_game {
        let label = match l.phase.as_str() {
            "ChampSelect" => "IN CHAMP SELECT WITH YOU",
            "InProgress" | "GameStart" | "Reconnect" => "IN THIS GAME WITH YOU",
            _ => "IN YOUR PARTY",
        };
        c.text(F::Bold, label, 11.0, ix, ry + 14.0, GOLD, Align::Left);
        ry += 22.0;
        // Friends first, then the people you've played with most.
        let mut now = known_now.clone();
        now.sort_by_key(|d| (std::cmp::Reverse(d.friend), std::cmp::Reverse(d.games)));
        for d in now.iter().take(4) {
            c.rrect((ix - 8.0, ry + 2.0, iw + 16.0, 36.0), 9.0, 0x22222A, 1.0);
            duo_row(c, ix, ry, iw, d, true);
            ry += 40.0;
        }
        if strangers > 0 {
            let line = if known_now.is_empty() {
                format!("{} new teammate{}  ·  no one you've played with before", strangers, if strangers == 1 { "" } else { "s" })
            } else {
                format!("+ {} new teammate{}", strangers, if strangers == 1 { "" } else { "s" })
            };
            c.text(F::Medium, &line, 14.0, ix, ry + 22.0, FAINT, Align::Left);
            ry += 32.0;
        }
        ry += 8.0;
    }
    let others: Vec<&league::Duo> = l.duos.iter().filter(|d| !l.with_now.iter().any(|n| n.puuid == d.puuid)).collect();
    if others.is_empty() && !in_game {
        let msg = if wins + losses == 0 { "Your record with friends shows here after your games are saved." } else { "Play with friends (or the same people 3+ times) and they show up here." };
        c.paragraph(F::Medium, msg, 17.0, ix, y + h / 2.0 - 10.0, iw, 3, 23.0, FAINT);
        return;
    }
    if !others.is_empty() {
        c.text(F::Bold, "MOST PLAYED", 11.0, ix, ry + 14.0, FAINT, Align::Left);
        ry += 22.0;
    }
    for d in others {
        if ry + 40.0 > y + h - 8.0 {
            break;
        }
        duo_row(c, ix, ry, iw, d, false);
        ry += 40.0;
    }
}

fn state_color(state: &str) -> (u32, u32) {
    match state {
        "running" => (GREEN, 0xFFFFFF),
        "paused" => (AMBER, 0x1C1500),
        "restarting" => (ORANGE, 0x1F0E07),
        "created" => (0x3A3A42, TEXT),
        _ => (0x2A2A31, MUTED),
    }
}

fn bar(c: &mut Canvas, x: f32, y: f32, w: f32, frac: f32, color: u32) {
    c.rrect((x, y, w, 8.0), 4.0, 0x2A2A31, 1.0);
    if frac > 0.0 {
        c.rrect((x, y, (w * frac.min(1.0)).max(8.0), 8.0), 4.0, color, 1.0);
    }
}

fn docker_page(c: &mut Canvas, v: &View) {
    let d = v.docker;
    // Strip
    let accent = v.accent;
    if let Some(e) = &d.error {
        let pw = c.pill(M, CY + 6.0, "DOCKER UNAVAILABLE", 14.0, AMBER, 0x1C1500);
        c.fit(F::Medium, e, 18.0, W - 2.0 * M - pw - 30.0, M + pw + 16.0, CY + 27.0, AMBER, Align::Left);
    } else {
        let pw = c.pill(M, CY + 6.0, &d.engine.to_uppercase(), 14.0, accent, 0xFFFFFF);
        let running = d.containers.iter().filter(|c| c.state == "running").count();
        let info = format!(
            "{running} of {} running   ·   {} CPUs   ·   {}   ·   {} images   ·   {} volumes",
            d.containers.len(),
            d.ncpu,
            bytes(d.mem_total),
            d.images,
            d.volumes
        );
        c.text(F::Medium, &info, 18.0, M + pw + 16.0, CY + 27.0, TEXT_2, Align::Left);
        c.text(F::Mono, &d.socket, 13.0, W - M, CY + 26.0, FAINT, Align::Right);
    }
    let n = d.containers.len();
    if n == 0 {
        let top = CY + DOCKER_STRIP_H + 6.0;
        c.rrect((M, top, W - 2.0 * M, CY + CH - top), 16.0, PANEL_DEEP, 1.0);
        let msg = if d.ok { "No containers" } else { "Can't reach the Docker engine" };
        c.text(F::Medium, msg, 30.0, W / 2.0, top + 170.0, FAINT, Align::Center);
        return;
    }
    let (first, cols) = window(n, v.tile);
    for slot in 0..cols {
        let r = container_rect(slot, cols);
        let i = first + slot;
        let Some(ct) = d.containers.get(i) else {
            c.rrect(r, 14.0, PANEL_DEEP, 1.0);
            continue;
        };
        let (x, y, w, h) = r;
        let (sc, sf) = state_color(&ct.state);
        c.rrect(r, 14.0, PANEL, 1.0);
        c.rrect((x, y, 6.0, h), 3.0, sc, 1.0);
        let ix = x + 22.0;
        let iw = w - 40.0;
        c.fit(F::Bold, &ct.name, 26.0, iw, ix, y + 38.0, TEXT, Align::Left);
        c.clip(F::Mono, &ct.image, 14.0, iw, ix, y + 62.0, MUTED);
        let pw = c.pill(ix, y + 76.0, &ct.state.to_uppercase(), 13.0, sc, sf);
        c.clip(F::Medium, &ct.status, 16.0, iw - pw - 10.0, ix + pw + 10.0, y + 94.0, MUTED);
        let mut cy = y + 136.0;
        if ct.state == "running" {
            let cpu = ct.cpu.unwrap_or(0.0);
            c.text(F::Bold, "CPU", 13.0, ix, cy, FAINT, Align::Left);
            c.text(F::Mono, &format!("{cpu:.1}%"), 15.0, ix + iw, cy, TEXT, Align::Right);
            bar(c, ix, cy + 10.0, iw, cpu / (100.0 * d.ncpu.max(1) as f32), accent);
            cy += 46.0;
            if let Some(mem) = ct.mem {
                let limit = ct.mem_limit.unwrap_or(d.mem_total).max(1);
                c.text(F::Bold, "MEMORY", 13.0, ix, cy, FAINT, Align::Left);
                c.text(F::Mono, &format!("{} / {}", bytes(mem), bytes(limit)), 15.0, ix + iw, cy, TEXT, Align::Right);
                bar(c, ix, cy + 10.0, iw, mem as f32 / limit as f32, accent);
                cy += 46.0;
            }
        }
        if !ct.ports.is_empty() {
            c.text(F::Bold, "PORTS", 13.0, ix, cy, FAINT, Align::Left);
            c.clip(F::Mono, &ct.ports.join("  "), 15.0, iw, ix, cy + 22.0, TEXT_2);
        }
        c.text(F::Mono, &ct.id.chars().take(12).collect::<String>(), 13.0, ix, y + h - 16.0, FAINT, Align::Left);
        if v.level == &Level::Focus && i == v.tile {
            c.outline(r, 14.0, TEXT, 3.0);
        }
    }
    let right = n.saturating_sub(first + cols);
    if right > 0 {
        let r = container_rect(cols - 1, cols);
        let label = format!("{right} ›");
        let w = c.measure(F::Bold, &label, 16.0) + 16.0 * 1.4;
        c.pill(r.0 + r.2 - w - 8.0, r.1 + r.3 - 40.0, &label, 16.0, 0x2A2A31, TEXT);
    }
    if first > 0 {
        let r = container_rect(0, cols);
        c.pill(r.0 + 8.0, r.1 + r.3 - 40.0, &format!("‹ {first}"), 16.0, 0x2A2A31, TEXT);
    }
}

fn docker_detail(c: &mut Canvas, v: &View, ct: &docker::Container) {
    let (sc, sf) = state_color(&ct.state);
    let left = (M, CY, 520.0, CH);
    c.rrect(left, 14.0, PANEL, 1.0);
    c.rrect((M, CY, 6.0, CH), 3.0, sc, 1.0);
    let ix = M + 24.0;
    let iw = 520.0 - 44.0;
    c.fit(F::Bold, &ct.name, 32.0, iw, ix, CY + 44.0, TEXT, Align::Left);
    c.clip(F::Mono, &ct.image, 15.0, iw, ix, CY + 72.0, MUTED);
    let pw = c.pill(ix, CY + 86.0, &ct.state.to_uppercase(), 14.0, sc, sf);
    c.clip(F::Medium, &ct.status, 17.0, iw - pw - 12.0, ix + pw + 12.0, CY + 106.0, MUTED);
    let mut cy = CY + 150.0;
    if ct.state == "running" {
        let cpu = ct.cpu.unwrap_or(0.0);
        c.text(F::Bold, "CPU", 13.0, ix, cy, FAINT, Align::Left);
        c.text(F::Mono, &format!("{cpu:.1}%"), 15.0, ix + iw, cy, TEXT, Align::Right);
        bar(c, ix, cy + 10.0, iw, cpu / (100.0 * v.docker.ncpu.max(1) as f32), v.accent);
        cy += 44.0;
        if let Some(mem) = ct.mem {
            let limit = ct.mem_limit.unwrap_or(v.docker.mem_total).max(1);
            c.text(F::Bold, "MEMORY", 13.0, ix, cy, FAINT, Align::Left);
            c.text(F::Mono, &format!("{} / {}", bytes(mem), bytes(limit)), 15.0, ix + iw, cy, TEXT, Align::Right);
            bar(c, ix, cy + 10.0, iw, mem as f32 / limit as f32, v.accent);
            cy += 44.0;
        }
    }
    if !ct.ports.is_empty() {
        c.text(F::Bold, "PORTS", 13.0, ix, cy, FAINT, Align::Left);
        c.clip(F::Mono, &ct.ports.join("  "), 15.0, iw, ix, cy + 22.0, TEXT_2);
    }
    let br = docker_back();
    c.rrect(br, 10.0, 0x26262D, 1.0);
    c.text(F::Medium, "‹  Back", 17.0, br.0 + br.2 / 2.0, br.1 + 27.0, TEXT, Align::Center);
    for (r, which) in docker_actions() {
        let action = docker_button_action(&ct.state, which);
        let (label, sym, bg) = match action {
            "stop" => ("Stop", "stop.fill", 0x4A1A1A),
            "start" => ("Start", "play.fill", 0x1A3A24),
            "restart" => ("Restart", "arrow.clockwise", 0x26262D),
            "pause" => ("Pause", "pause.fill", 0x26262D),
            _ => ("Resume", "playpause.fill", 0x26262D),
        };
        c.rrect(r, 10.0, bg, 1.0);
        c.symbol(sym, 16.0, r.0 + 26.0, r.1 + r.3 / 2.0, TEXT);
        c.text(F::Medium, label, 17.0, r.0 + 46.0, r.1 + 27.0, TEXT, Align::Left);
    }

    // Logs
    let rr = (M + 520.0 + G, CY, W - 2.0 * M - 520.0 - G, CH);
    let (rx, ry, rw, rh) = rr;
    c.rrect(rr, 14.0, PANEL_DEEP, 1.0);
    c.text(F::Bold, "LOGS", 12.0, rx + 24.0, ry + 28.0, FAINT, Align::Left);
    c.text(F::Medium, "newest at the bottom", 13.0, rx + rw - 24.0, ry + 28.0, FAINT, Align::Right);
    let lines: &[String] = match &v.docker.logs {
        Some((id, l)) if id == &ct.id => l,
        _ => &[],
    };
    if lines.is_empty() {
        c.text(F::Medium, "No logs yet", 22.0, rx + rw / 2.0, ry + rh / 2.0, FAINT, Align::Center);
        return;
    }
    let max_scroll = lines.len().saturating_sub(LOG_ROWS);
    let scroll = v.scroll.min(max_scroll);
    let end = lines.len() - scroll;
    let start = end.saturating_sub(LOG_ROWS);
    for (row, line) in lines[start..end].iter().enumerate() {
        let by = ry + 46.0 + row as f32 * LOG_ROW_H + 15.0;
        let lower = line.to_lowercase();
        let col = if lower.contains("error") || lower.contains("fatal") || lower.contains("panic") {
            0xFF9B9B
        } else if lower.contains("warn") {
            0xE6B673
        } else {
            0xC9C9D1
        };
        c.clip(F::Mono, line, 14.0, rw - 48.0, rx + 24.0, by, col);
    }
}

// ---------------------------------------------------------------- OPS

/// What the knob has selected on the OPS page, for the breadcrumb.
fn ops_item_name(o: &ops::Status, tile: usize) -> Option<String> {
    if tile == 0 {
        return Some("WARP".into());
    }
    if let Some(h) = o.hosts.get(tile - 1) {
        return Some(h.name.clone());
    }
    o.sites.get(tile - 1 - o.hosts.len()).map(|s| site_host(&s.url).to_string())
}

fn site_host(url: &str) -> &str {
    url.trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/')
}

/// Bar color: the page accent, amber past 85%, red past 95%.
fn usage_color(frac: f32, accent: u32) -> u32 {
    if frac > 0.95 {
        RED
    } else if frac > 0.85 {
        AMBER
    } else {
        accent
    }
}

fn ops_page(c: &mut Canvas, v: &View) {
    let o = v.ops;
    let focus = v.level != &Level::Top;
    // Strip: WARP toggle, AppGate, public IP and Cloudflare colo
    let wr = warp_button();
    let (label, bg, fg, sym) = match o.net.warp {
        Some(true) => ("WARP  ON", v.accent, 0x0B1F19, "lock.shield.fill"),
        Some(false) => ("WARP  OFF", 0x26262D, TEXT_2, "shield.slash"),
        None => ("WARP  ?", 0x1C1C21, FAINT, "shield"),
    };
    c.rrect(wr, 12.0, bg, 1.0);
    c.symbol(sym, 20.0, wr.0 + 30.0, wr.1 + wr.3 / 2.0, fg);
    c.text(F::Bold, label, 18.0, wr.0 + 54.0, wr.1 + 28.0, fg, Align::Left);
    if focus && v.tile == 0 {
        c.outline(wr, 12.0, TEXT, 3.0);
    }
    let mut x = wr.0 + wr.2 + 16.0;
    if o.net.appgate {
        x += c.pill(x, CY + 8.0, "APPGATE", 14.0, 0x1A3A5C, 0xCFE3FF) + 14.0;
    }
    let ip = match (o.net.ip.as_str(), o.net.colo.as_str()) {
        ("", _) => "no internet".to_string(),
        (ip, "") => ip.to_string(),
        (ip, colo) => format!("{ip}   ·   {colo}"),
    };
    c.text(F::Mono, &ip, 17.0, x, CY + 27.0, TEXT_2, Align::Left);
    let up = o.hosts.iter().filter(|h| h.up).count();
    let ok_sites = o.sites.iter().filter(|s| s.code.is_some_and(|code| code < 400)).count();
    let summary = format!("{up} of {} hosts up   ·   {ok_sites} of {} sites OK", o.hosts.len(), o.sites.len());
    c.text(F::Medium, &summary, 17.0, W - M, CY + 27.0, MUTED, Align::Right);

    // Hosts
    let (first, cols) = host_window(o.hosts.len(), v.tile);
    for slot in 0..cols {
        let r = host_rect(slot, cols);
        let i = first + slot;
        match o.hosts.get(i) {
            Some(h) => {
                host_card(c, v, h, r);
                if focus && v.tile == i + 1 {
                    c.outline(r, 14.0, TEXT, 3.0);
                }
            }
            None => {
                c.rrect(r, 14.0, PANEL_DEEP, 1.0);
                if o.hosts.is_empty() && slot == 0 {
                    c.text(F::Medium, "No hosts configured", 20.0, r.0 + r.2 / 2.0, r.1 + r.3 / 2.0, FAINT, Align::Center);
                }
            }
        }
    }
    if first > 0 {
        let r = host_rect(0, cols);
        c.pill(r.0 + 8.0, r.1 + r.3 - 40.0, &format!("‹ {first}"), 16.0, 0x2A2A31, TEXT);
    }
    let right = o.hosts.len().saturating_sub(first + cols);
    if right > 0 {
        let r = host_rect(cols - 1, cols);
        let label = format!("{right} ›");
        let w = c.measure(F::Bold, &label, 16.0) + 16.0 * 1.4;
        c.pill(r.0 + r.2 - w - 8.0, r.1 + r.3 - 40.0, &label, 16.0, 0x2A2A31, TEXT);
    }

    // Sites
    let p = sites_panel();
    c.rrect(p, 14.0, PANEL_DEEP, 1.0);
    c.text(F::Bold, "SITES", 12.0, p.0 + 22.0, p.1 + 26.0, FAINT, Align::Left);
    c.text(F::Bold, "STATUS      TIME        TLS", 12.0, p.0 + p.2 - 22.0, p.1 + 26.0, FAINT, Align::Right);
    if o.sites.is_empty() {
        c.text(F::Medium, "No sites configured", 20.0, p.0 + p.2 / 2.0, p.1 + p.3 / 2.0, FAINT, Align::Center);
    }
    let first = site_window(o, v.tile);
    for slot in 0..SITE_ROWS {
        let i = first + slot;
        let Some(s) = o.sites.get(i) else { break };
        let r = site_rect(slot);
        let selected = focus && v.tile == 1 + o.hosts.len() + i;
        if selected {
            c.rrect(r, 10.0, 0x22222A, 1.0);
            c.outline(r, 10.0, TEXT, 2.0);
        } else if slot > 0 {
            c.rect((r.0 + 14.0, r.1, r.2 - 28.0, 1.0), LINE, 0.7);
        }
        let mid = r.1 + r.3 / 2.0;
        let (dot, code) = match s.code {
            Some(code) if code < 400 => (GREEN, code.to_string()),
            Some(code) if code < 500 => (AMBER, code.to_string()),
            Some(code) => (RED, code.to_string()),
            None => (RED, "ERR".to_string()),
        };
        c.circle(r.0 + 22.0, mid, 6.0, dot, 1.0);
        let rx = r.0 + r.2 - 14.0;
        match s.tls_days {
            Some(d) => {
                let col = if d < 3 { RED } else if d < 14 { AMBER } else { MUTED };
                c.text(F::Mono, &format!("{d}d"), 16.0, rx, mid + 6.0, col, Align::Right);
            }
            None => {
                c.text(F::Mono, "—", 16.0, rx, mid + 6.0, FAINT, Align::Right);
            }
        }
        match &s.error {
            Some(e) if s.code.is_none() => {
                c.text(F::Mono, &code, 16.0, rx - 196.0, mid + 6.0, dot, Align::Right);
                c.clip(F::Medium, e, 13.0, 150.0, rx - 180.0, mid + 5.0, FAINT);
            }
            _ => {
                c.text(F::Mono, &code, 16.0, rx - 196.0, mid + 6.0, if dot == GREEN { TEXT } else { dot }, Align::Right);
                if let Some(ms) = s.ms {
                    let col = if ms > 2000 { AMBER } else { TEXT_2 };
                    c.text(F::Mono, &format!("{ms} ms"), 16.0, rx - 82.0, mid + 6.0, col, Align::Right);
                }
            }
        }
        c.clip(F::Bold, site_host(&s.url), 19.0, r.2 - 40.0 - 250.0, r.0 + 40.0, mid + 7.0, TEXT);
    }
}

fn host_card(c: &mut Canvas, v: &View, h: &ops::Host, r: R) {
    let (x, y, w, hh) = r;
    c.rrect(r, 14.0, PANEL, 1.0);
    c.rrect((x, y, 6.0, hh), 3.0, if h.up { GREEN } else { 0x6B2A2A }, 1.0);
    let ix = x + 22.0;
    let iw = w - 40.0;
    c.fit(F::Bold, &h.name, 26.0, iw, ix, y + 38.0, if h.up { TEXT } else { TEXT_2 }, Align::Left);
    c.clip(F::Mono, &h.address, 14.0, iw, ix, y + 61.0, MUTED);
    let pill = match (h.up, h.latency_ms) {
        (true, Some(ms)) => format!("UP  ·  {ms} ms"),
        (true, None) => "UP".to_string(),
        _ => "DOWN".to_string(),
    };
    let pw = c.pill(ix, y + 74.0, &pill, 13.0, if h.up { GREEN } else { 0x4A1A1A }, if h.up { 0xFFFFFF } else { 0xFFD0D0 });
    if !h.up {
        let why = h.reason.as_deref().unwrap_or("checking…");
        c.paragraph(F::Medium, why, 18.0, ix, y + 150.0, iw, 3, 24.0, FAINT);
        let via = if h.proxied { "via ProxyCommand" } else { "SSH port" };
        c.text(F::Medium, via, 14.0, ix, y + hh - 18.0, FAINT, Align::Left);
        return;
    }
    // "3 weeks, 2 days, 4 hours" -> "3 weeks, 2 days"
    let uptime: Vec<&str> = h.uptime.split(", ").take(2).collect();
    if !h.uptime.is_empty() {
        c.clip(F::Medium, &format!("up {}", uptime.join(", ")), 15.0, iw - pw - 10.0, ix + pw + 10.0, y + 92.0, MUTED);
    }
    if let Some(e) = &h.stats_error {
        c.paragraph(F::Medium, e, 15.0, ix, y + 140.0, iw, 3, 20.0, AMBER);
        return;
    }
    let mut cy = y + 134.0;
    let mut metric = |c: &mut Canvas, label: &str, value: String, frac: f32| {
        c.text(F::Bold, label, 13.0, ix, cy, FAINT, Align::Left);
        c.fit(F::Mono, &value, 15.0, iw - 50.0, ix + iw, cy, TEXT, Align::Right);
        bar(c, ix, cy + 10.0, iw, frac, usage_color(frac, v.accent));
        cy += 46.0;
    };
    if let Some(load) = h.load {
        let cpus = h.cpus.unwrap_or(1).max(1);
        metric(c, "LOAD", format!("{load:.2}  /  {cpus} cpu"), load / cpus as f32);
    }
    if let Some((used, total)) = h.mem {
        metric(c, "MEMORY", format!("{} / {}", bytes(used), bytes(total)), used as f32 / total.max(1) as f32);
    }
    if let Some((used, total)) = h.disk {
        metric(c, "DISK", format!("{} / {}", bytes(used), bytes(total)), used as f32 / total.max(1) as f32);
    }
    if let Some((util, mem_used, mem_total, temp)) = h.gpu {
        metric(c, "GPU", format!("{util:.0}%  ·  {:.0}/{:.0} GB  ·  {temp:.0}°C", mem_used / 1024.0, mem_total / 1024.0), util / 100.0);
    }
}

// ---------------------------------------------------------------- DASHBOARD

/// What one dashboard tile says about its page.
struct Brief {
    /// Health: green fine, amber look, red broken, faint nothing going on.
    dot: Option<u32>,
    big: String,
    big_color: u32,
    lines: Vec<(String, u32)>,
    /// A thin progress bar (music position).
    bar: Option<f32>,
}

impl Brief {
    fn new(big: impl Into<String>, big_color: u32) -> Brief {
        Brief { dot: None, big: big.into(), big_color, lines: Vec::new(), bar: None }
    }
    fn line(mut self, text: impl Into<String>, color: u32) -> Brief {
        let t = text.into();
        if !t.is_empty() {
            self.lines.push((t, color));
        }
        self
    }
    fn dot(mut self, color: u32) -> Brief {
        self.dot = Some(color);
        self
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn brief(v: &View, p: Page, accent: u32) -> Brief {
    match p {
        Page::Ai => {
            let count = |st: State| v.sessions.iter().filter(|s| s.state == st).count();
            let (waiting, working, done) = (count(State::Waiting), count(State::Working), count(State::Done));
            let names = |st: State| v.sessions.iter().filter(|s| s.state == st).map(|s| if s.machine.is_empty() { s.name.clone() } else { format!("{} ({})", s.name, s.machine) }).collect::<Vec<_>>().join(", ");
            let latest = v.sessions.iter().filter(|s| s.state == State::Working).max_by_key(|s| s.updated_at).and_then(|s| s.current_step().map(|st| format!("{}  ›  {} {}", s.name, st.tag, st.text)));
            if v.sessions.is_empty() {
                Brief::new("No sessions", FAINT).line("Claude sessions show up when you send a prompt", FAINT).dot(FAINT)
            } else if waiting > 0 {
                Brief::new(format!("{waiting} need{} you", if waiting == 1 { "s" } else { "" }), AMBER)
                    .line(names(State::Waiting), TEXT_2)
                    .line(if working > 0 { format!("{working} working  ·  {done} done") } else { format!("{done} done") }, MUTED)
                    .dot(AMBER)
            } else if working > 0 {
                Brief::new(format!("{working} working"), ORANGE).line(names(State::Working), TEXT_2).line(latest.unwrap_or_default(), MUTED).dot(ORANGE)
            } else {
                Brief::new("All quiet", TEXT_2).line(plural(v.sessions.len(), "session", "sessions") + " idle or done", MUTED).dot(GREEN)
            }
        }
        Page::Music => match v.track {
            Some(t) => {
                let mut b = Brief::new(t.title.clone(), TEXT).line(t.artist.clone(), TEXT_2).line(format!("{}  ·  {}", t.app, if t.playing { "playing" } else { "paused" }), MUTED).dot(if t.playing { SPOTIFY } else { FAINT });
                if t.duration > 0.0 {
                    b.bar = Some((v.position / t.duration) as f32);
                }
                b
            }
            None => Brief::new("Nothing playing", FAINT).line(v.problem.unwrap_or(""), MUTED).dot(FAINT),
        },
        Page::Shortcuts => Brief::new(plural(v.shortcuts.len(), "shortcut", "shortcuts"), TEXT_2),
        Page::Soundboard => {
            let s = v.sounds;
            let playing: Vec<&str> = s.playing.iter().filter_map(|&i| s.clips.get(i)).map(|c| c.title.as_str()).collect();
            let app = s.listener.as_deref().map(|l| crate::symbols::running_app_name(l).unwrap_or_else(|| l.rsplit('.').next().unwrap_or(l).to_string()));
            let b = if let Some(e) = &s.error {
                Brief::new("Audio problem", RED).line(e.clone(), AMBER).dot(RED)
            } else if s.live {
                Brief::new(match app { Some(a) => format!("Live in {a}"), None => "Live".into() }, GREEN).dot(GREEN)
            } else {
                Brief::new("Not on a call", TEXT_2).dot(FAINT)
            };
            let b = b.line(if playing.is_empty() { plural(s.clips.len(), "clip", "clips") } else { format!("playing  {}", playing.join(", ")) }, if playing.is_empty() { MUTED } else { TEXT });
            if s.mic_muted {
                b.line("mic muted", RED)
            } else {
                b
            }
        }
        Page::League => {
            let l = v.league;
            let auto = if v.league_cfg.auto_accept { ("auto-accept on", GREEN) } else { ("auto-accept off", MUTED) };
            if !l.running {
                return Brief::new("Client closed", FAINT).line(auto.0, auto.1).dot(FAINT);
            }
            let queue = l.queue.clone().unwrap_or_default();
            let (big, col, dot) = match l.phase.as_str() {
                "Lobby" => ("In lobby".to_string(), TEXT, GREEN),
                "Matchmaking" => (format!("In queue  {}", mmss(l.search.map(|s| s.0).unwrap_or(0.0))), GOLD, GOLD),
                "ReadyCheck" => ("Match found".to_string(), GOLD, GOLD),
                "ChampSelect" => ("Champ select".to_string(), GOLD, GOLD),
                "InProgress" | "GameStart" => ("In game".to_string(), TEXT, GREEN),
                "Reconnect" => ("Reconnect needed".to_string(), AMBER, AMBER),
                "WaitingForStats" | "PreEndOfGame" | "EndOfGame" => ("Game over".to_string(), TEXT_2, GREEN),
                _ => ("Home screen".to_string(), TEXT_2, GREEN),
            };
            Brief::new(big, col).line(queue, TEXT_2).line(auto.0, auto.1).dot(dot)
        }
        Page::Docker => {
            let d = v.docker;
            if let Some(e) = &d.error {
                return Brief::new("Engine unreachable", RED).line(e.clone(), AMBER).dot(RED);
            }
            let running = d.containers.iter().filter(|c| c.state == "running").count();
            let restarting: Vec<&str> = d.containers.iter().filter(|c| c.state == "restarting").map(|c| c.name.as_str()).collect();
            let stopped: Vec<&str> = d.containers.iter().filter(|c| c.state == "exited" || c.state == "dead").map(|c| c.name.as_str()).collect();
            let b = Brief::new(format!("{running} of {} running", d.containers.len()), if running > 0 { TEXT } else { TEXT_2 }).line(d.engine.clone(), MUTED);
            if !restarting.is_empty() {
                b.line(format!("restarting  {}", restarting.join(", ")), AMBER).dot(AMBER)
            } else if !stopped.is_empty() {
                b.line(format!("stopped  {}", stopped.join(", ")), MUTED).dot(GREEN)
            } else {
                b.dot(if d.ok { GREEN } else { FAINT })
            }
        }
        Page::Ops => {
            let o = v.ops;
            let down: Vec<&str> = o.hosts.iter().filter(|h| !h.up).map(|h| h.name.as_str()).collect();
            let failing: Vec<&str> = o.sites.iter().filter(|s| !s.code.is_some_and(|c| c < 400)).map(|s| site_host(&s.url)).collect();
            let net = format!(
                "WARP {}{}   ·   {}",
                match o.net.warp {
                    Some(true) => "on",
                    Some(false) => "off",
                    None => "?",
                },
                if o.net.appgate { "  ·  AppGate" } else { "" },
                if o.net.ip.is_empty() { "no internet".to_string() } else { format!("{}  {}", o.net.ip, o.net.colo) }
            );
            let up = o.hosts.len() - down.len();
            let summary = format!("{up}/{} hosts up  ·  {}/{} sites OK", o.hosts.len(), o.sites.len() - failing.len(), o.sites.len());
            if !failing.is_empty() {
                Brief::new(format!("{} failing", plural(failing.len(), "site", "sites")), RED).line(failing.join(", "), TEXT_2).line(net, MUTED).dot(RED)
            } else if !down.is_empty() {
                Brief::new(format!("{} down", plural(down.len(), "host", "hosts")), AMBER).line(summary, TEXT_2).line(net, MUTED).dot(AMBER)
            } else if o.hosts.is_empty() && o.sites.is_empty() {
                Brief::new("Nothing to watch", FAINT).line(net, MUTED).dot(FAINT)
            } else {
                Brief::new("All up", GREEN).line(summary, TEXT_2).line(net, MUTED).dot(GREEN)
            }
        }
        Page::Code => {
            let g = v.code;
            if !g.ok && !g.gitlab_ok {
                return Brief::new(if g.error.is_some() { "Can't reach GitHub" } else { "Loading…" }, MUTED).line(g.error.clone().unwrap_or_default(), AMBER).dot(FAINT);
            }
            let failing = g.mine.iter().filter(|p| matches!(p.ci.as_str(), "FAILURE" | "ERROR")).count();
            let conflicts = g.mine.iter().filter(|p| p.mergeable == "CONFLICTING").count();
            let approved = g.mine.iter().filter(|p| p.review == "APPROVED").count();
            let r = g.reviews.len();
            let b = if r > 0 {
                Brief::new(format!("{} waiting on you", plural(r, "review", "reviews")), accent).dot(accent)
            } else {
                Brief::new(plural(g.mine.len(), "open PR", "open PRs"), TEXT).dot(if failing > 0 { RED } else { GREEN })
            };
            let b = b.line(
                format!("{} open  ·  {failing} failing  ·  {conflicts} conflicting  ·  {approved} approved", g.mine.len()),
                if failing > 0 { 0xFF9B9B } else { TEXT_2 },
            );
            match &g.gitlab_error {
                Some(e) if e.contains("not signed in") => b.line("GitLab: glab auth login", AMBER),
                _ => b,
            }
        }
        Page::Digin => {
            let d = v.digin;
            if d.accounts.is_empty() {
                return Brief::new("Add a Digin server", FAINT).dot(FAINT);
            }
            if !d.signed_in() {
                return Brief::new("Not signed in", FAINT).dot(FAINT);
            }
            let mentions = d.inbox.iter().filter(|n| n.kind == "mentioned").count() as u32 + d.chat_mentions;
            if mentions > 0 {
                Brief::new(format!("{mentions} mention{}", if mentions == 1 { "" } else { "s" }), accent).dot(accent)
            } else if !d.inbox.is_empty() {
                Brief::new(format!("{} new", d.inbox.len()), accent).dot(accent)
            } else if !d.issues.is_empty() {
                Brief::new(plural(d.issues.len(), "issue open", "issues open"), TEXT).dot(GREEN)
            } else {
                Brief::new("All caught up", GREEN).dot(GREEN)
            }
        }
        Page::Dash | Page::Settings | Page::Focus | Page::Deploy | Page::Paste | Page::Windows | Page::Calendar | Page::Health | Page::Stocks => Brief::new("", TEXT),
    }
}

fn dash_page(c: &mut Canvas, v: &View) {
    let tiles = dash_pages(v.pages);
    if tiles.is_empty() {
        c.rrect((M, CY, W - 2.0 * M, CH), 16.0, PANEL_DEEP, 1.0);
        c.text(F::Medium, "No other pages to summarize", 30.0, W / 2.0, CY + CH / 2.0, FAINT, Align::Center);
        return;
    }
    for (i, p) in tiles.iter().copied().enumerate() {
        let r = dash_rect(i, tiles.len());
        let (x, y, w, h) = r;
        let color = crate::config::hex(v.ring.color_for(p), v.accent);
        let b = brief(v, p, color);
        let new = v.notice.is_some_and(|(_, _, q)| q == p);
        c.rrect(r, 16.0, PANEL, 1.0);
        c.rrect((x, y, 6.0, h), 3.0, color, 1.0);
        c.symbol(page_symbol(p), 17.0, x + 34.0, y + 30.0, color);
        c.text(F::Bold, p.label(), 14.0, x + 54.0, y + 35.0, color, Align::Left);
        let mut rx = x + w - 20.0;
        if let Some(d) = b.dot {
            c.circle(rx - 6.0, y + 30.0, 7.0, d, 1.0);
            rx -= 26.0;
        }
        if new {
            let pw = c.measure(F::Bold, "NEW", 12.0) + 12.0 * 1.4;
            c.pill(rx - pw, y + 19.0, "NEW", 12.0, color, 0x101013);
        }
        // Each tile draws its own picture of the page below the header.
        let inner = (x + 24.0, y + 50.0, w - 44.0, h - 62.0);
        match p {
            Page::Ai => dash_ai(c, v, inner, &b),
            Page::Music => dash_music(c, v, inner, color),
            Page::Shortcuts => dash_shortcuts(c, v, inner),
            Page::Soundboard => dash_sounds(c, v, inner),
            Page::League => dash_league(c, v, inner),
            Page::Docker => dash_docker(c, v, inner, color, &b),
            Page::Ops => dash_ops(c, v, inner, &b),
            Page::Code => dash_code(c, v, inner, color, &b),
            Page::Digin => dash_digin(c, v, inner, color, &b),
            Page::Dash | Page::Settings | Page::Focus | Page::Deploy | Page::Paste | Page::Windows | Page::Calendar | Page::Health | Page::Stocks => {}
        }
        if v.level != &Level::Top && v.tile == i {
            c.outline(r, 16.0, TEXT, 3.0);
        } else if new {
            c.outline(r, 16.0, color, 2.0);
        }
    }
}

// ---------------------------------------------------------------- dashboard tiles

/// How many samples the dashboard graphs keep.
pub const HISTORY: usize = 40;

/// Recent samples behind the dashboard's graphs (the app fills it as data comes in).
#[derive(Debug, Default)]
pub struct History {
    /// Claude steps taken in each 30-second slice, oldest first.
    pub ai_steps: VecDeque<f32>,
    /// CPU % per container id.
    pub docker_cpu: HashMap<String, VecDeque<f32>>,
    /// Response time (ms, 0 = failed) per site URL.
    pub site_ms: HashMap<String, VecDeque<f32>>,
}

impl History {
    pub fn push(q: &mut VecDeque<f32>, v: f32) {
        q.push_back(v);
        while q.len() > HISTORY {
            q.pop_front();
        }
    }
}

/// A line graph with a soft fill; the newest value is the dot at the right.
fn sparkline(c: &mut Canvas, r: R, values: &[f32], floor_max: f32, color: u32) {
    let (x, y, w, h) = r;
    if values.len() < 2 {
        c.rrect((x, y + h - 2.0, w, 2.0), 1.0, LINE, 1.0);
        return;
    }
    let max = values.iter().cloned().fold(floor_max, f32::max).max(1e-3);
    let n = values.len();
    let pts: Vec<(f32, f32)> = values.iter().enumerate().map(|(i, v)| (x + w * i as f32 / (n - 1) as f32, y + h - (h - 2.0) * (v / max).clamp(0.0, 1.0))).collect();
    let mut area = pts.clone();
    area.push((x + w, y + h));
    area.push((x, y + h));
    c.poly_alpha(&area, color, 0.16);
    c.polyline(&pts, color, 2.0);
    let (lx, ly) = pts[n - 1];
    c.circle(lx, ly, 3.2, color, 1.0);
}

/// Bars in fixed slots, newest at the right (empty slots on the left until it fills).
fn bars(c: &mut Canvas, r: R, values: &[f32], slots: usize, color: u32) {
    let (x, y, w, h) = r;
    let bw = w / slots as f32;
    let max = values.iter().cloned().fold(1.0, f32::max);
    let offset = slots.saturating_sub(values.len());
    for i in 0..slots {
        let bx = x + i as f32 * bw + 1.0;
        match i.checked_sub(offset).and_then(|j| values.get(j)) {
            Some(&v) if v > 0.0 => {
                let bh = (h * v / max).max(3.0);
                c.rrect((bx, y + h - bh, (bw - 2.0).max(1.0), bh), 1.5, color, 1.0);
            }
            _ => c.rrect((bx, y + h - 2.0, (bw - 2.0).max(1.0), 2.0), 1.0, LINE, 1.0),
        }
    }
}

/// A small pill with a colored dot and a label; returns its width.
fn chip(c: &mut Canvas, x: f32, y: f32, dot: u32, label: &str, bg: u32, fg: u32, max_w: f32) -> f32 {
    let tw = c.measure(F::Medium, label, 13.0).min(max_w - 30.0).max(0.0);
    let w = tw + 30.0;
    c.rrect((x, y, w, 24.0), 12.0, bg, 1.0);
    c.circle(x + 12.0, y + 12.0, 4.5, dot, 1.0);
    c.clip(F::Medium, label, 13.0, tw, x + 22.0, y + 17.0, fg);
    w
}

fn dash_ai(c: &mut Canvas, v: &View, r: R, b: &Brief) {
    let (x, y, w, h) = r;
    c.fit(F::Bold, &b.big, 28.0, w, x, y + 26.0, b.big_color, Align::Left);
    // One chip per session, waiting first.
    let mut order: Vec<&Session> = v.sessions.iter().collect();
    order.sort_by_key(|s| match s.state {
        State::Waiting => 0,
        State::Working => 1,
        State::Done => 2,
        State::Idle => 3,
    });
    let mut cx = x;
    let mut shown = 0;
    for s in &order {
        let st = state_style(s.state);
        let label = if s.machine.is_empty() { s.name.clone() } else { format!("{} · {}", s.name, s.machine) };
        let need = c.measure(F::Medium, &label, 13.0).min(150.0) + 30.0;
        if cx + need > x + w - 40.0 && shown > 0 {
            let more = order.len() - shown;
            c.text(F::Medium, &format!("+{more}"), 13.0, cx + 4.0, y + 54.0, MUTED, Align::Left);
            break;
        }
        let bg = if s.state == State::Waiting { 0x3A2C08 } else { 0x22222A };
        cx += chip(c, cx, y + 37.0, st.accent, &label, bg, if s.state == State::Idle { MUTED } else { TEXT }, 180.0) + 6.0;
        shown += 1;
    }
    // The newest thing a working session said.
    if let Some(s) = v.sessions.iter().filter(|s| s.state == State::Working && !s.comment.is_empty()).max_by_key(|s| s.updated_at) {
        c.symbol("sparkle", 12.0, x + 6.0, y + 78.0, ORANGE);
        c.clip(F::Medium, &s.comment, 14.0, w - 20.0, x + 18.0, y + 83.0, 0xF0C9B2);
    }
    // Activity: steps per 30 s over the last 20 minutes.
    let gr = (x, y + h - 46.0, w, 40.0);
    let steps: Vec<f32> = v.history.ai_steps.iter().copied().collect();
    if steps.iter().all(|s| *s == 0.0) && steps.len() < 4 {
        c.text(F::Medium, "activity shows up here as Claude works", 13.0, x, y + h - 10.0, FAINT, Align::Left);
    } else {
        bars(c, gr, &steps, HISTORY, ORANGE);
        c.text(F::Medium, "steps · last 20 min", 11.0, x + w, gr.1 - 4.0, FAINT, Align::Right);
    }
}

fn dash_music(c: &mut Canvas, v: &View, r: R, color: u32) {
    let (x, y, w, h) = r;
    let Some(t) = v.track else {
        c.symbol("music.note", 40.0, x + 40.0, y + h / 2.0, FAINT);
        c.text(F::Bold, "Nothing playing", 26.0, x + 86.0, y + h / 2.0 + 4.0, FAINT, Align::Left);
        if let Some(pb) = v.problem {
            c.clip(F::Medium, pb, 13.0, w - 90.0, x + 86.0, y + h / 2.0 + 28.0, MUTED);
        }
        return;
    };
    let art = h.min(128.0);
    let ar = (x, y + (h - art) / 2.0, art, art);
    match v.art {
        Some(img) => c.image(img, ar, 12.0),
        None => {
            c.rrect(ar, 12.0, 0x22222A, 1.0);
            c.symbol("music.note", 44.0, ar.0 + art / 2.0, ar.1 + art / 2.0, color);
        }
    }
    let tx = x + art + 18.0;
    let tw = w - art - 18.0;
    c.fit(F::Bold, &t.title, 22.0, tw, tx, y + 26.0, TEXT, Align::Left);
    c.fit(F::Medium, &t.artist, 16.0, tw, tx, y + 50.0, TEXT_2, Align::Left);
    c.symbol(if t.playing { "play.fill" } else { "pause.fill" }, 12.0, tx + 6.0, y + 70.0, if t.playing { SPOTIFY } else { MUTED });
    c.clip(F::Medium, &format!("{}  ·  {}", t.app, if t.playing { "playing" } else { "paused" }), 13.0, tw - 20.0, tx + 18.0, y + 75.0, MUTED);
    let frac = if t.duration > 0.0 { (v.position / t.duration) as f32 } else { 0.0 };
    bar(c, tx, y + h - 34.0, tw, frac.clamp(0.0, 1.0), color);
    c.text(F::Mono, &mmss(v.position), 13.0, tx, y + h - 8.0, MUTED, Align::Left);
    c.text(F::Mono, &format!("-{}", mmss((t.duration - v.position).max(0.0))), 13.0, tx + tw, y + h - 8.0, MUTED, Align::Right);
}

fn dash_shortcuts(c: &mut Canvas, v: &View, r: R) {
    let (x, y, w, h) = r;
    if v.shortcuts.is_empty() {
        c.text(F::Medium, "No shortcuts yet", 22.0, x, y + 40.0, FAINT, Align::Left);
        return;
    }
    let gap = 8.0;
    let cols = 8usize;
    let rows = if v.shortcuts.len() > cols { 2 } else { 1 };
    let s = ((w - gap * (cols as f32 - 1.0)) / cols as f32).min((h - gap * (rows as f32 - 1.0)) / rows as f32).min(52.0);
    let top = y + (h - (s * rows as f32 + gap * (rows as f32 - 1.0))) / 2.0;
    for (i, sc) in v.shortcuts.iter().take(cols * rows).enumerate() {
        let (cx, cy) = (x + (i % cols) as f32 * (s + gap), top + (i / cols) as f32 * (s + gap));
        let col = crate::config::hex(&sc.color, TEXT_2);
        c.rrect((cx, cy, s, s), 12.0, 0x22222A, 1.0);
        c.rrect((cx, cy + s - 3.0, s, 3.0), 1.5, col, 0.55);
        c.symbol(&sc.symbol, s * 0.42, cx + s / 2.0, cy + s / 2.0 - 1.0, col);
    }
}

fn dash_sounds(c: &mut Canvas, v: &View, r: R) {
    let (x, y, w, h) = r;
    let s = v.sounds;
    let (label, bg, fg) = if s.error.is_some() {
        ("AUDIO ERROR", 0x4A1A1A, 0xFFD0D0)
    } else if s.live {
        ("ON AIR", 0xD63A3A, 0xFFFFFF)
    } else {
        ("OFF AIR", 0x2A2A31, MUTED)
    };
    let bw = c.measure(F::Bold, label, 15.0) + 36.0;
    c.rrect((x, y + 4.0, bw, 32.0), 9.0, bg, 1.0);
    if s.live {
        c.circle(x + 14.0, y + 20.0, 4.5, 0xFFFFFF, if v.tick { 1.0 } else { 0.45 });
    }
    c.text(F::Bold, label, 15.0, x + if s.live { 24.0 } else { 18.0 }, y + 25.0, fg, Align::Left);
    let app = s.listener.as_deref().map(|l| crate::symbols::running_app_name(l).unwrap_or_else(|| l.rsplit('.').next().unwrap_or(l).to_string()));
    let what = match (&s.error, &app) {
        (Some(e), _) => e.clone(),
        (None, Some(a)) if s.live => format!("in {a}"),
        _ => "not on a call".into(),
    };
    c.fit(F::Medium, &what, 17.0, w - bw - 60.0, x + bw + 14.0, y + 26.0, if s.live { TEXT } else { MUTED }, Align::Left);
    c.symbol(if s.mic_muted { "mic.slash.fill" } else { "mic.fill" }, 18.0, x + w - 12.0, y + 20.0, if s.mic_muted { RED } else { MUTED });
    // The clips, the ones playing ringed.
    let d = 40.0;
    let n = ((w + 8.0) / (d + 8.0)) as usize;
    for (i, clip) in s.clips.iter().take(n).enumerate() {
        let cx = x + d / 2.0 + i as f32 * (d + 8.0);
        let cy = y + 72.0;
        let col = crate::config::hex(&clip.color, 0x4C8DFF);
        let playing = s.playing.contains(&i);
        if playing {
            c.circle(cx, cy, d / 2.0 + 4.0, col, 0.35);
        }
        c.circle(cx, cy, d / 2.0, col, if playing { 1.0 } else { 0.8 });
        let sym = if clip.symbol.is_empty() { "waveform" } else { clip.symbol.as_str() };
        c.symbol(sym, 16.0, cx, cy, 0xFFFFFF);
    }
    let playing: Vec<&str> = s.playing.iter().filter_map(|&i| s.clips.get(i)).map(|c| c.title.as_str()).collect();
    let foot = if playing.is_empty() { format!("{} clips", s.clips.len()) } else { format!("playing  {}", playing.join(", ")) };
    c.clip(F::Medium, &foot, 13.0, w, x, y + h - 6.0, if playing.is_empty() { FAINT } else { TEXT_2 });
}

fn dash_league(c: &mut Canvas, v: &View, r: R) {
    let (x, y, w, h) = r;
    let l = v.league;
    let accent = mode_theme(l.mode).1;
    let steps = ["Lobby", "Queue", "Found", "Pick", "Game"];
    let at: i32 = match l.phase.as_str() {
        "Lobby" => 0,
        "Matchmaking" => 1,
        "ReadyCheck" => 2,
        "ChampSelect" => 3,
        "InProgress" | "GameStart" | "Reconnect" | "WaitingForStats" | "PreEndOfGame" | "EndOfGame" => 4,
        _ => -1,
    };
    let sy = y + 18.0;
    let span = w - 40.0;
    let px = |i: usize| x + 20.0 + span * i as f32 / (steps.len() - 1) as f32;
    c.rect((px(0), sy - 1.5, span, 3.0), 0x2A2A31, 1.0);
    if at > 0 && l.running {
        c.rect((px(0), sy - 1.5, px(at as usize) - px(0), 3.0), accent, 1.0);
    }
    for (i, name) in steps.iter().enumerate() {
        let done = l.running && (i as i32) < at;
        let now = l.running && i as i32 == at;
        if now {
            c.circle(px(i), sy, 13.0, accent, if v.tick { 0.35 } else { 0.2 });
        }
        c.circle(px(i), sy, 8.0, if done || now { accent } else { 0x3A3A42 }, 1.0);
        c.text(F::Medium, name, 12.0, px(i), sy + 26.0, if now { accent } else if done { TEXT_2 } else { FAINT }, Align::Center);
    }
    let auto = v.league_cfg.auto_accept;
    let pw = c.measure(F::Bold, if auto { "AUTO-ACCEPT" } else { "MANUAL" }, 11.0) + 11.0 * 1.4;
    c.pill(x + w - pw, y + h - 22.0, if auto { "AUTO-ACCEPT" } else { "MANUAL" }, 11.0, if auto { 0x1A3A24 } else { 0x2A2A31 }, if auto { 0x8EE0A8 } else { MUTED });
    let queue = l.queue.clone().unwrap_or_default();
    let live = l.live.as_ref();
    let (big, col, sub) = if let Some(g) = live.filter(|g| g.dead && g.result.is_none()) {
        (format!("Respawn {}", mmss(g.respawn.max(0.0).ceil())), 0xFF8A8A, format!("killed by {}", g.killed_by))
    } else if let Some(g) = live {
        (format!("{}  ·  {}/{}/{}", mmss(g.time), g.kills, g.deaths, g.assists), TEXT, format!("{}  ·  {} – {}  ·  {}", if l.mode_name.is_empty() { g.mode.clone() } else { l.mode_name.clone() }, g.ally_kills, g.enemy_kills, g.champion))
    } else if !l.running {
        ("Client closed".to_string(), FAINT, String::new())
    } else if let Some(p) = l.post_game.as_ref().filter(|_| !matches!(l.phase.as_str(), "Matchmaking" | "ReadyCheck" | "ChampSelect")) {
        let result = if p.remake { "Remake" } else if p.win { "Victory" } else { "Defeat" };
        let rec = format!("{} {}W {}L", family_name(p.mode), p.mode_record.0, p.mode_record.1);
        (format!("{result}  ·  {}/{}/{}", p.kills, p.deaths, p.assists), if p.remake { TEXT_2 } else if p.win { GREEN } else { LOSS }, [p.champion.as_str(), rec.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join("  ·  "))
    } else {
        match l.phase.as_str() {
            "Matchmaking" => {
                let (t, est) = l.search.unwrap_or((0.0, 0.0));
                (mmss(t), accent, if est > 0.0 { format!("est {}  ·  {queue}", mmss(est)) } else { queue.clone() })
            }
            "ReadyCheck" => ("Match found".to_string(), accent, queue.clone()),
            "ChampSelect" => {
                let left = l.champ.as_ref().map(|c| (c.time_left_ms / 1000.0).max(0.0).ceil()).unwrap_or(0.0);
                let champ = l.champ.as_ref().map(|c| c.champion.clone()).unwrap_or_default();
                // Your record on it: "Yone  ·  ARAM 7W 3L"
                let sub = match &l.champ_record {
                    Some(r) if !champ.is_empty() && r.games() == 0 => format!("first game on {champ}"),
                    Some(r) if !champ.is_empty() => format!("{champ}  ·  {} {}W {}L", family_name(l.mode), r.mode.0, r.mode.1),
                    _ => champ,
                };
                (format!("{left:.0}s to pick"), accent, sub)
            }
            "Lobby" => ("In lobby".to_string(), TEXT, queue.clone()),
            "InProgress" | "GameStart" => ("In game".to_string(), TEXT, queue.clone()),
            "Reconnect" => ("Reconnect".to_string(), AMBER, String::new()),
            "WaitingForStats" | "PreEndOfGame" | "EndOfGame" => ("Game over".to_string(), TEXT_2, String::new()),
            _ => ("Home".to_string(), TEXT_2, String::new()),
        }
    };
    // In a game with people you know: your record with them instead of the queue name.
    let known: Vec<String> = l.with_now.iter().filter(|d| d.games > 0).take(3).map(|d| format!("{} {}–{}", d.name, d.wins, d.games - d.wins)).collect();
    let (sub, sub_col) = if !known.is_empty() { (format!("with {}", known.join("  ·  ")), accent) } else { (sub, MUTED) };
    c.fit(F::Bold, &big, 30.0, w - pw - 16.0, x, y + h - 26.0, col, Align::Left);
    if !sub.is_empty() {
        c.clip(F::Medium, &sub, 13.0, w - pw - 16.0, x, y + h - 4.0, sub_col);
    }
}

fn dash_docker(c: &mut Canvas, v: &View, r: R, color: u32, b: &Brief) {
    let (x, y, w, h) = r;
    let d = v.docker;
    if d.error.is_some() || d.containers.is_empty() {
        c.fit(F::Bold, &b.big, 26.0, w, x, y + 30.0, b.big_color, Align::Left);
        if let Some((t, col)) = b.lines.first() {
            c.clip(F::Medium, t, 14.0, w, x, y + 56.0, *col);
        }
        return;
    }
    let mut order: Vec<&docker::Container> = d.containers.iter().collect();
    order.sort_by_key(|c| (c.state != "running", c.name.clone()));
    let row_h = 30.0;
    let rows = ((h + 4.0) / row_h) as usize;
    let name_w = 150.0;
    let val_w = 58.0;
    for (i, ct) in order.iter().take(rows).enumerate() {
        let ry = y + i as f32 * row_h;
        let (sc, _) = state_color(&ct.state);
        c.circle(x + 5.0, ry + 13.0, 5.0, sc, 1.0);
        let running = ct.state == "running";
        c.clip(F::Medium, &ct.name, 15.0, name_w, x + 18.0, ry + 19.0, if running { TEXT } else { MUTED });
        let gx = x + 18.0 + name_w + 10.0;
        let gw = w - (gx - x) - val_w - 8.0;
        if running {
            let hist: Vec<f32> = d.containers.iter().find(|c| c.id == ct.id).and(v.history.docker_cpu.get(&ct.id)).map(|q| q.iter().copied().collect()).unwrap_or_default();
            sparkline(c, (gx, ry + 2.0, gw, 22.0), &hist, 5.0, color);
            c.text(F::Mono, &format!("{:.1}%", ct.cpu.unwrap_or(0.0)), 13.0, x + w, ry + 18.0, TEXT_2, Align::Right);
        } else {
            c.clip(F::Medium, &ct.status, 13.0, gw + val_w, gx, ry + 18.0, FAINT);
        }
    }
    if order.len() > rows {
        c.text(F::Medium, &format!("+{} more", order.len() - rows), 12.0, x + w, y + h + 6.0, FAINT, Align::Right);
    }
}

fn dash_ops(c: &mut Canvas, v: &View, r: R, b: &Brief) {
    let (x, y, w, h) = r;
    let o = v.ops;
    // Network and hosts on one line.
    let (wl, wbg, wfg) = match o.net.warp {
        Some(true) => ("WARP ON", 0x1A3A30, 0x8EE0C8),
        Some(false) => ("WARP OFF", 0x2A2A31, MUTED),
        None => ("WARP ?", 0x2A2A31, FAINT),
    };
    let mut hx = x + c.pill(x, y + 2.0, wl, 11.0, wbg, wfg) + 12.0;
    for host in &o.hosts {
        let label = host.name.split('.').next().unwrap_or(&host.name);
        let need = c.measure(F::Medium, label, 13.0) + 20.0;
        if hx + need > x + w {
            break;
        }
        c.circle(hx + 4.0, y + 12.0, 4.5, if host.up { GREEN } else { RED }, 1.0);
        hx += c.text(F::Medium, label, 13.0, hx + 13.0, y + 17.0, if host.up { TEXT_2 } else { MUTED }, Align::Left) + 22.0;
    }
    if o.sites.is_empty() {
        c.fit(F::Bold, &b.big, 24.0, w, x, y + 60.0, b.big_color, Align::Left);
        return;
    }
    let row_h = 28.0;
    let top = y + 32.0;
    let rows = ((y + h - top + 4.0) / row_h) as usize;
    for (i, site) in o.sites.iter().take(rows).enumerate() {
        let ry = top + i as f32 * row_h;
        let ok = site.code.is_some_and(|code| code < 400);
        let col = if ok { GREEN } else { RED };
        c.circle(x + 5.0, ry + 12.0, 4.5, col, 1.0);
        c.clip(F::Medium, site_host(&site.url), 14.0, 150.0, x + 16.0, ry + 17.0, if ok { TEXT } else { 0xFF9B9B });
        let gx = x + 176.0;
        let gw = w - 176.0 - 118.0;
        let hist: Vec<f32> = v.history.site_ms.get(&site.url).map(|q| q.iter().copied().collect()).unwrap_or_default();
        sparkline(c, (gx, ry + 3.0, gw, 18.0), &hist, 300.0, if ok { 0x7FD1B9 } else { RED });
        let ms = match (site.ms, ok) {
            (Some(ms), true) => format!("{ms}ms"),
            _ => site.code.map(|c| c.to_string()).unwrap_or_else(|| "ERR".into()),
        };
        c.text(F::Mono, &ms, 13.0, x + w - 50.0, ry + 17.0, if ok { TEXT_2 } else { RED }, Align::Right);
        if let Some(days) = site.tls_days {
            c.text(F::Mono, &format!("{days}d"), 12.0, x + w, ry + 17.0, if days < 14 { AMBER } else { FAINT }, Align::Right);
        }
    }
}

fn dash_code(c: &mut Canvas, v: &View, r: R, accent: u32, b: &Brief) {
    let (x, y, w, h) = r;
    let g = v.code;
    c.fit(F::Bold, &b.big, 26.0, w, x, y + 26.0, b.big_color, Align::Left);
    if !g.ok && !g.gitlab_ok {
        return;
    }
    // CI across your open PRs, as one stacked bar.
    let count = |f: &dyn Fn(&github::Pr) -> bool| g.mine.iter().filter(|p| f(p)).count();
    let pass = count(&|p| p.ci == "SUCCESS");
    let fail = count(&|p| matches!(p.ci.as_str(), "FAILURE" | "ERROR"));
    let pend = count(&|p| matches!(p.ci.as_str(), "PENDING" | "EXPECTED"));
    let none = g.mine.len() - pass - fail - pend;
    let total = g.mine.len().max(1) as f32;
    let by = y + 40.0;
    c.rrect((x, by, w, 14.0), 7.0, 0x2A2A31, 1.0);
    let mut sx = x;
    for (n, col) in [(pass, GREEN), (fail, RED), (pend, AMBER), (none, 0x4A4A54)] {
        if n == 0 {
            continue;
        }
        let sw = w * n as f32 / total;
        c.rrect((sx, by, sw.max(4.0), 14.0), 7.0, col, 1.0);
        sx += sw;
    }
    let mut lx = x;
    for (n, label, col) in [(pass, "passing", GREEN), (fail, "failing", RED), (pend, "pending", AMBER), (none, "no CI", MUTED)] {
        if n == 0 {
            continue;
        }
        c.circle(lx + 4.0, by + 30.0, 4.0, col, 1.0);
        lx += c.text(F::Medium, &format!("{n} {label}"), 13.0, lx + 12.0, by + 35.0, TEXT_2, Align::Left) + 24.0;
    }
    let conflicts = g.mine.iter().filter(|p| p.mergeable == "CONFLICTING").count();
    let foot = if let Some(p) = g.reviews.first() {
        (format!("{}  {}", pr_ref(p), p.title), accent)
    } else if conflicts > 0 {
        (format!("{conflicts} with merge conflicts"), AMBER)
    } else {
        (format!("{} approved", g.mine.iter().filter(|p| p.review == "APPROVED").count()), MUTED)
    };
    c.clip(F::Medium, &foot.0, 14.0, w, x, y + h - 6.0, foot.1);
}

// ---------------------------------------------------------------- DIGIN

fn status_style(status: &str) -> (u32, u32) {
    match status {
        "in_progress" => (0x3A2C08, AMBER),
        "waiting" => (0x1A2A44, 0x8AB4F8),
        "todo" => (0x2A2A31, TEXT_2),
        "backlog" => (0x1E1E24, MUTED),
        "done" => (0x12301F, 0x6EDB9A),
        _ => (0x2A2A31, MUTED),
    }
}

/// Priority as Digin draws it: urgent is a red mark, then three, two, one bars.
fn priority_glyph(c: &mut Canvas, x: f32, cy: f32, p: u8) {
    if p == 1 {
        c.rrect((x, cy - 8.0, 16.0, 16.0), 4.0, RED, 1.0);
        c.text(F::Bold, "!", 13.0, x + 8.0, cy + 5.0, 0xFFFFFF, Align::Center);
        return;
    }
    let lit = match p {
        2 => 3,
        3 => 2,
        4 => 1,
        _ => 0,
    };
    for k in 0..3 {
        let h = 5.0 + k as f32 * 4.0;
        c.rrect((x + k as f32 * 6.0, cy + 7.0 - h, 4.0, h), 1.0, if k < lit { TEXT_2 } else { 0x3A3A42 }, 1.0);
    }
}

fn note_symbol(kind: &str) -> &'static str {
    match kind {
        "mentioned" => "at",
        "message" => "bubble.left.fill",
        "assigned" => "person.crop.circle.badge.checkmark",
        "commented" => "text.bubble.fill",
        "status" => "arrow.triangle.2.circlepath",
        "due" => "calendar.badge.exclamationmark",
        "call_missed" => "phone.down.fill",
        _ => "bell.fill",
    }
}

fn digin_page(c: &mut Canvas, v: &View) {
    if let Some(open) = dg_open(v) {
        return digin_detail(c, v, open);
    }
    let d = v.digin;
    let focus = v.level != &Level::Top;
    if d.accounts.is_empty() {
        c.rrect((M, CY, W - 2.0 * M, CH), 16.0, PANEL_DEEP, 1.0);
        c.symbol(page_symbol(Page::Digin), 46.0, W / 2.0, CY + CH / 2.0 - 60.0, v.accent);
        c.text(F::Medium, "No Digin servers yet", 34.0, W / 2.0, CY + CH / 2.0 + 10.0, TEXT_2, Align::Center);
        c.text(F::Regular, "Add one in the Digin tab of Quaked Settings: sign in with your email and a code.", 20.0, W / 2.0, CY + CH / 2.0 + 50.0, FAINT, Align::Center);
        return;
    }
    // Strip: one chip per account, chat summary on the right.
    let mut x = M;
    for a in &d.accounts {
        let (dot, fg) = match (a.signed_in, &a.error) {
            (true, None) => (GREEN, TEXT),
            (true, Some(_)) => (AMBER, TEXT_2),
            (false, _) => (FAINT, MUTED),
        };
        let who = if a.handle.is_empty() { a.label.clone() } else { format!("{}  @{}", a.label, a.handle) };
        x += chip(c, x, CY + 8.0, dot, &who, 0x22222A, fg, 360.0) + 10.0;
        if let Some(e) = &a.error {
            x += c.fit(F::Medium, e, 14.0, 300.0, x, CY + 25.0, AMBER, Align::Left) + 16.0;
        }
    }
    if d.signed_in() {
        let chat = if d.chat_unread == 0 && d.chat_mentions == 0 {
            "chat: all read".to_string()
        } else {
            format!("chat: {} unread in {}{}", d.chat_unread, plural(d.chat_channels as usize, "channel", "channels"), if d.chat_mentions > 0 { format!("  ·  {} @", d.chat_mentions) } else { String::new() })
        };
        c.text(F::Medium, &chat, 17.0, W - M, CY + 26.0, if d.chat_mentions > 0 { v.accent } else { TEXT_2 }, Align::Right);
    }
    let multi = d.accounts.len() > 1;
    let titles = ["MY ISSUES", "INBOX", "PAGES"];
    let empty = ["Nothing assigned to you", "Inbox zero", "No pages"];
    for (k, (col, first, offset, n)) in digin_lists(d, v.tile).into_iter().enumerate() {
        let (cx, cy, cw, ch) = col;
        c.rrect(col, 14.0, PANEL_DEEP, 1.0);
        c.text(F::Bold, titles[k], 12.0, cx + 22.0, cy + 26.0, FAINT, Align::Left);
        let count = if n > DG_ROWS { format!("{}–{} of {n}", first + 1, (first + DG_ROWS).min(n)) } else { n.to_string() };
        c.text(F::Bold, &count, 12.0, cx + cw - 22.0, cy + 26.0, FAINT, Align::Right);
        if n == 0 {
            c.text(F::Medium, if d.signed_in() { empty[k] } else { "" }, 19.0, cx + cw / 2.0, cy + ch / 2.0 + 10.0, FAINT, Align::Center);
            continue;
        }
        for slot in 0..DG_ROWS {
            let i = first + slot;
            if i >= n {
                break;
            }
            let r = digin_row(col, slot);
            let (rx, ry, rw, rh) = r;
            let mid = ry + rh / 2.0;
            if focus && v.tile == offset + i {
                c.rrect(r, 10.0, 0x22222A, 1.0);
                c.outline(r, 10.0, TEXT, 2.0);
            } else if slot > 0 {
                c.rect((rx + 14.0, ry, rw - 28.0, 1.0), LINE, 0.7);
            }
            let host = |acct: usize| if multi { d.accounts.get(acct).map(|a| a.label.split('.').next().unwrap_or("").to_string()).unwrap_or_default() } else { String::new() };
            match k {
                0 => {
                    let x = &d.issues[i];
                    priority_glyph(c, rx + 12.0, mid, x.priority);
                    c.clip(F::Mono, &x.key, 14.0, 92.0, rx + 40.0, mid + 5.0, MUTED);
                    let label = if x.status_name.is_empty() { x.status.replace('_', " ") } else { x.status_name.clone() };
                    let (bg, fg) = status_style(&x.status);
                    let pw = c.measure(F::Bold, &label.to_uppercase(), 11.0) + 11.0 * 1.4;
                    c.pill(rx + rw - 12.0 - pw, mid - 10.0, &label.to_uppercase(), 11.0, bg, fg);
                    c.clip(F::Medium, &x.title, 17.0, rw - 150.0 - pw, rx + 136.0, mid + 6.0, TEXT);
                }
                1 => {
                    let nt = &d.inbox[i];
                    let col = if nt.kind == "mentioned" { v.accent } else { TEXT_2 };
                    c.symbol(note_symbol(&nt.kind), 16.0, rx + 20.0, mid, col);
                    let age = age(v.now_ms.saturating_sub(nt.created));
                    let aw = c.text(F::Medium, &age, 13.0, rx + rw - 12.0, mid + 5.0, FAINT, Align::Right);
                    let h = host(nt.account);
                    let hw = if h.is_empty() { 0.0 } else { c.text(F::Medium, &h, 11.0, rx + rw - 22.0 - aw, mid + 5.0, FAINT, Align::Right) + 10.0 };
                    let room = rw - 54.0 - aw - hw - 14.0;
                    let tw = c.clip(F::Bold, &nt.title, 15.0, (room * 0.45).max(60.0), rx + 40.0, mid + 5.0, TEXT);
                    c.clip(F::Medium, &nt.body, 14.0, (room - tw - 10.0).max(0.0), rx + 50.0 + tw, mid + 5.0, MUTED);
                }
                _ => {
                    let pg = &d.pages[i];
                    c.symbol("doc.text.fill", 15.0, rx + 18.0, mid, if pg.changed { v.accent } else { MUTED });
                    let meta = format!("{}  ·  {}", pg.project, age(v.now_ms.saturating_sub(pg.updated)));
                    let mw = c.text(F::Medium, &meta, 13.0, rx + rw - 12.0, mid + 5.0, FAINT, Align::Right);
                    c.clip(F::Medium, &pg.title, 16.0, rw - 60.0 - mw, rx + 36.0, mid + 6.0, if pg.changed { TEXT } else { TEXT_2 });
                }
            }
        }
    }
}

fn dash_digin(c: &mut Canvas, v: &View, r: R, accent: u32, b: &Brief) {
    let (x, y, w, h) = r;
    let d = v.digin;
    c.fit(F::Bold, &b.big, 26.0, w, x, y + 26.0, b.big_color, Align::Left);
    if !d.signed_in() {
        c.clip(F::Medium, "Sign in on the Digin tab of Quaked Settings", 14.0, w, x, y + 54.0, FAINT);
        return;
    }
    // Three counts side by side.
    let stats = [
        (d.issues.len().to_string(), "issues", TEXT),
        (d.inbox.len().to_string(), "inbox", if d.inbox.is_empty() { TEXT } else { accent }),
        (d.chat_unread.to_string(), if d.chat_mentions > 0 { "chat · @" } else { "chat" }, if d.chat_mentions > 0 { accent } else { TEXT }),
    ];
    let sw = w / 3.0;
    for (k, (n, label, col)) in stats.iter().enumerate() {
        let sx = x + k as f32 * sw;
        c.text(F::Bold, n, 26.0, sx, y + 70.0, *col, Align::Left);
        c.text(F::Medium, label, 12.0, sx, y + 88.0, FAINT, Align::Left);
    }
    let foot = d.inbox.first().map(|n| (format!("{}  {}", n.title, n.body), note_symbol(&n.kind))).or_else(|| d.issues.first().map(|i| (format!("{}  {}", i.key, i.title), "circle.dashed")));
    if let Some((text, sym)) = foot {
        c.symbol(sym, 13.0, x + 7.0, y + h - 11.0, MUTED);
        c.clip(F::Medium, &text, 14.0, w - 22.0, x + 20.0, y + h - 6.0, TEXT_2);
    }
}

// ---------------------------------------------------------------- DIGIN: one item open
//
// The knob walks one line of stops: down the text (three lines a detent), then along the
// action bar. A press on the text jumps to the actions (an issue's next status, "Mark read");
// a press on an action does it. Done and Canceled want a second press within 4 seconds.

const DG_GREEN: u32 = 0x6EDB9A;
const DG_BODY_PX: f32 = 19.0;
const DG_BODY_H: f32 = 27.0;

#[derive(Clone, Copy, PartialEq)]
enum DgDeco {
    Plain,
    Code,
    Quote,
    Rule,
    /// A table's heading row: underlined.
    Head,
}

/// One drawn line of an open item's text.
struct DgLine {
    f: F,
    px: f32,
    /// Left edge, from the text area's.
    x: f32,
    text: String,
    color: u32,
    /// Space above it (dropped when it's the first line showing), and its own height.
    pad: f32,
    h: f32,
    /// Drawn left of the first line of a list item.
    marker: Option<(String, u32)>,
    /// After the text, fainter (a comment's age).
    meta: String,
    deco: DgDeco,
    /// A table row: each cell and where it starts (instead of `text`).
    cells: Vec<(f32, String)>,
}

impl DgLine {
    fn new(f: F, px: f32, x: f32, text: String, color: u32, pad: f32, h: f32) -> DgLine {
        DgLine { f, px, x, text, color, pad, h, marker: None, meta: String::new(), deco: DgDeco::Plain, cells: Vec::new() }
    }
}

/// "just now", "4 min ago", "3 h ago", "yesterday", "5 days ago" (empty for no time).
fn dg_ago(now_ms: u64, ms: u64) -> String {
    if ms == 0 {
        return String::new();
    }
    match now_ms.saturating_sub(ms) / 60_000 {
        0 => "just now".into(),
        m @ 1..=59 => format!("{m} min ago"),
        m @ 60..=1439 => format!("{} h ago", m / 60),
        m if m < 2880 => "yesterday".into(),
        m => format!("{} days ago", m / 1440),
    }
}

/// Where each column of the table that row `i` is in starts: as wide as its widest cell,
/// squeezed to fit `w`.
fn dg_columns(fonts: &Fonts, blocks: &[digin::Block], i: usize, w: f32) -> Vec<f32> {
    let row = |b: &digin::Block| matches!(b, digin::Block::Row(..));
    let start = (0..=i).rev().take_while(|&k| row(&blocks[k])).last().unwrap_or(i);
    let end = (i..blocks.len()).take_while(|&k| row(&blocks[k])).last().unwrap_or(i);
    let mut widths: Vec<f32> = Vec::new();
    for b in &blocks[start..=end] {
        if let digin::Block::Row(cells, head) = b {
            for (k, cell) in cells.iter().enumerate() {
                let cw = fonts.measure(if *head { F::Bold } else { F::Regular }, cell, 18.0) + 40.0;
                match widths.get_mut(k) {
                    Some(old) => *old = old.max(cw),
                    None => widths.push(cw),
                }
            }
        }
    }
    let total: f32 = widths.iter().sum();
    let k = if total > w { w / total } else { 1.0 };
    widths.iter().scan(0.0, |x, cw| {
        let at = *x;
        *x += cw * k;
        Some(at)
    }).collect()
}

/// Markdown blocks as lines `w` wide.
fn dg_blocks(fonts: &Fonts, blocks: &[digin::Block], w: f32, out: &mut Vec<DgLine>) {
    use digin::Block;
    let wrap = |f: F, t: &str, px: f32, w: f32| {
        let lines = fonts.wrap(f, t, px, w.max(40.0), 2000);
        if lines.is_empty() { vec![String::new()] } else { lines }
    };
    let mut prev: Option<&Block> = None;
    for (bi, b) in blocks.iter().enumerate() {
        // Space between blocks; list items, code lines and table rows run on.
        let pad = match (prev, b) {
            (None, _) => 0.0,
            (Some(Block::Item(..)), Block::Item(..)) | (Some(Block::Code(_)), Block::Code(_)) | (Some(Block::Row(..)), Block::Row(..)) => 0.0,
            (_, Block::Heading(..)) => 16.0,
            _ => 10.0,
        };
        let first = |k: usize| if k == 0 { pad } else { 0.0 };
        match b {
            Block::Heading(level, t) => {
                let px = match level {
                    1 => 24.0,
                    2 => 21.0,
                    _ => 19.0,
                };
                for (k, l) in wrap(F::Bold, t, px, w).into_iter().enumerate() {
                    out.push(DgLine::new(F::Bold, px, 0.0, l, TEXT, first(k), (px * 1.4).round()));
                }
            }
            Block::Para(t) => {
                for (k, l) in wrap(F::Regular, t, DG_BODY_PX, w).into_iter().enumerate() {
                    out.push(DgLine::new(F::Regular, DG_BODY_PX, 0.0, l, TEXT_2, first(k), DG_BODY_H));
                }
            }
            Block::Item(marker, depth, t) => {
                let x = 30.0 + *depth as f32 * 26.0;
                let mcol = match marker.as_str() {
                    "✓" => DG_GREEN,
                    "○" => FAINT,
                    _ => MUTED,
                };
                for (k, l) in wrap(F::Regular, t, DG_BODY_PX, w - x).into_iter().enumerate() {
                    let mut line = DgLine::new(F::Regular, DG_BODY_PX, x, l, TEXT_2, first(k), DG_BODY_H);
                    if k == 0 {
                        line.marker = Some((marker.clone(), mcol));
                    }
                    out.push(line);
                }
            }
            Block::Code(t) => {
                let indent = t.chars().take_while(|c| *c == ' ').count() as f32 * fonts.measure(F::Mono, " ", 15.0);
                let x = 14.0 + indent;
                for (k, l) in wrap(F::Mono, t.trim_start(), 15.0, w - x - 14.0).into_iter().enumerate() {
                    let mut line = DgLine::new(F::Mono, 15.0, x, l, 0xE0CDA9, first(k), 23.0);
                    line.deco = DgDeco::Code;
                    out.push(line);
                }
            }
            Block::Quote(t) => {
                for (k, l) in wrap(F::Regular, t, 18.0, w - 22.0).into_iter().enumerate() {
                    let mut line = DgLine::new(F::Regular, 18.0, 22.0, l, MUTED, first(k), 26.0);
                    line.deco = DgDeco::Quote;
                    out.push(line);
                }
            }
            Block::Row(cells, head) => {
                let (f, color) = if *head { (F::Bold, TEXT) } else { (F::Regular, TEXT_2) };
                let mut line = DgLine::new(f, 18.0, 0.0, String::new(), color, pad, 30.0);
                line.cells = dg_columns(fonts, blocks, bi, w).into_iter().zip(cells.iter().cloned()).collect();
                if *head {
                    line.deco = DgDeco::Head;
                }
                out.push(line);
            }
            Block::Rule => {
                let mut line = DgLine::new(F::Regular, DG_BODY_PX, 0.0, String::new(), LINE, pad, 18.0);
                line.deco = DgDeco::Rule;
                out.push(line);
            }
            Block::Aside(t) => {
                for (k, l) in wrap(F::Medium, t, 17.0, w).into_iter().enumerate() {
                    out.push(DgLine::new(F::Medium, 17.0, 0.0, l, FAINT, first(k), 25.0));
                }
            }
        }
        prev = Some(b);
    }
}

/// Everything an open item's text panel shows, as lines `w` wide.
fn dg_lines(fonts: &Fonts, d: &digin::Detail, w: f32, now_ms: u64) -> Vec<DgLine> {
    let mut out = Vec::new();
    let quiet = |t: &str| DgLine::new(F::Medium, 18.0, 0.0, t.to_string(), FAINT, 0.0, 26.0);
    match &d.body {
        digin::Body::Issue(x) => {
            if x.description.is_empty() && !d.loading {
                out.push(quiet("No description"));
            }
            dg_blocks(fonts, &x.description, w, &mut out);
            if x.comment_count > 0 {
                let mut head = DgLine::new(F::Bold, 12.0, 0.0, format!("COMMENTS  ·  {}", x.comment_count), FAINT, if out.is_empty() { 0.0 } else { 24.0 }, 24.0);
                head.meta = "newest first".into();
                out.push(head);
                for cm in &x.comments {
                    let mut who = DgLine::new(F::Bold, 16.0, 0.0, if cm.author.is_empty() { "Someone".into() } else { cm.author.clone() }, TEXT, 10.0, 26.0);
                    who.meta = age(now_ms.saturating_sub(cm.created));
                    out.push(who);
                    let start = out.len();
                    dg_blocks(fonts, &cm.text, w - 18.0, &mut out);
                    for l in &mut out[start..] {
                        l.x += 18.0;
                    }
                }
                if x.comment_count > x.comments.len() {
                    let mut more = quiet(&format!("{} older in Digin", x.comment_count - x.comments.len()));
                    more.pad = 10.0;
                    out.push(more);
                }
            }
        }
        digin::Body::Note(n) => {
            if n.text.is_empty() {
                out.push(quiet("Nothing more to it than the line above"));
            }
            dg_blocks(fonts, &n.text, w, &mut out);
        }
        digin::Body::Page(p) => {
            if p.text.is_empty() && !d.loading {
                out.push(quiet("This page is empty"));
            }
            dg_blocks(fonts, &p.text, w, &mut out);
        }
    }
    out
}

/// The first line that can show at the top with the rest still filling the panel.
fn dg_max_scroll(lines: &[DgLine], room: f32) -> usize {
    let (mut s, mut below) = (lines.len(), 0.0);
    while s > 0 && below + lines[s - 1].h <= room + 0.5 {
        below += lines[s - 1].h + lines[s - 1].pad;
        s -= 1;
    }
    s.min(lines.len().saturating_sub(1))
}

/// How far the knob can scroll the open item's text (in lines).
pub fn digin_scroll_max(fonts: &Fonts, d: &digin::Status) -> usize {
    let Some(open) = &d.detail else { return 0 };
    let (_, _, w, h) = dg_text_area();
    dg_max_scroll(&dg_lines(fonts, open, w, 0), h)
}

fn digin_hints(v: &View) -> &'static str {
    match dg_open(v) {
        Some(d) if d.armed.is_some() => "press again  confirm   ·   turn  cancel   ·   double-click  back",
        Some(d) if d.focus.is_some() => "turn  choose   ·   press  do it   ·   double-click  back",
        _ => "turn  scroll, then actions   ·   press  actions   ·   double-click  back",
    }
}

fn priority_name(p: u8) -> &'static str {
    match p {
        1 => "Urgent",
        2 => "High",
        3 => "Medium",
        4 => "Low",
        _ => "",
    }
}

fn note_kind_label(kind: &str) -> &'static str {
    match kind {
        "mentioned" => "MENTION",
        "message" => "MESSAGE",
        "assigned" => "ASSIGNED TO YOU",
        "commented" => "COMMENT",
        "status" => "STATUS CHANGE",
        "due" => "DUE",
        "changed" => "CHANGED",
        "call_missed" => "MISSED CALL",
        _ => "NOTICE",
    }
}

fn digin_detail(c: &mut Canvas, v: &View, d: &digin::Detail) {
    dg_header(c, v, d);
    dg_text_panel(c, v, d);
    dg_action_bar(c, d);
}

/// The left panel: what the item is, and how the last action went.
fn dg_header(c: &mut Canvas, v: &View, d: &digin::Detail) {
    let r = dg_head();
    let (x, y, w, h) = r;
    c.rrect(r, 14.0, PANEL, 1.0);
    let (ix, iw) = (x + 26.0, w - 52.0);
    let host = if v.digin.accounts.len() > 1 { v.digin.accounts.get(d.account).map(|a| a.label.clone()).unwrap_or_default() } else { String::new() };
    let ago = |ms: u64| dg_ago(v.now_ms, ms);
    let join = |parts: &[&str]| parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join("  ·  ");
    match &d.body {
        digin::Body::Issue(it) => {
            let (bg, fg) = status_style(&it.status);
            c.rrect((x, y, 6.0, h), 3.0, fg, 1.0);
            let kw = c.text(F::Mono, &it.key, 17.0, ix, y + 38.0, MUTED, Align::Left);
            let mut right = x + w - 26.0;
            let pname = priority_name(it.priority);
            if !pname.is_empty() {
                let tw = c.text(F::Medium, pname, 15.0, right, y + 38.0, if it.priority == 1 { RED } else { MUTED }, Align::Right);
                priority_glyph(c, right - tw - 26.0, y + 33.0, it.priority);
                right -= tw + 40.0;
            }
            c.clip(F::Medium, &join(&[&it.project, &host]), 15.0, (right - ix - kw - 24.0).max(0.0), ix + kw + 14.0, y + 38.0, FAINT);
            let th = c.paragraph(F::Bold, &it.title, 27.0, ix, y + 80.0, iw, 3, 33.0, TEXT);
            let py = y + 80.0 + th - 12.0;
            let label = if it.status_name.is_empty() { it.status.replace('_', " ") } else { it.status_name.clone() };
            let pw = c.pill(ix, py, &label.to_uppercase(), 13.0, bg, fg);
            let who = match it.assignee.as_str() {
                "" => String::new(),
                "you" => "Assigned to you".into(),
                a => format!("Assigned to {a}"),
            };
            c.clip(F::Medium, &who, 16.0, iw - pw - 14.0, ix + pw + 14.0, py + 17.0, TEXT_2);
            let comments = if it.comment_count > 0 { plural(it.comment_count, "comment", "comments") } else { String::new() };
            let updated = if it.updated > 0 { format!("Updated {}", ago(it.updated)) } else { String::new() };
            c.clip(F::Medium, &join(&[&updated, &comments]), 15.0, iw, ix, py + 58.0, FAINT);
        }
        digin::Body::Note(n) => {
            let col = if n.kind == "mentioned" { v.accent } else { TEXT_2 };
            c.rrect((x, y, 6.0, h), 3.0, if n.read { GREEN } else { v.accent }, 1.0);
            c.symbol(note_symbol(&n.kind), 18.0, ix + 10.0, y + 32.0, col);
            let kind = note_kind_label(&n.kind);
            let kw = c.text(F::Bold, kind, 13.0, ix + 32.0, y + 38.0, FAINT, Align::Left);
            c.clip(F::Medium, &host, 13.0, (iw - kw - 50.0).max(0.0), ix + 32.0 + kw + 14.0, y + 38.0, FAINT);
            let th = c.paragraph(F::Bold, &n.title, 27.0, ix, y + 80.0, iw, 3, 33.0, TEXT);
            let mut ly = y + 80.0 + th + 4.0;
            if !n.author.is_empty() && !n.title.contains(&n.author) {
                c.clip(F::Medium, &format!("from {}", n.author), 17.0, iw, ix, ly, TEXT_2);
                ly += 28.0;
            }
            c.clip(F::Medium, &ago(n.created), 15.0, iw, ix, ly, FAINT);
        }
        digin::Body::Page(p) => {
            let col = if p.changed { v.accent } else { MUTED };
            c.rrect((x, y, 6.0, h), 3.0, col, 1.0);
            c.symbol("doc.text.fill", 17.0, ix + 8.0, y + 32.0, col);
            c.clip(F::Bold, &join(&["PAGE", &p.project.to_uppercase(), &host]), 13.0, iw - 30.0, ix + 30.0, y + 38.0, FAINT);
            let th = c.paragraph(F::Bold, &p.title, 27.0, ix, y + 80.0, iw, 3, 33.0, TEXT);
            let mut ly = y + 80.0 + th + 4.0;
            let by = if p.updated_by.is_empty() { format!("Updated {}", ago(p.updated)) } else { format!("Updated by {}  ·  {}", p.updated_by, ago(p.updated)) };
            c.clip(F::Medium, &by, 16.0, iw, ix, ly, TEXT_2);
            ly += 18.0;
            if p.changed {
                c.pill(ix, ly, "CHANGED SINCE YOU LAST READ IT", 11.0, v.accent, 0x101013);
            }
        }
    }
    dg_status_line(c, d, ix, y + h - 22.0, iw);
}

/// How the last action went, or what's waiting on you, at the foot of the left panel.
fn dg_status_line(c: &mut Canvas, d: &digin::Detail, x: f32, base: f32, w: f32) {
    let armed = d.armed.and_then(|(k, _)| d.body.actions().get(k).cloned());
    let (text, color, sym) = if let Some(b) = &d.busy {
        (b.clone(), AMBER, "arrow.triangle.2.circlepath")
    } else if let Some(digin::Action::Status(s)) = armed {
        (format!("Press again to move it to {}", s.name), AMBER, "exclamationmark.circle.fill")
    } else if let Some((s, ok)) = &d.said {
        if digin::needs_permit(s) {
            (s.clone(), AMBER, "lock.fill")
        } else if *ok {
            (s.clone(), DG_GREEN, "checkmark.circle.fill")
        } else {
            (s.clone(), RED, "xmark.octagon.fill")
        }
    } else {
        return;
    };
    let lines = c.fonts.wrap(F::Bold, &text, 16.0, w - 30.0, 3);
    let top = base - (lines.len().max(1) - 1) as f32 * 21.0;
    c.symbol(sym, 17.0, x + 9.0, top - 6.0, color);
    for (k, l) in lines.iter().enumerate() {
        c.text(F::Bold, l, 16.0, x + 28.0, top + k as f32 * 21.0, color, Align::Left);
    }
}

/// The right panel: the text, scrolled by the knob.
fn dg_text_panel(c: &mut Canvas, v: &View, d: &digin::Detail) {
    let r = dg_text();
    let (x, y, w, h) = r;
    c.rrect(r, 14.0, PANEL_DEEP, 1.0);
    if d.focus.is_none() {
        c.outline(r, 14.0, v.accent, 2.0); // the knob is on the text
    }
    let label = match &d.body {
        digin::Body::Issue(_) => "DESCRIPTION",
        digin::Body::Note(_) => "MESSAGE",
        digin::Body::Page(_) => "PAGE",
    };
    c.text(F::Bold, label, 12.0, x + 26.0, y + 30.0, FAINT, Align::Left);
    let (ax, ay, aw, ah) = dg_text_area();
    if let Some(e) = &d.error {
        let (col, sym, words) = if digin::needs_permit(e) { (AMBER, "lock.fill", e.clone()) } else { (RED, "xmark.octagon.fill", format!("Couldn't load it: {e}")) };
        c.symbol(sym, 30.0, x + w / 2.0, y + h / 2.0 - 30.0, col);
        let lines = c.fonts.wrap(F::Medium, &words, 19.0, aw - 80.0, 3);
        for (k, l) in lines.iter().enumerate() {
            c.text(F::Medium, l, 19.0, x + w / 2.0, y + h / 2.0 + 14.0 + k as f32 * 26.0, TEXT_2, Align::Center);
        }
        return;
    }
    let lines = dg_lines(c.fonts, d, aw, v.now_ms);
    if lines.is_empty() && d.loading {
        c.text(F::Medium, "Loading…", 20.0, x + w / 2.0, y + h / 2.0 + 6.0, FAINT, Align::Center);
        return;
    }
    let max = dg_max_scroll(&lines, ah);
    let scroll = v.scroll.min(max);
    let mut ty = ay;
    for (k, l) in lines.iter().enumerate().skip(scroll) {
        let top = ty + if k == scroll { 0.0 } else { l.pad };
        if top + l.h > ay + ah + 0.5 {
            break;
        }
        let base = top + (l.h + 0.72 * l.px) / 2.0;
        match l.deco {
            DgDeco::Code => c.rect((ax, top, aw, l.h), 0x1B1B22, 1.0),
            DgDeco::Quote => c.rect((ax + 4.0, top + 2.0, 3.0, l.h - 4.0), 0x3A3A44, 1.0),
            DgDeco::Rule => c.rect((ax, top + l.h / 2.0, aw, 1.0), LINE, 1.0),
            DgDeco::Head => c.rect((ax, top + l.h - 1.0, aw, 1.0), LINE, 1.0),
            DgDeco::Plain => {}
        }
        if let Some((m, col)) = &l.marker {
            // Checkboxes are drawn (the font has no ✓ or ○); bullets and numbers are text.
            let (s, mid) = (15.0, base - 0.36 * l.px);
            let (bx, by) = (ax + l.x - 12.0 - s, mid - s / 2.0);
            match m.as_str() {
                "✓" => {
                    c.rrect((bx, by, s, s), 4.0, *col, 1.0);
                    c.polyline(&[(bx + 3.5, by + 7.8), (bx + 6.5, by + 10.8), (bx + 11.5, by + 4.6)], 0x0E2416, 2.0);
                }
                "○" => c.outline((bx, by, s, s), 4.0, *col, 1.5),
                _ => {
                    c.text(F::Bold, m, l.px, ax + l.x - 10.0, base, *col, Align::Right);
                }
            }
        }
        for (k, (cx, cell)) in l.cells.iter().enumerate() {
            let next = l.cells.get(k + 1).map(|n| n.0).unwrap_or(aw);
            c.clip(l.f, cell, l.px, next - cx - 16.0, ax + cx, base, l.color);
        }
        let tw = c.text(l.f, &l.text, l.px, ax + l.x, base, l.color, Align::Left);
        if !l.meta.is_empty() {
            c.text(F::Medium, &l.meta, 14.0, ax + l.x + tw + 12.0, base, FAINT, Align::Left);
        }
        ty = top + l.h;
    }
    if d.loading {
        c.text(F::Medium, "Loading…", 13.0, x + w - 26.0, y + 30.0, FAINT, Align::Right);
    } else if max > 0 {
        // Where you are: a thin scroll bar, and whether there's more below.
        let track = (x + w - 16.0, ay, 4.0, ah);
        let thumb_h = (ah * (1.0 - max as f32 / lines.len() as f32)).clamp(28.0, ah);
        c.rrect(track, 2.0, LINE, 1.0);
        c.rrect((track.0, ay + (ah - thumb_h) * scroll as f32 / max as f32, 4.0, thumb_h), 2.0, MUTED, 1.0);
        let more = if scroll < max { "MORE BELOW" } else { "END" };
        c.text(F::Bold, more, 12.0, x + w - 26.0, y + 30.0, FAINT, Align::Right);
    }
}

/// The bottom bar: "‹ Back" (touch), then the actions the knob moves along.
fn dg_action_bar(c: &mut Canvas, d: &digin::Detail) {
    let back = dg_back();
    c.rrect(back, 12.0, 0x1C1C21, 1.0);
    c.text(F::Medium, "‹  Back", 17.0, back.0 + back.2 / 2.0, back.1 + back.3 / 2.0 + 6.0, TEXT_2, Align::Center);
    let actions = d.body.actions();
    let current = d.body.current();
    for (i, (a, r)) in actions.iter().zip(dg_bar(actions.len())).enumerate() {
        let focused = d.focus == Some(i);
        let armed = d.armed.is_some_and(|(k, _)| k == i);
        let busy = focused && d.busy.is_some();
        let mid = r.1 + r.3 / 2.0;
        let plain = if focused { (0x2C2C34, TEXT) } else { (0x1C1C21, TEXT_2) };
        match a {
            digin::Action::Status(s) => {
                let (sbg, sfg) = status_style(&s.category);
                let is_current = current == Some(i);
                let (bg, fg) = if armed {
                    (AMBER, 0x1C1500)
                } else if is_current {
                    (sbg, sfg)
                } else {
                    plain
                };
                c.rrect(r, 12.0, bg, 1.0);
                let label = if busy {
                    "Saving…".to_string()
                } else if armed {
                    format!("Again: {}", s.name)
                } else {
                    s.name.clone()
                };
                let dot = if armed { 0x1C1500 } else { sfg };
                let tw = c.measure(F::Bold, &label, 17.0).min(r.2 - 52.0);
                let x0 = r.0 + (r.2 - tw - 18.0) / 2.0;
                c.circle(x0 + 4.0, mid, if is_current { 5.5 } else { 4.0 }, if s.category == "todo" && !is_current && !armed { MUTED } else { dot }, 1.0);
                c.fit(F::Bold, &label, 17.0, r.2 - 52.0, x0 + 18.0, mid + 6.0, fg, Align::Left);
            }
            digin::Action::MarkRead => {
                c.rrect(r, 12.0, plain.0, 1.0);
                let label = if busy { "Marking read…" } else { "Mark read" };
                let tw = c.measure(F::Bold, label, 17.0);
                let x0 = r.0 + (r.2 - tw - 30.0) / 2.0;
                c.symbol("checkmark.circle", 18.0, x0 + 9.0, mid, DG_GREEN);
                c.text(F::Bold, label, 17.0, x0 + 30.0, mid + 6.0, plain.1, Align::Left);
            }
            digin::Action::Open => {
                c.rrect(r, 12.0, plain.0, 1.0);
                let label = "Open in Digin";
                let tw = c.measure(F::Bold, label, 17.0);
                let x0 = r.0 + (r.2 - tw - 30.0) / 2.0;
                c.symbol("arrow.up.right.square", 18.0, x0 + 9.0, mid, plain.1);
                c.text(F::Bold, label, 17.0, x0 + 30.0, mid + 6.0, plain.1, Align::Left);
            }
        }
        if focused {
            c.outline(r, 12.0, TEXT, 2.5);
        }
    }
}

// ---------------------------------------------------------------- Claude's questions

fn ask_overlay(c: &mut Canvas, v: &View, a: &AskView) {
    // Who's asking, and how long you have.
    c.symbol("sparkles", 22.0, M + 16.0, 30.0, ORANGE);
    let mut x = M + 36.0;
    x += c.text(F::Bold, "Claude asks", 20.0, x, 37.0, AMBER, Align::Left) + 16.0;
    x += c.fit(F::Bold, a.session, 20.0, 420.0, x, 37.0, TEXT, Align::Left) + 14.0;
    if !a.machine.is_empty() {
        c.symbol("laptopcomputer", 16.0, x + 9.0, 31.0, 0x8AB4F8);
        c.fit(F::Medium, a.machine, 17.0, 260.0, x + 24.0, 37.0, 0x8AB4F8, Align::Left);
    }
    let mut rx = W - M;
    rx -= c.text(F::Mono, &mmss(a.secs_left as f64), 20.0, rx, 37.0, if a.secs_left <= 15 { RED } else { MUTED }, Align::Right) + 18.0;
    if a.count > 1 {
        rx -= c.text(F::Medium, &format!("question {} of {}", a.index + 1, a.count), 17.0, rx, 37.0, TEXT_2, Align::Right) + 18.0;
    }
    if a.queued > 0 {
        c.text(F::Medium, &format!("+{} waiting", a.queued), 17.0, rx, 37.0, AMBER, Align::Right);
    }
    // The question.
    let mut qy = 92.0;
    if !a.header.is_empty() {
        let pw = c.pill(M, 60.0, &a.header.to_uppercase(), 13.0, 0x3A2C08, AMBER);
        if let Some((_, _, why)) = a.detail.filter(|d| !d.2.is_empty()) {
            c.fit(F::Medium, why, 17.0, W - 2.0 * M - pw - 20.0, M + pw + 14.0, 77.0, MUTED, Align::Left);
        }
        qy = 110.0;
    }
    if let Some((tag, what, _)) = a.detail {
        // A permission prompt: the exact command or path, in a code box.
        c.fit(F::Bold, a.question, 26.0, W - 2.0 * M, M, 104.0, TEXT, Align::Left);
        let bx = (M, 116.0, W - 2.0 * M, 62.0);
        c.rrect(bx, 12.0, 0x0E0E11, 1.0);
        c.outline(bx, 12.0, LINE, 1.0);
        let tw = c.text(F::Mono, tag, 18.0, M + 18.0, 154.0, 0xE6B673, Align::Left);
        c.paragraph(F::Mono, what, 18.0, M + 34.0 + tw, 141.0, W - 2.0 * M - 52.0 - tw, 2, 24.0, TEXT);
    } else {
        c.paragraph(F::Bold, a.question, 32.0, M, qy, W - 2.0 * M, 2, 38.0, TEXT);
    }
    if a.multi {
        c.text(F::Medium, "pick any, then Done", 15.0, W - M, 76.0, MUTED, Align::Right);
    }
    // The cards.
    let n = ask_cards(a.options.len(), a.multi);
    let done = if a.multi { Some(a.options.len()) } else { None };
    for i in 0..n {
        let r = ask_card_rect(i, n);
        let (x, y, w, _) = r;
        let sel = i == a.sel;
        // Allow / Deny read as green / red on a permission prompt.
        let tone = match a.options.get(i).map(|o| o.0.as_str()) {
            Some("Allow" | "Always allow") if a.detail.is_some() => GREEN,
            Some("Deny") if a.detail.is_some() => RED,
            _ => AMBER,
        };
        c.rrect(r, 16.0, if sel { 0x2B2412 } else { PANEL }, 1.0);
        if sel {
            c.rrect(r, 16.0, tone, 0.10);
            c.outline(r, 16.0, tone, 4.0);
        }
        let ix = x + 22.0;
        let iw = w - 44.0;
        if i < a.options.len() {
            let (label, desc) = &a.options[i];
            c.circle(ix + 14.0, y + 32.0, 14.0, if sel { AMBER } else { 0x2A2A31 }, 1.0);
            c.text(F::Bold, &(i + 1).to_string(), 15.0, ix + 14.0, y + 38.0, if sel { 0x1C1500 } else { MUTED }, Align::Center);
            if a.multi {
                let on = a.picked.get(i).copied().unwrap_or(false);
                let bx = (x + w - 50.0, y + 16.0, 30.0, 30.0);
                if on {
                    c.rrect(bx, 8.0, GREEN, 1.0);
                    c.symbol("checkmark", 16.0, bx.0 + 15.0, bx.1 + 15.0, 0xFFFFFF);
                } else {
                    c.outline(bx, 8.0, FAINT, 2.0);
                }
            }
            let ly = y + 98.0;
            let used = c.paragraph(F::Bold, label, 34.0, ix, ly, iw, 2, 40.0, if sel { TEXT } else { TEXT_2 });
            c.paragraph(F::Medium, desc, 18.0, ix, ly + used + 10.0, iw, 4, 24.0, MUTED);
        } else if Some(i) == done {
            let picked = a.picked.iter().filter(|p| **p).count();
            c.symbol("checkmark.circle.fill", 40.0, x + w / 2.0, y + 80.0, if picked > 0 { GREEN } else { FAINT });
            c.text(F::Bold, "Done", 26.0, x + w / 2.0, y + 150.0, if picked > 0 { TEXT } else { MUTED }, Align::Center);
            c.text(F::Medium, &format!("send {picked} picked"), 16.0, x + w / 2.0, y + 176.0, MUTED, Align::Center);
        } else {
            c.symbol("xmark.circle", 36.0, x + w / 2.0, y + 80.0, if sel { TEXT_2 } else { FAINT });
            c.text(F::Bold, "Dismiss", 22.0, x + w / 2.0, y + 150.0, if sel { TEXT } else { MUTED }, Align::Center);
            c.text(F::Medium, "answer it in Claude instead", 15.0, x + w / 2.0, y + 176.0, FAINT, Align::Center);
        }
    }
    let _ = v;
    c.text(F::Medium, "turn  choose     ·     press  answer     ·     double-click  dismiss     ·     or answer on the Mac", 15.0, W / 2.0, H - 14.0, FAINT, Align::Center);
}

// ---------------------------------------------------------------- SETTINGS

fn settings_page(c: &mut Canvas, v: &View) {
    let st = v.settings;
    let focus = v.level != &Level::Top;
    let adjusting = matches!(v.level, Level::Detail(_));
    let idle_i = idle_index(st.idle);
    let idle_label = if st.idle >= 120 { format!("{} min", st.idle / 60) } else { format!("{} s", st.idle) };
    let sliders: [(&str, String, f32, &str); SETTING_SLIDERS] = [
        ("sun.max.fill", format!("{}%", (st.screen as f32 / 255.0 * 100.0).round()), (st.screen.saturating_sub(SCREEN_MIN)) as f32 / (255 - SCREEN_MIN) as f32, "the Quake's screen"),
        ("circle.circle.fill", format!("{}%", (st.ring as f32 / 255.0 * 100.0).round()), st.ring as f32 / 255.0, if st.ring_on { "the knob's light ring" } else { "ring light is off" }),
        ("timer", idle_label, idle_i as f32 / (IDLE_STEPS.len() - 1) as f32, if st.idle_home { "then the dashboard" } else { "then back to the top" }),
    ];
    for (i, (sym, value, frac, sub)) in sliders.iter().enumerate() {
        let r = setting_rect(i);
        let (x, y, w, _) = r;
        let editing = adjusting && v.tile == i;
        c.rrect(r, 16.0, if editing { 0x1E1E24 } else { PANEL }, 1.0);
        c.symbol(sym, 22.0, x + 42.0, y + 40.0, if editing { v.accent } else { MUTED });
        c.text(F::Bold, &setting_name(i).to_uppercase(), 14.0, x + 66.0, y + 46.0, FAINT, Align::Left);
        c.text(F::Medium, sub, 15.0, x + w - 28.0, y + 46.0, FAINT, Align::Right);
        c.text(F::Bold, value, 62.0, x + 28.0, y + 124.0, TEXT, Align::Left);
        if editing {
            c.text(F::Medium, "turn to change  ·  press when done", 16.0, x + w - 28.0, y + 118.0, v.accent, Align::Right);
        }
        let (bx, by, bw, bh) = setting_bar(i);
        c.rrect((bx, by, bw, bh), bh / 2.0, 0x2A2A31, 1.0);
        let fill = (bw * frac.clamp(0.0, 1.0)).max(bh);
        let dim = i == 1 && !st.ring_on;
        c.rrect((bx, by, fill, bh), bh / 2.0, if dim { FAINT } else { v.accent }, 1.0);
        c.circle(bx + fill - bh / 2.0, by + bh / 2.0, 13.0, TEXT, 1.0);
        if i == 2 {
            // tick marks for the fixed steps
            for k in 0..IDLE_STEPS.len() {
                let tx = bx + bh / 2.0 + (bw - bh) * k as f32 / (IDLE_STEPS.len() - 1) as f32;
                c.rect((tx - 1.0, by + bh + 6.0, 2.0, 6.0), FAINT, 1.0);
            }
        }
        if focus && v.tile == i {
            c.outline(r, 16.0, if editing { v.accent } else { TEXT }, 3.0);
        }
    }
    let switches: [(bool, &str, &str); SETTING_COUNT - SETTING_SLIDERS] = [
        (st.idle_home, "house.fill", "back to the dashboard when idle"),
        (st.ring_on, "rays", "page colors on the knob"),
        (st.alert, "bell.badge.fill", "breathe when Claude needs you"),
        (st.blink, "light.beacon.max.fill", "flash the ring when something is new"),
        (st.jump, "arrow.uturn.forward.circle.fill", "go to the page that has it"),
        (st.buzzer, "speaker.wave.2.fill", "chirp on alerts"),
        (st.mic, "mic.fill", "built-in mic and its light"),
        (st.auto_accept, "checkmark.seal.fill", "League ready checks"),
        (st.snap, "bolt.fill", "jump there on queue pop"),
    ];
    for (j, (on, sym, sub)) in switches.iter().enumerate() {
        let i = SETTING_SLIDERS + j;
        let r = setting_rect(i);
        let (x, y, w, h) = r;
        c.rrect(r, 16.0, PANEL, 1.0);
        c.symbol(sym, 28.0, x + 40.0, y + 44.0, if *on { v.accent } else { FAINT });
        c.fit(F::Bold, setting_name(i), 22.0, w - 40.0, x + 20.0, y + 102.0, if *on { TEXT } else { TEXT_2 }, Align::Left);
        c.paragraph(F::Medium, sub, 14.0, x + 20.0, y + 126.0, w - 40.0, 2, 18.0, MUTED);
        // switch
        let (sw, sh) = (60.0, 32.0);
        let (sx, sy) = (x + w - sw - 22.0, y + h - sh - 20.0);
        c.rrect((sx, sy, sw, sh), sh / 2.0, if *on { GREEN } else { 0x3A3A42 }, 1.0);
        let knob_x = if *on { sx + sw - sh / 2.0 } else { sx + sh / 2.0 };
        c.circle(knob_x, sy + sh / 2.0, sh / 2.0 - 4.0, 0xFFFFFF, 1.0);
        c.text(F::Bold, if *on { "ON" } else { "OFF" }, 14.0, x + 20.0, y + h - 30.0, if *on { GREEN } else { FAINT }, Align::Left);
        if focus && v.tile == i {
            c.outline(r, 16.0, TEXT, 3.0);
        }
    }
}

// ---------------------------------------------------------------- CODE

fn short_repo(repo: &str) -> &str {
    repo.rsplit('/').next().unwrap_or(repo)
}

/// "platform #412" for a GitHub PR, "infra !92" for a GitLab merge request.
fn pr_ref(p: &github::Pr) -> String {
    format!("{} {}{}", short_repo(&p.repo), if p.gitlab { '!' } else { '#' }, p.number)
}

const GITLAB_ORANGE: u32 = 0xFC6D26;

fn code_page(c: &mut Canvas, v: &View) {
    let g = v.code;
    let mut pw = c.pill(M, CY + 8.0, "GITHUB", 14.0, v.accent, 0xFFFFFF);
    if !g.gitlab_hosts.is_empty() {
        let (bg, fg) = if g.gitlab_ok { (GITLAB_ORANGE, 0xFFFFFF) } else { (0x3A2A22, 0xB88A70) };
        pw += 8.0 + c.pill(M + pw + 8.0, CY + 8.0, "GITLAB", 14.0, bg, fg);
    }
    let tx = M + pw + 16.0;
    if !g.ok {
        let msg = g.error.as_deref().unwrap_or("Loading pull requests…");
        c.fit(F::Medium, msg, 18.0, W - 2.0 * M - pw - 30.0, tx, CY + 27.0, if g.error.is_some() { AMBER } else { MUTED }, Align::Left);
    } else {
        let failing = g.mine.iter().filter(|p| matches!(p.ci.as_str(), "FAILURE" | "ERROR")).count();
        let approved = g.mine.iter().filter(|p| p.review == "APPROVED").count();
        let conflicts = g.mine.iter().filter(|p| p.mergeable == "CONFLICTING").count();
        let mut x = tx;
        x += c.text(F::Bold, &format!("@{}", g.login), 18.0, x, CY + 27.0, TEXT, Align::Left) + 22.0;
        let parts = [
            (format!("{} open", g.mine.len()), TEXT_2),
            (format!("{failing} failing"), if failing > 0 { RED } else { MUTED }),
            (format!("{approved} approved"), if approved > 0 { GREEN } else { MUTED }),
            (format!("{conflicts} conflicting"), if conflicts > 0 { AMBER } else { MUTED }),
            (format!("{} waiting on you", g.reviews.len()), if g.reviews.is_empty() { MUTED } else { v.accent }),
        ];
        for (i, (t, col)) in parts.iter().enumerate() {
            if i > 0 {
                x += c.text(F::Medium, "·", 18.0, x + 4.0, CY + 27.0, FAINT, Align::Left) + 18.0;
            }
            x += c.text(F::Medium, t, 18.0, x, CY + 27.0, *col, Align::Left) + 10.0;
        }
        let note = match (&g.error, &g.gitlab_error) {
            (Some(e), _) => Some(format!("GitHub stale: {e}")),
            (None, Some(e)) if e.contains("not signed in") => Some("GitLab: sign in with  glab auth login".to_string()),
            (None, Some(e)) => Some(format!("GitLab: {e}")),
            _ => None,
        };
        if let Some(n) = note {
            c.fit(F::Medium, &n, 15.0, (W - M - x - 30.0).max(60.0), W - M, CY + 27.0, AMBER, Align::Right);
        }
    }
    let titles = ["YOUR PULL REQUESTS", "REVIEW REQUESTS"];
    for (k, (panel, first, offset, n)) in pr_lists(g, v.tile).into_iter().enumerate() {
        let prs = if k == 0 { &g.mine } else { &g.reviews };
        let reviews = k == 1;
        let (x, y, w, h) = panel;
        c.rrect(panel, 14.0, PANEL_DEEP, 1.0);
        c.text(F::Bold, titles[k], 12.0, x + 22.0, y + 26.0, FAINT, Align::Left);
        let count = if n > PR_ROWS { format!("{}–{} of {n}", first + 1, (first + PR_ROWS).min(n)) } else { n.to_string() };
        c.text(F::Bold, &count, 12.0, x + w - 22.0, y + 26.0, FAINT, Align::Right);
        if n == 0 {
            let msg = match (g.ok, reviews) {
                (false, _) => "",
                (true, true) => "Nothing waiting on you",
                (true, false) => "No open pull requests",
            };
            c.text(F::Medium, msg, 20.0, x + w / 2.0, y + h / 2.0 + 10.0, FAINT, Align::Center);
            continue;
        }
        for slot in 0..PR_ROWS {
            let i = first + slot;
            let Some(p) = prs.get(i) else { break };
            let r = pr_rect(panel, slot);
            let selected = v.level != &Level::Top && v.tile == offset + i;
            if selected {
                c.rrect(r, 10.0, 0x22222A, 1.0);
                c.outline(r, 10.0, TEXT, 2.0);
            } else if slot > 0 {
                c.rect((r.0 + 14.0, r.1, r.2 - 28.0, 1.0), LINE, 0.7);
            }
            pr_line(c, v, p, r, reviews);
        }
    }
}

fn pr_line(c: &mut Canvas, v: &View, p: &github::Pr, r: R, reviews: bool) {
    let (x, y, w, h) = r;
    let mid = y + h / 2.0;
    let (sym, col) = match p.ci.as_str() {
        "SUCCESS" => ("checkmark.circle.fill", GREEN),
        "FAILURE" | "ERROR" => ("xmark.circle.fill", RED),
        "PENDING" | "EXPECTED" => ("clock.fill", AMBER),
        _ => ("circle.dashed", FAINT),
    };
    c.symbol(sym, 18.0, x + 24.0, mid, col);
    let tag_w = if reviews { 172.0 } else { 214.0 };
    c.clip(F::Mono, &pr_ref(p), 14.0, tag_w - 12.0, x + 46.0, mid + 5.0, if p.gitlab { 0xE8A27C } else { MUTED });
    // Right edge, right to left: age, then author (reviews) or review / merge badges.
    let mut rx = x + w - 14.0;
    rx -= c.text(F::Medium, &age(v.now_ms.saturating_sub(p.updated)), 15.0, rx, mid + 5.0, FAINT, Align::Right) + 14.0;
    let mut badge = |c: &mut Canvas, label: &str, bg: u32, fg: u32| {
        let bw = c.measure(F::Bold, label, 12.0) + 12.0 * 1.4;
        c.pill(rx - bw, mid - 10.8, label, 12.0, bg, fg);
        rx -= bw + 8.0;
    };
    if reviews {
        if p.draft {
            badge(c, "DRAFT", 0x2A2A31, MUTED);
        }
    } else {
        match p.review.as_str() {
            "APPROVED" => badge(c, "APPROVED", GREEN, 0xFFFFFF),
            "CHANGES_REQUESTED" => badge(c, "CHANGES", 0x4A1A1A, 0xFFD0D0),
            _ => {}
        }
        if p.mergeable == "CONFLICTING" {
            badge(c, "CONFLICT", AMBER, 0x1C1500);
        }
        if p.draft {
            badge(c, "DRAFT", 0x2A2A31, MUTED);
        }
    }
    if reviews && !p.author.is_empty() {
        rx -= c.fit(F::Medium, &format!("@{}", p.author), 15.0, 130.0, rx, mid + 5.0, TEXT_2, Align::Right) + 12.0;
    }
    let tx = x + 46.0 + tag_w;
    c.clip(F::Medium, &p.title, 18.0, (rx - tx - 8.0).max(0.0), tx, mid + 6.0, if p.draft { MUTED } else { TEXT });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_times_read_like_the_app() {
        assert_eq!(turn_time(45_000), "45s");
        assert_eq!(turn_time(415_000), "6m 55s");
        assert_eq!(turn_time(3_840_000), "1h 04m");
    }


    #[test]
    fn clip_windows() {
        assert_eq!(clip_window(10, 9), 0);
        assert_eq!(clip_window(20, 0), 0);
        assert_eq!(clip_window(20, 13), 6);
        assert_eq!(clip_window(20, 19), 12); // rows 2-3: clips 12..20
    }

    #[test]
    fn ages() {
        assert_eq!(age(30_000), "now");
        assert_eq!(age(4 * 60_000), "4m");
        assert_eq!(age(75 * 60_000), "1h15");
        assert_eq!(age_short(12_000), "12s");
        assert_eq!(mmss(86.4), "1:26");
    }

    #[test]
    fn list_windows_keep_the_selection_visible() {
        assert_eq!(list_window(5, 4, 7), 0);
        assert_eq!(list_window(20, 0, 7), 0);
        assert_eq!(list_window(20, 9, 7), 3);
        assert_eq!(list_window(20, 19, 7), 13);
        // OPS: tile 0 is WARP, hosts from 1; tiles past the hosts keep the last ones shown
        assert_eq!(host_window(6, 0), (0, 4));
        assert_eq!(host_window(6, 6), (2, 4));
        assert_eq!(host_window(6, 9), (2, 4));
    }

    #[test]
    fn overview_window_follows_selection() {
        assert_eq!(window(2, 0), (0, 3));
        assert_eq!(window(8, 0), (0, 5));
        assert_eq!(window(8, 6), (2, 5));
        assert_eq!(window(8, 7), (3, 5));
    }
}
