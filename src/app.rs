//! The daemon: a borderless window pinned to the "DK-QUAKE" display, fed by the device
//! threads, the session files, and the music poller.
//!
//! Navigation (knob):
//!   Top     turn: switch page (AI / Music)   press: focus the page   hold: dim / undim
//!   Focus   AI: turn picks a session, press opens it
//!           Music: turn = volume, press = play/pause
//!   Detail  turn scrolls the steps, press opens Claude
//!   double-click goes back one level, hold goes back to Top; 45s idle returns to Top.

use crate::device::{self, Command, Input};
use crate::music::{self, MusicCmd, MusicEvent, Track};
use crate::render::{self, Canvas, Fonts, Hit, Level, Page, View};
use crate::status::{self, Session, State};
use objc2::msg_send;
use objc2::runtime::AnyObject;
use objc2_foundation::NSRect;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tiny_skia::Pixmap;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::monitor::MonitorHandle;
use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS, MonitorHandleExtMacOS, WindowAttributesExtMacOS};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowId, WindowLevel};

const DISPLAY_NAME: &str = "DK-QUAKE";
const STATUS_POLL: Duration = Duration::from_secs(1);
const DISPLAY_POLL: Duration = Duration::from_secs(4);
const IDLE_TO_TOP: Duration = Duration::from_secs(45);
const FLASH: Duration = Duration::from_millis(180);
/// Two short chirps when a session starts waiting on you.
const NEEDS_YOU_TONE: &[(u8, u64)] = &[(70, 70), (0, 60), (70, 70)];
const LUMINANCE_DIM: u8 = 24;
const VOLUME_STEP: i32 = 4;

// Knob press codes, as the firmware reports them.
const KNOB_PRESS: u8 = 1;
const KNOB_DOUBLE: u8 = 2;
const KNOB_HOLD: u8 = 5;

#[derive(Debug)]
pub enum Event {
    Device(Input),
    Music(MusicEvent),
}

impl std::fmt::Debug for MusicEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MusicEvent::State { track, volume, .. } => write!(f, "Music({:?}, vol {volume:?})", track.as_ref().map(|t| &t.title)),
            MusicEvent::Artwork { url, image } => write!(f, "Artwork({url}, {})", image.is_some()),
        }
    }
}

pub struct Options {
    pub buzzer: bool,
}

struct Gfx {
    window: Rc<Window>,
    _context: softbuffer::Context<Rc<Window>>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
}

struct App {
    opts: Options,
    gfx: Option<Gfx>,
    fonts: Fonts,
    commands: Sender<Command>,
    music_cmd: Sender<MusicCmd>,
    music_fast: Arc<AtomicBool>,
    // AI state
    sessions: Vec<Session>,
    states: HashMap<String, State>,
    loaded_once: bool,
    // Music state
    track: Option<Track>,
    track_read: Instant,
    volume: Option<u8>,
    music_problem: Option<String>,
    art: Option<(String, Pixmap)>,
    // Navigation
    page: Page,
    level: Level,
    sel: usize,
    scroll: usize,
    last_input: Instant,
    // Device
    connected: bool,
    luminance: u8,
    undim_to: Option<u8>,
    cursor: (f64, f64),
    flash: Option<(Hit, Instant)>,
    next_status: Instant,
    next_display: Instant,
    drawn_second: u64,
}

/// The display's real name (winit only reports "Monitor #<id>" on macOS).
fn screen_name(m: &MonitorHandle) -> Option<String> {
    let screen = m.ns_screen()? as *mut AnyObject;
    unsafe {
        let name: *mut AnyObject = msg_send![screen, localizedName];
        if name.is_null() {
            return None;
        }
        let utf8: *const std::ffi::c_char = msg_send![name, UTF8String];
        (!utf8.is_null()).then(|| std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned())
    }
}

