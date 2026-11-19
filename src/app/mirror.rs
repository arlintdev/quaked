//! The Quake window: the board's screen in a floating window you can drag anywhere, on
//! either Mac. On the Mac with the Quake it's drawn along with the board. On a paired Mac
//! it shows the board's Mac's screen (`ctl frame-since`, fetched only when it changed);
//! with no Quake anywhere it shows this Mac's own.
//!
//! Clicks are taps, the scroll wheel and arrow keys turn the knob, Return (or a right
//! click) presses it and Escape goes back. Drag it to move it; ⌘W hides it, ⌘+ / ⌘− size it.
//! Whether it's up, where, and how big are this Mac's own (window.json, not synced).
use super::*;
use serde::{Deserialize, Serialize};
use std::sync::mpsc;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Width in points when first shown (Retina: the Quake's own 1920 pixels).
const DEFAULT_W: f64 = 960.0;
const MIN_W: f64 = 480.0;
const MAX_W: f64 = 1920.0;
/// How often a paired Mac asks the board's Mac whether the screen changed.
const FRAME_POLL: Duration = Duration::from_millis(250);
/// How far (points) a press moves before it's a drag instead of a tap.
const DRAG_SLOP: f64 = 4.0;
/// Scroll needed for one knob detent: wheel lines, trackpad points.
const LINES_PER_TURN: f64 = 1.0;
const POINTS_PER_TURN: f64 = 36.0;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    shown: bool,
    /// Top-left corner, in screen pixels.
    x: Option<i32>,
    y: Option<i32>,
    /// Width in points.
    width: Option<f64>,
}

fn saved_path() -> std::path::PathBuf {
    config::dir().join("window.json")
}

/// What the window shows.
#[derive(Debug, Clone, PartialEq)]
enum Source {
    /// This Mac's board (the Quake is here, or nowhere).
    Here,
    /// The paired Mac with the Quake: (name, host, port).
    Peer(String, String, u16),
}

/// From the threads that talk to the board's Mac.
pub enum Update {
    Frame(u64, Pixmap),
    /// Couldn't get a picture: why.
    Note(String),
    /// A turn or tap went through: look again now.
    Poke,
}

pub(super) struct Mirror {
    saved: Saved,
    gfx: Option<Gfx>,
    cursor: (f64, f64),
    /// Where the left button went down (window pixels), and whether it's become a drag.
    press: Option<(f64, f64)>,
    dragging: bool,
    mods: ModifiersState,
    scroll: f64,
    /// Moved or resized: save once it settles.
    moved_at: Option<Instant>,
    /// The board's Mac's screen, when it's another Mac's.
    frame: Option<Pixmap>,
    seq: u64,
    note: Option<String>,
    next_poll: Option<Instant>,
    busy: Arc<AtomicBool>,
    /// Turns, presses and taps for the board's Mac, sent in order.
    input: Option<mpsc::Sender<(String, String, u16, String)>>,
}

