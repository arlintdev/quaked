//! The daemon: a borderless window pinned to the "DK-QUAKE" display, fed by the device
//! threads and the session files.

use crate::device::{self, Command, Input};
use crate::render::{self, Board, Canvas, Fonts, Target};
use crate::status::{self, Session, State};
use objc2::msg_send;
use objc2::runtime::AnyObject;
use objc2_foundation::NSRect;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};
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
const FLASH: Duration = Duration::from_millis(180);
/// Two short chirps when a session starts waiting on you.
const NEEDS_YOU_TONE: &[(u8, u64)] = &[(70, 70), (0, 60), (70, 70)];
const LUMINANCE_STEP: i16 = 16;
const LUMINANCE_DIM: u8 = 24;

#[derive(Debug)]
pub enum Event {
    Device(Input),
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
    sessions: Vec<Session>,
    states: HashMap<String, State>,
    loaded_once: bool,
    connected: bool,
    luminance: u8,
    undim_to: Option<u8>,
    cursor: (f64, f64),
    flash: Option<(Target, Instant)>,
    next_status: Instant,
    next_display: Instant,
    drawn_minute: u64,
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
    (format!("{}:{:02}", h, tm.tm_min), t as u64 / 60)
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

    fn draw(&mut self) {
        let Some(g) = &mut self.gfx else { return };
        let size = g.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
        if g.surface.resize(w, h).is_err() {
            return;
        }
        let (clock, minute) = clock();
        self.drawn_minute = minute;
        let mut canvas = Canvas::new(size.width, size.height, &self.fonts);
        render::draw(
            &mut canvas,
            &Board { sessions: &self.sessions, now_ms: status::now_ms(), clock, connected: self.connected },
        );
        if let Some((t, at)) = self.flash {
            if at.elapsed() < FLASH {
                canvas.flash(t);
            }
        }
        if let Ok(mut buf) = g.surface.buffer_mut() {
            canvas.copy_to(&mut buf);
            let _ = buf.present();
        }
    }

    fn poll_status(&mut self) {
        let sessions = status::load(status::now_ms());
        let mut chirp = false;
        let states: HashMap<String, State> = sessions.iter().map(|s| (s.id.clone(), s.state)).collect();
        if self.loaded_once {
            for (id, st) in &states {
                if *st == State::Waiting && self.states.get(id) != Some(&State::Waiting) {
                    chirp = true;
                }
            }
        }
        if chirp && self.opts.buzzer {
            let _ = self.commands.send(Command::Tone(NEEDS_YOU_TONE.to_vec()));
        }
        if sessions != self.sessions {
            self.sessions = sessions;
            self.redraw();
        }
        self.states = states;
        self.loaded_once = true;
    }

    fn tap(&mut self, x: f64, y: f64) {
        let Some(t) = render::hit(x, y) else { return };
        device::log(format!("tap {t:?} at {x:.0},{y:.0}"));
        self.flash = Some((t, Instant::now()));
        self.redraw();
        let _ = std::process::Command::new("/usr/bin/open").arg("claude://").spawn();
    }

    fn set_luminance(&mut self, v: u8) {
        self.luminance = v;
        let _ = self.commands.send(Command::Luminance(v));
    }

    fn on_device(&mut self, input: Input) {
        match input {
            Input::Connected { luminance, firmware } => {
                device::log(format!("device: Quake firmware {}", firmware.as_deref().unwrap_or("?")));
                self.connected = true;
                if let Some(l) = luminance {
                    self.luminance = l;
                }
                self.redraw();
            }
            Input::Disconnected => {
                self.connected = false;
                self.redraw();
            }
            Input::Tap { x, y } => self.tap(x, y),
            Input::KnobTurn(dir) => {
                self.undim_to = None;
                let v = (self.luminance as i16 + dir as i16 * LUMINANCE_STEP).clamp(8, 255) as u8;
                self.set_luminance(v);
            }
            Input::KnobPress(state) => {
                if state != 1 {
                    return; // act on press, not release
                }
                match self.undim_to.take() {
                    Some(prev) => self.set_luminance(prev),
                    None => {
                        self.undim_to = Some(self.luminance);
                        self.set_luminance(LUMINANCE_DIM);
                    }
                }
            }
        }
    }
}

impl ApplicationHandler<Event> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        self.ensure_window(el);
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, event: Event) {
        let Event::Device(input) = event;
        self.on_device(input);
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::CursorMoved { position, .. } => self.cursor = (position.x, position.y),
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                // Mouse clicks on the screen act like taps (handy without touching the device).
                if let Some(g) = &self.gfx {
                    let s = g.window.inner_size();
                    let (x, y) = (self.cursor.0 * render::W as f64 / s.width as f64, self.cursor.1 * render::H as f64 / s.height as f64);
                    self.tap(x, y);
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
        if clock().1 != self.drawn_minute {
            self.redraw(); // clock and ages
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

pub fn run(opts: Options) {
    let event_loop = EventLoop::<Event>::with_user_event()
        .with_activation_policy(ActivationPolicy::Accessory)
        .build()
        .expect("event loop");
    let proxy = event_loop.create_proxy();
    let commands = device::spawn(move |input| {
        let _ = proxy.send_event(Event::Device(input));
    });
    let mut app = App {
        opts,
        gfx: None,
        fonts: Fonts::load(),
        commands,
        sessions: Vec::new(),
        states: HashMap::new(),
        loaded_once: false,
        connected: false,
        luminance: 255,
        undim_to: None,
        cursor: (0.0, 0.0),
        flash: None,
        next_status: Instant::now(),
        next_display: Instant::now() + DISPLAY_POLL,
        drawn_minute: 0,
    };
    event_loop.run_app(&mut app).expect("event loop run");
}