fn find_monitor(el: &ActiveEventLoop) -> Option<MonitorHandle> {
    let found = el
        .available_monitors()
        .find(|m| screen_name(m).is_some_and(|n| n.contains(DISPLAY_NAME)))
        .or_else(|| el.available_monitors().find(|m| m.size().width == 1920 && m.size().height == 480));
    if found.is_none() {
        static LOGGED: std::sync::Once = std::sync::Once::new();
        LOGGED.call_once(|| {
            let names: Vec<String> = el.available_monitors().map(|m| format!("{:?} {:?}", screen_name(&m), m.size())).collect();
            device::log(format!("window: no {DISPLAY_NAME} display yet; monitors: {names:?}"));
        });
    }
    found
}

fn ns_window(window: &Window) -> Option<*mut AnyObject> {
    let RawWindowHandle::AppKit(h) = window.window_handle().ok()?.as_raw() else { return None };
    let view = h.ns_view.as_ptr() as *mut AnyObject;
    let win: *mut AnyObject = unsafe { msg_send![view, window] };
    (!win.is_null()).then_some(win)
}

/// Puts the window exactly over the Quake's screen, above the menu bar, on every Space.
fn pin(window: &Window, monitor: &MonitorHandle) {
    let (Some(win), Some(screen)) = (ns_window(window), monitor.ns_screen()) else { return };
    unsafe {
        let screen = screen as *mut AnyObject;
        let target: NSRect = msg_send![screen, frame];
        let current: NSRect = msg_send![win, frame];
        if current != target {
            let _: () = msg_send![win, setFrame: target, display: true];
        }
        let _: () = msg_send![win, setLevel: 25isize]; // NSStatusWindowLevel
        // canJoinAllSpaces | stationary | ignoresCycle | fullScreenAuxiliary
        let _: () = msg_send![win, setCollectionBehavior: (1usize | 1 << 4 | 1 << 6 | 1 << 8)];
    }
}

fn clock() -> (String, u64) {
    let t = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    let h = match tm.tm_hour % 12 {
        0 => 12,
        h => h,
    };
    (format!("{}:{:02}", h, tm.tm_min), t as u64)
}

fn open_claude() {
    let _ = std::process::Command::new("/usr/bin/open").arg("claude://").spawn();
}

impl App {
    fn ensure_window(&mut self, el: &ActiveEventLoop) {
        let monitor = find_monitor(el);
        match (&self.gfx, monitor) {
            (Some(g), Some(m)) => {
                g.window.set_visible(true);
                pin(&g.window, &m);
            }
            (Some(g), None) => g.window.set_visible(false),
            (None, Some(m)) => {
                let attrs = Window::default_attributes()
                    .with_title("quaked")
                    .with_decorations(false)
                    .with_resizable(false)
                    .with_has_shadow(false)
                    .with_window_level(WindowLevel::AlwaysOnTop)
                    .with_position(m.position())
                    .with_inner_size(m.size());
                let window = match el.create_window(attrs) {
                    Ok(w) => Rc::new(w),
                    Err(e) => return device::log(format!("window: {e}")),
                };
                pin(&window, &m);
                let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
                let surface = softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
                device::log(format!("window: on {DISPLAY_NAME} {:?}", m.size()));
                window.request_redraw();
                self.gfx = Some(Gfx { window, _context: context, surface });
            }
            (None, None) => {}
        }
    }

    fn redraw(&self) {
        if let Some(g) = &self.gfx {
            g.window.request_redraw();
        }
    }

    fn view<'a>(&'a self, clock: &'a str, now_ms: u64, second: u64) -> View<'a> {
        View {
            page: self.page,
            level: &self.level,
            sessions: &self.sessions,
            sel: self.sel,
            scroll: self.scroll,
            track: self.track.as_ref(),
            position: self.track.as_ref().map(|t| t.position_at(self.track_read)).unwrap_or(0.0),
            volume: self.volume,
            problem: self.music_problem.as_deref(),
            art: self.art.as_ref().filter(|(url, _)| self.track.as_ref().is_some_and(|t| &t.art_url == url)).map(|(_, p)| p),
            now_ms,
            clock,
            connected: self.connected,
            flash: self.flash.filter(|(_, at)| at.elapsed() < FLASH).map(|(h, _)| h),
            tick: second % 2 == 0,
        }
    }