impl Mirror {
    pub(super) fn load() -> Mirror {
        let saved = std::fs::read(saved_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Mirror {
            saved,
            gfx: None,
            cursor: (0.0, 0.0),
            press: None,
            dragging: false,
            mods: ModifiersState::empty(),
            scroll: 0.0,
            moved_at: None,
            frame: None,
            seq: 0,
            note: None,
            next_poll: None,
            busy: Arc::new(AtomicBool::new(false)),
            input: None,
        }
    }

    pub(super) fn shown(&self) -> bool {
        self.saved.shown
    }

    pub(super) fn is(&self, id: WindowId) -> bool {
        self.gfx.as_ref().is_some_and(|g| g.window.id() == id)
    }

    fn save(&self) {
        if let Ok(json) = serde_json::to_vec_pretty(&self.saved) {
            let _ = std::fs::write(saved_path(), json);
        }
    }
}

/// Is a window's top-left corner on some screen (it may have been unplugged)?
fn on_a_screen(el: &ActiveEventLoop, x: i32, y: i32) -> bool {
    el.available_monitors().any(|m| {
        let (p, s) = (m.position(), m.size());
        x >= p.x && y >= p.y && x < p.x + s.width as i32 - 40 && y < p.y + s.height as i32 - 40
    })
}

/// Floats it on every Space, keeps it 4:1 as it's resized, and rounds its corners.
fn style(window: &Window) {
    use objc2::runtime::Bool;
    use objc2_foundation::NSSize;
    let Some(win) = ns_window(window) else { return };
    unsafe {
        let _: () = msg_send![win, setContentAspectRatio: NSSize::new(4.0, 1.0)];
        // canJoinAllSpaces | fullScreenAuxiliary
        let _: () = msg_send![win, setCollectionBehavior: (1usize | 1 << 8)];
        let _: () = msg_send![win, setOpaque: Bool::NO];
        let clear: *mut AnyObject = msg_send![objc2::class!(NSColor), clearColor];
        let _: () = msg_send![win, setBackgroundColor: clear];
        let view: *mut AnyObject = msg_send![win, contentView];
        if !view.is_null() {
            let _: () = msg_send![view, setWantsLayer: Bool::YES];
            let layer: *mut AnyObject = msg_send![view, layer];
            if !layer.is_null() {
                let _: () = msg_send![layer, setCornerRadius: 12.0f64];
                let _: () = msg_send![layer, setMasksToBounds: Bool::YES];
            }
        }
        let _: () = msg_send![win, invalidateShadow];
    }
}

/// Copies a picture into a 0RGB buffer of `w` x `h`, scaled to fit.
fn blit(frame: &Pixmap, w: u32, h: u32, out: &mut [u32]) {
    let scaled;
    let src = if frame.width() == w && frame.height() == h {
        frame
    } else {
        let Some(mut p) = Pixmap::new(w, h) else { return };
        let (sx, sy) = (w as f32 / frame.width() as f32, h as f32 / frame.height() as f32);
        let paint = tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bicubic, ..Default::default() };
        p.draw_pixmap(0, 0, frame.as_ref(), &paint, tiny_skia::Transform::from_scale(sx, sy), None);
        scaled = p;
        &scaled
    };
    for (dst, px) in out.iter_mut().zip(src.data().chunks_exact(4)) {
        *dst = (px[0] as u32) << 16 | (px[1] as u32) << 8 | px[2] as u32;
    }
}

impl App {
    /// Menu bar or `ctl window`: show or hide the Quake window (None = toggle).
    pub(super) fn mirror_show(&mut self, show: Option<bool>) {
        let show = show.unwrap_or(!self.mirror.saved.shown);
        if show != self.mirror.saved.shown {
            device::log(format!("window: {}", if show { "shown" } else { "hidden" }));
            self.mirror.saved.shown = show;
            self.mirror.save();
        }
    }

    fn mirror_source(&self) -> Source {
        if self.connected {
            return Source::Here;
        }
        match self.peers.peers.iter().find(|p| p.ok && p.board) {
            Some(p) => Source::Peer(p.name.clone(), p.host.clone(), p.port),
            None => Source::Here,
        }
    }

    /// Opens or closes the window to match the setting (from about_to_wait).
    pub(super) fn mirror_ensure(&mut self, el: &ActiveEventLoop) {
        match (self.mirror.saved.shown, self.mirror.gfx.is_some()) {
            (false, true) => {
                self.mirror.gfx = None;
                self.mirror.frame = None;
            }
            (true, false) => self.mirror_open(el),
            _ => {}
        }
    }