    fn draw(&mut self) {
        let (clock, second) = clock();
        self.drawn_second = second;
        let Some(g) = &self.gfx else { return };
        let size = g.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
        let mut canvas = Canvas::new(size.width, size.height, &self.fonts);
        render::draw(&mut canvas, &self.view(&clock, status::now_ms(), second));
        let Some(g) = &mut self.gfx else { return };
        if g.surface.resize(w, h).is_err() {
            return;
        }
        if let Ok(mut buf) = g.surface.buffer_mut() {
            canvas.copy_to(&mut buf);
            let _ = buf.present();
        }
    }

    /// Does the screen change on its own every second right now?
    fn animating(&self) -> bool {
        match (self.page, &self.level) {
            (Page::Music, _) => self.track.as_ref().is_some_and(|t| t.playing),
            (Page::Ai, Level::Detail(_)) => true,
            (Page::Ai, _) => self.sessions.iter().any(|s| s.steps.last().is_some_and(|st| st.status == status::StepStatus::Running)),
        }
    }

    fn poll_status(&mut self) {
        let sessions = status::load(status::now_ms());
        let states: HashMap<String, State> = sessions.iter().map(|s| (s.id.clone(), s.state)).collect();
        let chirp = self.loaded_once
            && states.iter().any(|(id, st)| *st == State::Waiting && self.states.get(id) != Some(&State::Waiting));
        if chirp && self.opts.buzzer {
            let _ = self.commands.send(Command::Tone(NEEDS_YOU_TONE.to_vec()));
        }
        if sessions != self.sessions {
            // Keep the selection on the same session when the list changes.
            let selected = self.sessions.get(self.sel).map(|s| s.id.clone());
            self.sessions = sessions;
            if let Some(id) = selected {
                if let Some(i) = self.sessions.iter().position(|s| s.id == id) {
                    self.sel = i;
                }
            }
            self.sel = self.sel.min(self.sessions.len().saturating_sub(1));
            if let Level::Detail(id) = &self.level {
                if !self.sessions.iter().any(|s| &s.id == id) {
                    self.level = Level::Focus;
                }
            }
            self.redraw();
        }
        self.states = states;
        self.loaded_once = true;
    }

    fn set_page(&mut self, p: Page) {
        self.page = p;
        self.level = Level::Top;
        self.scroll = 0;
        self.music_fast.store(p == Page::Music, Ordering::Relaxed);
    }

    fn set_luminance(&mut self, v: u8) {
        self.luminance = v;
        let _ = self.commands.send(Command::Luminance(v));
    }

    fn back(&mut self) {
        self.level = match self.level {
            Level::Detail(_) => Level::Focus,
            _ => Level::Top,
        };
        self.scroll = 0;
    }

    fn knob_turn(&mut self, d: i8) {
        let d = d as i32;
        match (self.page, &self.level) {
            (_, Level::Top) => self.set_page(if self.page == Page::Ai { Page::Music } else { Page::Ai }),
            (Page::Ai, Level::Focus) => {
                let n = self.sessions.len() as i32;
                if n > 0 {
                    self.sel = (self.sel as i32 + d).clamp(0, n - 1) as usize;
                }
            }
            (Page::Ai, Level::Detail(_)) => {
                self.scroll = (self.scroll as i32 + d).max(0) as usize;
                if let Some(s) = self.detail_session() {
                    self.scroll = self.scroll.min(s.steps.len().saturating_sub(render::DETAIL_ROWS));
                }
            }
            (Page::Music, _) => {
                self.volume = Some((self.volume.unwrap_or(50) as i32 + d * VOLUME_STEP).clamp(0, 100) as u8);
                let _ = self.music_cmd.send(MusicCmd::VolumeBy(d * VOLUME_STEP));
            }
        }
    }

    fn detail_session(&self) -> Option<&Session> {
        let Level::Detail(id) = &self.level else { return None };
        self.sessions.iter().find(|s| &s.id == id)
    }