    fn mirror_open(&mut self, el: &ActiveEventLoop) {
        let w = self.mirror.saved.width.unwrap_or(DEFAULT_W).clamp(MIN_W, MAX_W);
        let mut attrs = Window::default_attributes()
            .with_title("Quake")
            .with_decorations(false)
            .with_resizable(true)
            .with_has_shadow(true)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_inner_size(LogicalSize::new(w, w / 4.0))
            .with_min_inner_size(LogicalSize::new(MIN_W, MIN_W / 4.0))
            .with_max_inner_size(LogicalSize::new(MAX_W, MAX_W / 4.0));
        if let (Some(x), Some(y)) = (self.mirror.saved.x, self.mirror.saved.y) {
            if on_a_screen(el, x, y) {
                attrs = attrs.with_position(PhysicalPosition::new(x, y));
            }
        }
        let window = match el.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                device::log(format!("window: {e}"));
                self.mirror.saved.shown = false;
                return;
            }
        };
        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        let surface = softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
        style(&window);
        window.request_redraw();
        self.mirror.gfx = Some(Gfx { window, _context: context, surface });
        self.mirror.seq = 0;
        self.mirror.next_poll = None;
    }

    /// The board changed: redraw the window too, when it's drawn here.
    pub(super) fn mirror_redraw(&self) {
        if let Some(g) = &self.mirror.gfx {
            if self.mirror_source() == Source::Here {
                g.window.request_redraw();
            }
        }
    }

    pub(super) fn mirror_draw(&mut self) {
        let Some(g) = &self.mirror.gfx else { return };
        let size = g.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
        let mut pixels = vec![0u32; (size.width * size.height) as usize];
        match (self.mirror_source(), &self.mirror.frame) {
            (Source::Here, _) => self.render(size.width, size.height).copy_to(&mut pixels),
            (Source::Peer(..), Some(f)) => blit(f, size.width, size.height, &mut pixels),
            (Source::Peer(name, ..), None) => {
                let mut c = Canvas::new(size.width, size.height, &self.fonts, &self.symbols);
                let why = self.mirror.note.clone().unwrap_or_else(|| "Getting the screen…".into());
                render::placeholder(&mut c, &format!("The Quake is on {name}"), &why);
                c.copy_to(&mut pixels);
            }
        }
        let Some(g) = &mut self.mirror.gfx else { return };
        if g.surface.resize(w, h).is_err() {
            return;
        }
        if let Ok(mut buf) = g.surface.buffer_mut() {
            buf.copy_from_slice(&pixels);
            let _ = buf.present();
        }
    }

    /// Asks the board's Mac for its screen when it changed; saves where the window went.
    pub(super) fn mirror_tick(&mut self, now: Instant) -> Option<Instant> {
        if self.mirror.moved_at.is_some_and(|t| t.elapsed() > Duration::from_secs(1)) {
            self.mirror.moved_at = None;
            self.mirror_remember();
        }
        let g = self.mirror.gfx.as_ref()?;
        let Source::Peer(name, host, port) = self.mirror_source() else {
            self.mirror.frame = None;
            return None;
        };
        let next = self.mirror.next_poll.unwrap_or(now);
        if now < next {
            return Some(next);
        }
        let next = now + FRAME_POLL;
        self.mirror.next_poll = Some(next);
        if self.mirror.busy.swap(true, Ordering::SeqCst) {
            return Some(next);
        }
        let size = g.window.inner_size();
        let ask = format!("ctl frame-since {} {} {}", self.mirror.seq, size.width.max(1), size.height.max(1));
        let (key, proxy, busy) = (self.cfg.sync.key.clone(), self.proxy.clone(), self.mirror.busy.clone());
        std::thread::Builder::new()
            .name("window-frame".into())
            .spawn(move || {
                let got = sync::request(&host, port, &key, &ask);
                busy.store(false, Ordering::SeqCst);
                let update = match got {
                    Ok(r) if r.starts_with(b"same") => return,
                    Ok(r) if r.starts_with(b"frame ") => {
                        let nl = r.iter().position(|&b| b == b'\n').unwrap_or(r.len());
                        let seq = String::from_utf8_lossy(&r[6..nl]).trim().parse().unwrap_or(0);
                        match r.get(nl + 1..).and_then(|png| Pixmap::decode_png(png).ok()) {
                            Some(p) => Update::Frame(seq, p),
                            None => Update::Note("its picture didn't come through".into()),
                        }
                    }
                    Ok(r) if r.starts_with(b"error: unknown") => Update::Note(format!("update quaked on {name} to see it here")),
                    Ok(r) => Update::Note(String::from_utf8_lossy(&r).trim().chars().take(80).collect()),
                    Err(e) => Update::Note(e),
                };
                let _ = proxy.send_event(Event::Mirror(update));
            })
            .ok();
        Some(next)
    }

    pub(super) fn mirror_update(&mut self, u: Update) {
        match u {
            Update::Frame(seq, p) => {
                self.mirror.seq = seq;
                self.mirror.frame = Some(p);
                self.mirror.note = None;
            }
            Update::Note(n) => {
                self.mirror.frame = None;
                self.mirror.note = Some(n);
            }
            Update::Poke => {
                self.mirror.next_poll = None;
                return;
            }
        }
        if let Some(g) = &self.mirror.gfx {
            g.window.request_redraw();
        }
    }

    /// Saves where the window is and how big.
    fn mirror_remember(&mut self) {
        let Some(g) = &self.mirror.gfx else { return };
        if let Ok(p) = g.window.outer_position() {
            self.mirror.saved.x = Some(p.x);
            self.mirror.saved.y = Some(p.y);
        }
        let size = g.window.inner_size().to_logical::<f64>(g.window.scale_factor());
        self.mirror.saved.width = Some(size.width.round());
        self.mirror.save();
    }

    /// The knob or a tap, from the window: here, or on the board's Mac.
    fn mirror_input(&mut self, input: Input) {
        match self.mirror_source() {
            Source::Here => self.on_device(input),
            Source::Peer(_, host, port) => {
                let line = match input {
                    Input::KnobTurn(d) => format!("ctl turn {d}"),
                    Input::KnobPress(KNOB_DOUBLE) => "ctl double".into(),
                    Input::KnobPress(_) => "ctl press".into(),
                    Input::Tap { x, y } => format!("ctl tap {x:.0} {y:.0}"),
                    _ => return,
                };
                if self.mirror.input.is_none() {
                    let (tx, rx) = mpsc::channel::<(String, String, u16, String)>();
                    let proxy = self.proxy.clone();
                    std::thread::Builder::new()
                        .name("window-input".into())
                        .spawn(move || {
                            for (key, host, port, line) in rx {
                                if let Err(e) = sync::request(&host, port, &key, &line) {
                                    device::log(format!("window: {line}: {e}"));
                                }
                                let _ = proxy.send_event(Event::Mirror(Update::Poke));
                            }
                        })
                        .ok();
                    self.mirror.input = Some(tx);
                }
                if let Some(tx) = &self.mirror.input {
                    let _ = tx.send((self.cfg.sync.key.clone(), host, port, line));
                }
            }
        }
    }

    /// Makes the window bigger or smaller, keeping 4:1.
    fn mirror_resize(&mut self, by: f64) {
        let Some(g) = &self.mirror.gfx else { return };
        let w = (g.window.inner_size().to_logical::<f64>(g.window.scale_factor()).width * by).clamp(MIN_W, MAX_W);
        let _ = g.window.request_inner_size(LogicalSize::new(w, w / 4.0));
        self.mirror.moved_at = Some(Instant::now());
    }

    pub(super) fn mirror_event(&mut self, event: WindowEvent) {
        let Some(g) = &self.mirror.gfx else { return };
        let window = g.window.clone();
        let scale = window.scale_factor();
        match event {
            WindowEvent::RedrawRequested => self.mirror_draw(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                window.request_redraw();
                self.mirror.moved_at = Some(Instant::now());
            }
            WindowEvent::Moved(_) => self.mirror.moved_at = Some(Instant::now()),
            WindowEvent::CloseRequested => self.mirror_show(Some(false)),
            WindowEvent::ModifiersChanged(m) => self.mirror.mods = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.mirror.cursor = (position.x, position.y);
                let Some((px, py)) = self.mirror.press else { return };
                let (dx, dy) = (position.x - px, position.y - py);
                if !self.mirror.dragging && dx.hypot(dy) > DRAG_SLOP * scale {
                    self.mirror.dragging = true;
                }
                if self.mirror.dragging {
                    // Keep the spot you grabbed under the pointer.
                    if let Ok(at) = window.outer_position() {
                        window.set_outer_position(PhysicalPosition::new(at.x + dx.round() as i32, at.y + dy.round() as i32));
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                self.mirror.press = Some(self.mirror.cursor);
                self.mirror.dragging = false;
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                let press = self.mirror.press.take();
                if std::mem::take(&mut self.mirror.dragging) {
                    self.mirror.moved_at = Some(Instant::now());
                } else if let Some((x, y)) = press {
                    let s = window.inner_size();
                    self.mirror_input(Input::Tap { x: x * render::W as f64 / s.width.max(1) as f64, y: y * render::H as f64 / s.height.max(1) as f64 });
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Right, .. } => self.mirror_input(Input::KnobPress(KNOB_PRESS)),
            WindowEvent::MouseWheel { delta, .. } => {
                self.mirror.scroll += match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64 / LINES_PER_TURN,
                    MouseScrollDelta::PixelDelta(p) => p.y / scale / POINTS_PER_TURN,
                };
                // Scrolling down (content moving up) is turning right.
                while self.mirror.scroll.abs() >= 1.0 {
                    let d: i8 = if self.mirror.scroll < 0.0 { 1 } else { -1 };
                    self.mirror.scroll += d as f64;
                    self.mirror_input(Input::KnobTurn(d));
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let cmd = self.mirror.mods.super_key();
                match event.logical_key.as_ref() {
                    Key::Character("w") if cmd => self.mirror_show(Some(false)),
                    Key::Character("=" | "+") if cmd => self.mirror_resize(1.25),
                    Key::Character("-") if cmd => self.mirror_resize(0.8),
                    _ if cmd => {}
                    Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) => self.mirror_input(Input::KnobTurn(1)),
                    Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) => self.mirror_input(Input::KnobTurn(-1)),
                    Key::Named(NamedKey::Enter | NamedKey::Space) => self.mirror_input(Input::KnobPress(KNOB_PRESS)),
                    Key::Named(NamedKey::Escape | NamedKey::Backspace) => self.mirror_input(Input::KnobPress(KNOB_DOUBLE)),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// `ctl frame-since <seq> <w> <h>` from a paired Mac's Quake window: "same" when the
    /// screen hasn't changed since frame `seq`, else "frame <seq>" and a PNG at that size.
    pub(super) fn frame_since(&self, seq: &str, w: &str, h: &str) -> Vec<u8> {
        let cur = self.frame_seq.get();
        if seq.parse::<u64>().ok() == Some(cur) {
            return b"same\n".to_vec();
        }
        let w = w.parse::<u32>().unwrap_or(render::W as u32).clamp(240, 3840);
        let h = h.parse::<u32>().unwrap_or(w / 4).clamp(60, 960);
        let mut out = format!("frame {cur}\n").into_bytes();
        out.extend(self.render(w, h).pixmap.encode_png().unwrap_or_default());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_scales_to_the_window() {
        let mut big = Pixmap::new(1920, 480).unwrap();
        big.fill(tiny_skia::Color::from_rgba8(0x10, 0x20, 0x30, 0xFF));
        let mut out = vec![0u32; 960 * 240];
        blit(&big, 960, 240, &mut out);
        assert!(out.iter().all(|&p| p == 0x102030));
        // Same size: copied as is.
        let mut same = vec![0u32; 1920 * 480];
        blit(&big, 1920, 480, &mut same);
        assert_eq!(same[0], 0x102030);
    }

    #[test]
    fn an_old_or_empty_window_file_reads() {
        let s: Saved = serde_json::from_str("{}").unwrap();
        assert!(!s.shown && s.x.is_none() && s.width.is_none());
        let s: Saved = serde_json::from_str(r#"{"shown":true,"x":10,"y":20,"width":800}"#).unwrap();
        assert_eq!((s.shown, s.x, s.y, s.width), (true, Some(10), Some(20), Some(800.0)));
    }
}