    fn knob_press(&mut self, code: u8) {
        match code {
            KNOB_PRESS => match (self.page, self.level.clone()) {
                (Page::Ai, Level::Top) => {
                    if !self.sessions.is_empty() {
                        // Start on whatever needs you, if anything does.
                        if let Some(i) = self.sessions.iter().position(|s| s.state == State::Waiting) {
                            self.sel = i;
                        }
                        self.level = Level::Focus;
                    }
                }
                (Page::Ai, Level::Focus) => {
                    if let Some(s) = self.sessions.get(self.sel) {
                        self.level = Level::Detail(s.id.clone());
                        self.scroll = 0;
                    }
                }
                (Page::Ai, Level::Detail(_)) => open_claude(),
                (Page::Music, Level::Top) => self.level = Level::Focus,
                (Page::Music, _) => {
                    self.toggle_play_optimistic();
                    let _ = self.music_cmd.send(MusicCmd::PlayPause);
                }
            },
            KNOB_DOUBLE => self.back(),
            KNOB_HOLD => {
                if self.level == Level::Top {
                    match self.undim_to.take() {
                        Some(prev) => self.set_luminance(prev),
                        None => {
                            self.undim_to = Some(self.luminance);
                            self.set_luminance(LUMINANCE_DIM);
                        }
                    }
                } else {
                    self.level = Level::Top;
                    self.scroll = 0;
                }
            }
            _ => {} // release (255) and anything unknown
        }
    }

    fn toggle_play_optimistic(&mut self) {
        if let Some(t) = &mut self.track {
            t.position = t.position_at(self.track_read);
            t.playing = !t.playing;
            self.track_read = Instant::now();
        }
    }

    fn tap(&mut self, x: f64, y: f64) {
        let (clock, second) = clock();
        let hit = render::hit(&self.view(&clock, status::now_ms(), second), x, y);
        let Some(h) = hit else { return };
        self.flash = Some((h, Instant::now()));
        match h {
            Hit::Tab(p) => self.set_page(p),
            Hit::Session(i) => {
                self.sel = i;
                if let Some(s) = self.sessions.get(i) {
                    self.level = Level::Detail(s.id.clone());
                    self.scroll = 0;
                }
            }
            Hit::Back => {
                self.level = Level::Top;
                self.scroll = 0;
            }
            Hit::OpenClaude => open_claude(),
            Hit::Prev => {
                let _ = self.music_cmd.send(MusicCmd::Previous);
            }
            Hit::PlayPause => {
                self.toggle_play_optimistic();
                let _ = self.music_cmd.send(MusicCmd::PlayPause);
            }
            Hit::Next => {
                let _ = self.music_cmd.send(MusicCmd::Next);
            }
            Hit::Volume(frac) => {
                let v = (frac * 100.0).round() as u8;
                self.volume = Some(v);
                let _ = self.music_cmd.send(MusicCmd::SetVolume(v));
            }
        }
    }

    fn on_device(&mut self, input: Input) {
        let is_user = matches!(input, Input::Tap { .. } | Input::KnobTurn(_) | Input::KnobPress(_));
        if is_user {
            self.last_input = Instant::now();
        }
        match input {
            Input::Connected { luminance, firmware } => {
                device::log(format!("device: Quake firmware {}", firmware.as_deref().unwrap_or("?")));
                self.connected = true;
                if let Some(l) = luminance {
                    self.luminance = l;
                }
            }
            Input::Disconnected => self.connected = false,
            Input::Tap { x, y } => self.tap(x, y),
            Input::KnobTurn(d) => self.knob_turn(d),
            Input::KnobPress(code) => self.knob_press(code),
        }
        self.redraw();
    }

    fn on_music(&mut self, ev: MusicEvent) {
        match ev {
            MusicEvent::State { track, volume, problem } => {
                self.track = track;
                self.track_read = Instant::now();
                self.volume = volume.or(self.volume);
                self.music_problem = problem;
            }
            MusicEvent::Artwork { url, image } => {
                self.art = image.map(|p| (url, p));
            }
        }
        self.redraw();
    }
}

impl ApplicationHandler<Event> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        self.ensure_window(el);
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, event: Event) {
        match event {
            Event::Device(input) => self.on_device(input),
            Event::Music(ev) => self.on_music(ev),
        }
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::CursorMoved { position, .. } => self.cursor = (position.x, position.y),
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                // Mouse clicks on the screen act like taps.
                if let Some(g) = &self.gfx {
                    let s = g.window.inner_size();
                    let (x, y) = (self.cursor.0 * render::W as f64 / s.width as f64, self.cursor.1 * render::H as f64 / s.height as f64);
                    self.on_device(Input::Tap { x, y });
                }
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => self.redraw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_status {
            self.poll_status();
            self.next_status = now + STATUS_POLL;
        }
        if now >= self.next_display {
            self.ensure_window(el);
            self.next_display = now + DISPLAY_POLL;
        }
        if self.level != Level::Top && self.last_input.elapsed() > IDLE_TO_TOP {
            self.level = Level::Top;
            self.scroll = 0;
            self.redraw();
        }
        let (_, second) = clock();
        if second != self.drawn_second && (self.animating() || second % 60 == 0) {
            self.redraw();
        }
        let mut wake = self.next_status.min(self.next_display);
        if let Some((_, at)) = self.flash {
            if at.elapsed() >= FLASH {
                self.flash = None;
                self.redraw();
            } else {
                wake = wake.min(at + FLASH);
            }
        }
        el.set_control_flow(ControlFlow::WaitUntil(wake));
    }
}

pub fn ctl_socket_path() -> std::path::PathBuf {
    let dir = status::dir();
    dir.parent().unwrap_or(&dir).join("ctl.sock")
}

/// Lines on the control socket act like the knob or a tap:
/// "turn 1", "turn -1", "press", "double", "hold", "tap X Y".
fn ctl_thread(emit: impl Fn(Input) + Send + 'static) {
    let path = ctl_socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = match std::os::unix::net::UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => return device::log(format!("ctl: cannot listen on {}: {e}", path.display())),
    };
    for conn in listener.incoming().flatten() {
        for line in BufReader::new(conn).lines().map_while(Result::ok) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let input = match parts.as_slice() {
                ["turn", d] => d.parse::<i8>().ok().map(|d| Input::KnobTurn(d.signum())),
                ["press"] => Some(Input::KnobPress(KNOB_PRESS)),
                ["double"] => Some(Input::KnobPress(KNOB_DOUBLE)),
                ["hold"] => Some(Input::KnobPress(KNOB_HOLD)),
                ["tap", x, y] => match (x.parse(), y.parse()) {
                    (Ok(x), Ok(y)) => Some(Input::Tap { x, y }),
                    _ => None,
                },
                _ => None,
            };
            match input {
                Some(i) => emit(i),
                None => device::log(format!("ctl: ignored {line:?}")),
            }
        }
    }
}

pub fn run(opts: Options) {
    let event_loop = EventLoop::<Event>::with_user_event()
        .with_activation_policy(ActivationPolicy::Accessory)
        .build()
        .expect("event loop");
    let proxy = event_loop.create_proxy();
    let p1 = proxy.clone();
    let commands = device::spawn(move |input| {
        let _ = p1.send_event(Event::Device(input));
    });
    let music_fast = Arc::new(AtomicBool::new(false));
    let p2 = proxy.clone();
    let music_cmd = music::spawn(music_fast.clone(), move |ev| {
        let _ = p2.send_event(Event::Music(ev));
    });
    let p3 = proxy.clone();
    std::thread::Builder::new()
        .name("ctl".into())
        .spawn(move || {
            ctl_thread(move |input| {
                let _ = p3.send_event(Event::Device(input));
            })
        })
        .unwrap();
    let mut app = App {
        opts,
        gfx: None,
        fonts: Fonts::load(),
        commands,
        music_cmd,
        music_fast,
        sessions: Vec::new(),
        states: HashMap::new(),
        loaded_once: false,
        track: None,
        track_read: Instant::now(),
        volume: None,
        music_problem: None,
        art: None,
        page: Page::Ai,
        level: Level::Top,
        sel: 0,
        scroll: 0,
        last_input: Instant::now(),
        connected: false,
        luminance: 255,
        undim_to: None,
        cursor: (0.0, 0.0),
        flash: None,
        next_status: Instant::now(),
        next_display: Instant::now() + DISPLAY_POLL,
        drawn_second: 0,
    };
    event_loop.run_app(&mut app).expect("event loop run");
}
