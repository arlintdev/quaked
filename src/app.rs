//! The daemon: a borderless window pinned to the "DK-QUAKE" display, fed by the device
//! threads, the session files, the music poller and the soundboard engine.
//!
//! Navigation (knob), the same on every page:
//!   Top     turn: switch page   press: focus the page   hold: dim / undim the screen
//!   Focus   turn moves the selection (session, shortcut, clip) or sets volume (music);
//!           press opens / runs / plays
//!   Detail  (AI) turn scrolls the steps, press opens Claude
//!   double-click goes back one level, hold goes back to Top; idle returns to Top.

use crate::audio::{self, Audio};
use crate::config::{self, Action, Config, PageId};
use crate::device::{self, Command, Input};
use crate::{docker, league};
use crate::keys;
use crate::music::{self, MusicCmd, MusicEvent, Track};
use crate::proto;
use crate::render::{self, Canvas, Fonts, Hit, Level, View};
use crate::status::{self, Session, State};
use crate::symbols::Symbols;
use objc2::msg_send;
use objc2::runtime::AnyObject;
use objc2_foundation::NSRect;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tiny_skia::Pixmap;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::monitor::MonitorHandle;
use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS, MonitorHandleExtMacOS, WindowAttributesExtMacOS};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowId, WindowLevel};

const DISPLAY_NAME: &str = "DK-QUAKE";
const POLL: Duration = Duration::from_secs(1);
const DISPLAY_POLL: Duration = Duration::from_secs(4);
const FLASH: Duration = Duration::from_millis(180);
/// Two short chirps when a session starts waiting on you.
const NEEDS_YOU_TONE: &[(u8, u64)] = &[(70, 70), (0, 60), (70, 70)];
/// Rising triple chirp when a League queue pops.
const MATCH_TONE: &[(u8, u64)] = &[(60, 90), (0, 50), (75, 90), (0, 50), (90, 220)];
const LOG_REFRESH: Duration = Duration::from_secs(3);
const LUMINANCE_DIM: u8 = 24;
const VOLUME_STEP: i32 = 4;
/// What the ring showed before quaked took over (read from the device), restored when
/// ring control is switched off.
const RING_FACTORY: Command = Command::Ring { effect: 13, hue: 0, sat: 255, brightness: 247, speed: 127 };

// Knob press codes, as the firmware reports them.
const KNOB_PRESS: u8 = 1;
const KNOB_DOUBLE: u8 = 2;
const KNOB_HOLD: u8 = 5;

pub enum Event {
    Device(Input),
    Music(MusicEvent),
    Sounds,
    League(league::Event),
    Docker(docker::Status),
    /// A control-socket request and where to send the reply.
    Ctl(String, Sender<Vec<u8>>),
}

struct Gfx {
    window: Rc<Window>,
    _context: softbuffer::Context<Rc<Window>>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
}

struct App {
    cfg: Config,
    cfg_mtime: Option<SystemTime>,
    gfx: Option<Gfx>,
    fonts: Fonts,
    symbols: Symbols,
    commands: Sender<Command>,
    music_cmd: Sender<MusicCmd>,
    music_fast: Arc<AtomicBool>,
    audio: Audio,
    sounds: audio::Status,
    league_cmd: Sender<league::Cmd>,
    league: league::Status,
    docker_cmd: Sender<docker::Cmd>,
    docker: docker::Status,
    docker_fast: Arc<AtomicBool>,
    next_logs: Instant,
    // AI
    sessions: Vec<Session>,
    states: HashMap<String, State>,
    loaded_once: bool,
    // Music
    track: Option<Track>,
    track_read: Instant,
    volume: Option<u8>,
    music_problem: Option<String>,
    art: Option<(String, Pixmap)>,
    // Navigation
    page: PageId,
    level: Level,
    sel: usize,
    scroll: usize,
    tile: usize,
    last_input: Instant,
    // Device
    connected: bool,
    ring_sent: Option<Command>,
    luminance: u8,
    undim_to: Option<u8>,
    cursor: (f64, f64),
    flash: Option<(Hit, Instant)>,
    next_poll: Instant,
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

/// Runs a program without waiting for it (and reaps it when it exits).
fn spawn_detached(mut cmd: std::process::Command) {
    match cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => device::log(format!("action: could not start {cmd:?}: {e}")),
    }
}

fn open_target(target: &str) {
    let mut c = std::process::Command::new("/usr/bin/open");
    c.arg(target);
    spawn_detached(c);
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

    fn accent(&self) -> u32 {
        config::hex(self.cfg.ring.color_for(self.page), 0xD97757)
    }

    fn view<'a>(&'a self, clock: &'a str, now_ms: u64, second: u64) -> View<'a> {
        View {
            pages: &self.cfg.pages,
            page: self.page,
            accent: self.accent(),
            shortcuts: &self.cfg.shortcuts,
            sounds: &self.sounds,
            tile: self.tile,
            league: &self.league,
            league_cfg: &self.cfg.league,
            docker: &self.docker,
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

    /// Renders the current screen into a canvas of the given size.
    fn render(&self, w: u32, h: u32) -> Canvas<'_> {
        let (clock, second) = clock();
        let mut canvas = Canvas::new(w, h, &self.fonts, &self.symbols);
        render::draw(&mut canvas, &self.view(&clock, status::now_ms(), second));
        canvas
    }

    fn draw(&mut self) {
        self.drawn_second = clock().1;
        let Some(g) = &self.gfx else { return };
        let size = g.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
        let mut pixels = vec![0u32; (size.width * size.height) as usize];
        self.render(size.width, size.height).copy_to(&mut pixels);
        let Some(g) = &mut self.gfx else { return };
        if g.surface.resize(w, h).is_err() {
            return;
        }
        if let Ok(mut buf) = g.surface.buffer_mut() {
            buf.copy_from_slice(&pixels);
            let _ = buf.present();
        }
    }

    /// Does the screen change on its own every second right now?
    fn animating(&self) -> bool {
        match (self.page, &self.level) {
            (PageId::Music, _) => self.track.as_ref().is_some_and(|t| t.playing),
            (PageId::Ai, Level::Detail(_)) => true,
            (PageId::Ai, _) => self.sessions.iter().any(|s| s.steps.last().is_some_and(|st| st.status == status::StepStatus::Running)),
            _ => false,
        }
    }

    // ------------------------------------------------------------ config & ring

    fn reload_config(&mut self, force: bool) {
        let mtime = config::modified();
        if !force && mtime == self.cfg_mtime {
            return;
        }
        self.cfg_mtime = mtime;
        let cfg = config::load();
        if cfg != self.cfg || force {
            if cfg.soundboard != self.cfg.soundboard || force {
                let _ = self.audio.cmd.send(audio::Cmd::Configure(cfg.soundboard.clone()));
            }
            if cfg.league != self.cfg.league || force {
                let _ = self.league_cmd.send(league::Cmd::Configure(cfg.league.clone()));
            }
            if cfg.device != self.cfg.device && self.connected {
                let _ = self.commands.send(Command::Mic(cfg.device.mic));
            }
            if cfg.docker != self.cfg.docker || force {
                let _ = self.docker_cmd.send(docker::Cmd::Configure(cfg.docker.clone()));
            }
            self.cfg = cfg;
            device::set_touch_flip(self.cfg.touch.flip_x, self.cfg.touch.flip_y);
            if self.cfg.pages.is_empty() {
                self.cfg.pages = vec![PageId::Ai];
            }
            if !self.cfg.pages.contains(&self.page) {
                self.set_page(self.cfg.pages[0]);
            }
            device::log("config: loaded");
            self.update_ring();
            self.redraw();
        }
    }

    fn update_ring(&mut self) {
        if !self.connected {
            return;
        }
        let r = &self.cfg.ring;
        let want = if !r.enabled {
            // Hand the ring back once, then leave it alone.
            if self.ring_sent.is_none() || self.ring_sent == Some(RING_FACTORY) {
                return;
            }
            RING_FACTORY
        } else if self.league.phase == "ReadyCheck" {
            let (hue, sat) = config::hue_sat(config::hex(&r.league, 0xC89B3C));
            Command::Ring { effect: proto::EFFECT_BREATHING, hue, sat, brightness: r.brightness.max(180), speed: 220 }
        } else if r.alert && self.sessions.iter().any(|s| s.state == State::Waiting) {
            let (hue, sat) = config::hue_sat(config::hex(&r.alert_color, 0xF2B705));
            Command::Ring { effect: proto::EFFECT_BREATHING, hue, sat, brightness: r.brightness, speed: 170 }
        } else {
            let (hue, sat) = config::hue_sat(self.accent());
            Command::Ring { effect: proto::EFFECT_SOLID, hue, sat, brightness: r.brightness, speed: 128 }
        };
        if self.ring_sent.as_ref() != Some(&want) {
            let _ = self.commands.send(want.clone());
            self.ring_sent = Some(want);
        }
    }

    // ------------------------------------------------------------ data

    fn poll_status(&mut self) {
        let sessions = status::load(status::now_ms());
        let states: HashMap<String, State> = sessions.iter().map(|s| (s.id.clone(), s.state)).collect();
        let chirp = self.loaded_once
            && states.iter().any(|(id, st)| *st == State::Waiting && self.states.get(id) != Some(&State::Waiting));
        if chirp && self.cfg.buzzer {
            let _ = self.commands.send(Command::Tone(NEEDS_YOU_TONE.to_vec()));
        }
        if sessions != self.sessions {
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
        self.update_ring();
    }

    // ------------------------------------------------------------ navigation

    fn set_page(&mut self, p: PageId) {
        self.page = p;
        self.level = Level::Top;
        self.scroll = 0;
        self.tile = 0;
        self.music_fast.store(p == PageId::Music, Ordering::Relaxed);
        self.docker_fast.store(p == PageId::Docker, Ordering::Relaxed);
        self.update_ring();
    }

    /// Writes the config (after a change made on the board, like the auto-accept toggle).
    fn save_config(&mut self) {
        let path = config::path();
        let tmp = path.with_extension("json.tmp");
        if let Ok(text) = serde_json::to_string_pretty(&self.cfg) {
            if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
                self.cfg_mtime = config::modified();
            }
        }
    }

    fn toggle_auto_accept(&mut self) {
        self.cfg.league.auto_accept = !self.cfg.league.auto_accept;
        let _ = self.league_cmd.send(league::Cmd::Configure(self.cfg.league.clone()));
        self.save_config();
        device::log(format!("league: auto-accept {}", if self.cfg.league.auto_accept { "on" } else { "off" }));
    }

    fn league_primary(&mut self) {
        let cmd = match self.league.phase.as_str() {
            "ReadyCheck" => Some(league::Cmd::Accept),
            "Lobby" => Some(league::Cmd::FindMatch),
            "Matchmaking" => Some(league::Cmd::CancelSearch),
            _ => None,
        };
        // Nothing to do in other phases; auto-accept is only changed by its own toggle.
        if let Some(c) = cmd {
            let _ = self.league_cmd.send(c);
        }
    }

    fn open_container(&mut self, i: usize) {
        if let Some(ct) = self.docker.containers.get(i) {
            self.tile = i;
            self.level = Level::Detail(ct.id.clone());
            self.scroll = 0;
            let _ = self.docker_cmd.send(docker::Cmd::Logs(ct.id.clone()));
            self.next_logs = Instant::now() + LOG_REFRESH;
        }
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

    fn tile_count(&self) -> usize {
        match self.page {
            PageId::Shortcuts => self.cfg.shortcuts.len().min(render::SC_COLS * render::SC_ROWS),
            PageId::Soundboard => self.sounds.clips.len(),
            PageId::Docker => self.docker.containers.len(),
            _ => 0,
        }
    }

    fn knob_turn(&mut self, d: i8) {
        let d = d as i32;
        match (self.page, &self.level) {
            (_, Level::Top) => {
                let pages = &self.cfg.pages;
                let i = pages.iter().position(|p| *p == self.page).unwrap_or(0) as i32;
                let next = (i + d).rem_euclid(pages.len() as i32) as usize;
                self.set_page(pages[next]);
            }
            (PageId::Ai, Level::Focus) => {
                let n = self.sessions.len() as i32;
                if n > 0 {
                    self.sel = (self.sel as i32 + d).clamp(0, n - 1) as usize;
                }
            }
            (PageId::Ai, Level::Detail(_)) => {
                self.scroll = (self.scroll as i32 + d).max(0) as usize;
                if let Some(s) = self.detail_session() {
                    self.scroll = self.scroll.min(s.steps.len().saturating_sub(render::DETAIL_ROWS));
                }
            }
            (PageId::Music, _) => {
                self.volume = Some((self.volume.unwrap_or(50) as i32 + d * VOLUME_STEP).clamp(0, 100) as u8);
                let _ = self.music_cmd.send(MusicCmd::VolumeBy(d * VOLUME_STEP));
            }
            (PageId::Docker, Level::Detail(_)) => {
                // Logs: newest at the bottom; turning left goes back in time.
                self.scroll = (self.scroll as i32 - d).max(0) as usize;
            }
            (PageId::Shortcuts | PageId::Soundboard | PageId::Docker, _) => {
                let n = self.tile_count() as i32;
                if n > 0 {
                    self.tile = (self.tile as i32 + d).clamp(0, n - 1) as usize;
                }
            }
            (PageId::League, _) => {}
        }
    }

    fn detail_session(&self) -> Option<&Session> {
        let Level::Detail(id) = &self.level else { return None };
        self.sessions.iter().find(|s| &s.id == id)
    }

    fn knob_press(&mut self, code: u8) {
        match code {
            KNOB_PRESS => match (self.page, self.level.clone()) {
                (PageId::Ai, Level::Top) => {
                    if !self.sessions.is_empty() {
                        if let Some(i) = self.sessions.iter().position(|s| s.state == State::Waiting) {
                            self.sel = i;
                        }
                        self.level = Level::Focus;
                    }
                }
                (PageId::Ai, Level::Focus) => {
                    if let Some(s) = self.sessions.get(self.sel) {
                        self.level = Level::Detail(s.id.clone());
                        self.scroll = 0;
                    }
                }
                (PageId::Ai, Level::Detail(_)) => open_target("claude://"),
                (PageId::League, Level::Top) => self.level = Level::Focus,
                (PageId::League, _) => self.league_primary(),
                (PageId::Docker, Level::Focus) => self.open_container(self.tile),
                (PageId::Docker, Level::Detail(id)) => {
                    let _ = self.docker_cmd.send(docker::Cmd::Logs(id));
                }
                (_, Level::Top) => {
                    self.level = Level::Focus;
                    self.tile = self.tile.min(self.tile_count().saturating_sub(1));
                }
                (PageId::Music, _) => self.play_pause(),
                (PageId::Shortcuts, _) => self.run_shortcut(self.tile),
                (PageId::Soundboard, _) => self.toggle_clip(self.tile),
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

    fn play_pause(&mut self) {
        if let Some(t) = &mut self.track {
            t.position = t.position_at(self.track_read);
            t.playing = !t.playing;
            self.track_read = Instant::now();
        }
        let _ = self.music_cmd.send(MusicCmd::PlayPause);
    }

    fn toggle_clip(&mut self, i: usize) {
        if i >= self.sounds.clips.len() {
            return;
        }
        let cmd = if self.sounds.playing.contains(&i) { audio::Cmd::Stop(i) } else { audio::Cmd::Play(i) };
        let _ = self.audio.cmd.send(cmd);
    }

    fn run_shortcut(&mut self, i: usize) {
        let Some(sc) = self.cfg.shortcuts.get(i).cloned() else { return };
        device::log(format!("shortcut: {}", sc.title));
        self.run_action(&sc.action);
    }

    fn run_action(&mut self, a: &Action) {
        match a {
            Action::Keys { keys } => match keys::parse(keys) {
                Some((modifiers, key)) => {
                    let _ = self.commands.send(Command::Keys { modifiers, key });
                }
                None => device::log(format!("shortcut: can't type \"{keys}\"")),
            },
            Action::Open { target } => open_target(target),
            Action::Shortcut { name } => {
                let mut c = std::process::Command::new("/usr/bin/shortcuts");
                c.args(["run", name]);
                spawn_detached(c);
            }
            Action::Shell { command } => {
                let mut c = std::process::Command::new("/bin/sh");
                c.args(["-c", command]);
                spawn_detached(c);
            }
            Action::MicMute => {
                let _ = self.audio.cmd.send(audio::Cmd::ToggleMute);
            }
            Action::Media { command } => match command.as_str() {
                "next" => {
                    let _ = self.music_cmd.send(MusicCmd::Next);
                }
                "previous" | "prev" => {
                    let _ = self.music_cmd.send(MusicCmd::Previous);
                }
                _ => self.play_pause(),
            },
            Action::Clip { title } => {
                let _ = self.audio.cmd.send(audio::Cmd::PlayTitle(title.clone()));
            }
            Action::Page { page } => {
                if self.cfg.pages.contains(page) {
                    self.set_page(*page);
                }
            }
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
            Hit::OpenClaude => open_target("claude://"),
            Hit::Prev => {
                let _ = self.music_cmd.send(MusicCmd::Previous);
            }
            Hit::PlayPause => self.play_pause(),
            Hit::Next => {
                let _ = self.music_cmd.send(MusicCmd::Next);
            }
            Hit::Volume(frac) => {
                let v = (frac * 100.0).round() as u8;
                self.volume = Some(v);
                let _ = self.music_cmd.send(MusicCmd::SetVolume(v));
            }
            Hit::Shortcut(i) => {
                self.tile = i;
                self.run_shortcut(i);
            }
            Hit::Clip(i) => {
                self.tile = i;
                self.toggle_clip(i);
            }
            Hit::StopAll => {
                let _ = self.audio.cmd.send(audio::Cmd::StopAll);
            }
            Hit::MicMute => {
                let _ = self.audio.cmd.send(audio::Cmd::ToggleMute);
            }
            Hit::LeagueAuto => self.toggle_auto_accept(),
            Hit::LeagueAccept => {
                let _ = self.league_cmd.send(league::Cmd::Accept);
            }
            Hit::LeagueDecline => {
                let _ = self.league_cmd.send(league::Cmd::Decline);
            }
            Hit::LeagueFind => {
                let _ = self.league_cmd.send(league::Cmd::FindMatch);
            }
            Hit::LeagueCancel => {
                let _ = self.league_cmd.send(league::Cmd::CancelSearch);
            }
            Hit::Container(i) => self.open_container(i),
            Hit::DockerAction(a) => {
                if let Level::Detail(id) = &self.level {
                    let _ = self.docker_cmd.send(docker::Cmd::Action(id.clone(), a));
                }
            }
            Hit::DockerBack => {
                self.level = Level::Top;
                self.scroll = 0;
            }
        }
    }

    fn on_device(&mut self, input: Input) {
        if matches!(input, Input::Tap { .. } | Input::KnobTurn(_) | Input::KnobPress(_)) {
            self.last_input = Instant::now();
        }
        match input {
            Input::Connected { luminance, firmware } => {
                device::log(format!("device: Quake firmware {}", firmware.as_deref().unwrap_or("?")));
                self.connected = true;
                self.ring_sent = None;
                let _ = self.commands.send(Command::Mic(self.cfg.device.mic));
                if let Some(l) = luminance {
                    self.luminance = l;
                }
                self.update_ring();
            }
            Input::Disconnected => {
                self.connected = false;
                self.ring_sent = None;
            }
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
            MusicEvent::Artwork { url, image } => self.art = image.map(|p| (url, p)),
        }
        self.redraw();
    }

    // ------------------------------------------------------------ control socket

    fn level_name(&self) -> &'static str {
        match self.level {
            Level::Top => "top",
            Level::Focus => "focus",
            Level::Detail(_) => "detail",
        }
    }

    fn status_json(&self) -> String {
        let count = |st: State| self.sessions.iter().filter(|s| s.state == st).count();
        let playing: Vec<&str> = self.sounds.playing.iter().filter_map(|&i| self.sounds.clips.get(i)).map(|c| c.title.as_str()).collect();
        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "connected": self.connected,
            "page": self.page,
            "level": self.level_name(),
            "sessions": self.sessions.len(),
            "waiting": count(State::Waiting),
            "working": count(State::Working),
            "track": self.track.as_ref().map(|t| serde_json::json!({"title": t.title, "artist": t.artist, "playing": t.playing})),
            "league": {"running": self.league.running, "phase": self.league.phase, "autoAccept": self.cfg.league.auto_accept},
            "docker": {"ok": self.docker.ok, "engine": self.docker.engine, "running": self.docker.containers.iter().filter(|c| c.state == "running").count(), "containers": self.docker.containers.len()},
            "soundboard": {
                "routing": self.sounds.live,
                "listener": self.sounds.listener,
                "error": self.sounds.error,
                "playing": playing,
                "micMuted": self.sounds.mic_muted,
                "clips": self.sounds.clips.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
            },
        })
        .to_string()
    }

    fn on_ctl(&mut self, line: &str) -> Vec<u8> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let ok = b"ok\n".to_vec();
        let input = |i: Input| Some(i);
        let as_input = match parts.as_slice() {
            ["turn", d] => d.parse::<i8>().ok().and_then(|d| input(Input::KnobTurn(d.signum()))),
            ["press"] => input(Input::KnobPress(KNOB_PRESS)),
            ["double"] => input(Input::KnobPress(KNOB_DOUBLE)),
            ["hold"] => input(Input::KnobPress(KNOB_HOLD)),
            ["tap", x, y] => match (x.parse(), y.parse()) {
                (Ok(x), Ok(y)) => input(Input::Tap { x, y }),
                _ => None,
            },
            _ => None,
        };
        if let Some(i) = as_input {
            self.on_device(i);
            return ok;
        }
        match parts.as_slice() {
            ["status"] => (self.status_json() + "\n").into_bytes(),
            ["frame"] => self.render(render::W as u32, render::H as u32).pixmap.encode_png().unwrap_or_default(),
            ["reload"] => {
                self.reload_config(true);
                ok
            }
            ["page", p] => match serde_json::from_value::<PageId>(serde_json::Value::String(p.to_string())) {
                Ok(page) => {
                    self.last_input = Instant::now();
                    self.set_page(page);
                    self.redraw();
                    ok
                }
                Err(_) => format!("error: no page {p}\n").into_bytes(),
            },
            ["shortcut", n] => match n.parse::<usize>() {
                Ok(i) if i < self.cfg.shortcuts.len() => {
                    self.run_shortcut(i);
                    ok
                }
                _ => b"error: no such shortcut\n".to_vec(),
            },
            ["play", ..] => {
                let _ = self.audio.cmd.send(audio::Cmd::PlayTitle(parts[1..].join(" ")));
                ok
            }
            ["stop"] => {
                let _ = self.audio.cmd.send(audio::Cmd::StopAll);
                ok
            }
            _ => format!("error: unknown command {line:?}\n").into_bytes(),
        }
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
            Event::Sounds => {
                if let Ok(s) = self.audio.status.lock() {
                    self.sounds = s.clone();
                }
                self.tile = self.tile.min(self.tile_count().saturating_sub(1).max(0));
                self.redraw();
            }
            Event::League(league::Event::Status(st)) => {
                self.league = st;
                self.update_ring();
                if self.page == PageId::League {
                    self.redraw();
                }
            }
            Event::League(league::Event::QueuePop) => {
                if self.cfg.league.snap && self.cfg.pages.contains(&PageId::League) {
                    self.set_page(PageId::League);
                    self.last_input = Instant::now();
                }
                if self.cfg.buzzer {
                    let _ = self.commands.send(Command::Tone(MATCH_TONE.to_vec()));
                }
                self.redraw();
            }
            Event::Docker(st) => {
                self.docker = st;
                if let Level::Detail(id) = &self.level {
                    if self.page == PageId::Docker && !self.docker.containers.iter().any(|c| &c.id == id) {
                        self.level = Level::Focus;
                    }
                }
                self.tile = self.tile.min(self.tile_count().saturating_sub(1));
                if self.page == PageId::Docker {
                    self.redraw();
                }
            }
            Event::Ctl(line, reply) => {
                let out = self.on_ctl(&line);
                let _ = reply.send(out);
            }
        }
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::CursorMoved { position, .. } => self.cursor = (position.x, position.y),
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
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
        if now >= self.next_poll {
            self.reload_config(false);
            self.poll_status();
            self.next_poll = now + POLL;
        }
        if now >= self.next_display {
            self.ensure_window(el);
            self.next_display = now + DISPLAY_POLL;
        }
        if self.page == PageId::Docker && now >= self.next_logs {
            if let Level::Detail(id) = &self.level {
                let _ = self.docker_cmd.send(docker::Cmd::Logs(id.clone()));
            }
            self.next_logs = now + LOG_REFRESH;
        }
        if self.level != Level::Top && self.last_input.elapsed() > Duration::from_secs(self.cfg.idle_seconds.max(5)) {
            self.level = Level::Top;
            self.scroll = 0;
            self.redraw();
        }
        let (_, second) = clock();
        if second != self.drawn_second && (self.animating() || second % 60 == 0) {
            self.redraw();
        }
        let mut wake = self.next_poll.min(self.next_display);
        if self.page == PageId::Docker && matches!(self.level, Level::Detail(_)) {
            wake = wake.min(self.next_logs);
        }
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

/// One request per connection: a line in, a reply out, then close.
fn ctl_thread(proxy: winit::event_loop::EventLoopProxy<Event>, audio: Sender<audio::Cmd>) {
    let path = ctl_socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = match std::os::unix::net::UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => return device::log(format!("ctl: cannot listen on {}: {e}", path.display())),
    };
    for conn in listener.incoming().flatten() {
        let proxy = proxy.clone();
        let audio = audio.clone();
        std::thread::spawn(move || {
            let _ = conn.set_read_timeout(Some(Duration::from_secs(25)));
            let mut reader = BufReader::new(&conn);
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                return;
            }
            let line = line.trim().to_string();
            let reply = if line == "audio-devices" {
                let (tx, rx) = std::sync::mpsc::channel();
                let _ = audio.send(audio::Cmd::Devices(tx));
                // The first enumeration can take seconds while devices wake up.
                match rx.recv_timeout(Duration::from_secs(20)) {
                    Ok((inputs, outputs)) => (serde_json::json!({"inputs": inputs, "outputs": outputs}).to_string() + "\n").into_bytes(),
                    Err(_) => b"error: audio engine busy\n".to_vec(),
                }
            } else {
                let (tx, rx) = std::sync::mpsc::channel();
                if proxy.send_event(Event::Ctl(line, tx)).is_err() {
                    return;
                }
                rx.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| b"error: timeout\n".to_vec())
            };
            let mut w = &conn;
            let _ = w.write_all(&reply);
        });
    }
}

pub fn run() {
    let cfg = config::load();
    device::set_touch_flip(cfg.touch.flip_x, cfg.touch.flip_y);
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
    let audio = audio::spawn(cfg.soundboard.clone(), move || {
        let _ = p3.send_event(Event::Sounds);
    });
    let p5 = proxy.clone();
    let league_cmd = league::spawn(cfg.league.clone(), move |ev| {
        let _ = p5.send_event(Event::League(ev));
    });
    let docker_fast = Arc::new(AtomicBool::new(false));
    let p6 = proxy.clone();
    let docker_cmd = docker::spawn(cfg.docker.clone(), docker_fast.clone(), move |st| {
        let _ = p6.send_event(Event::Docker(st));
    });
    let p4 = proxy.clone();
    let audio_cmd = audio.cmd.clone();
    std::thread::Builder::new().name("ctl".into()).spawn(move || ctl_thread(p4, audio_cmd)).unwrap();
    let first_page = cfg.pages.first().copied().unwrap_or(PageId::Ai);
    let mut app = App {
        cfg_mtime: config::modified(),
        cfg,
        gfx: None,
        fonts: Fonts::load(),
        symbols: Symbols::default(),
        commands,
        music_cmd,
        music_fast,
        audio,
        sounds: audio::Status::default(),
        league_cmd,
        league: league::Status::default(),
        docker_cmd,
        docker: docker::Status::default(),
        docker_fast,
        next_logs: Instant::now(),
        sessions: Vec::new(),
        states: HashMap::new(),
        loaded_once: false,
        track: None,
        track_read: Instant::now(),
        volume: None,
        music_problem: None,
        art: None,
        page: first_page,
        level: Level::Top,
        sel: 0,
        scroll: 0,
        tile: 0,
        last_input: Instant::now(),
        connected: false,
        ring_sent: None,
        luminance: 255,
        undim_to: None,
        cursor: (0.0, 0.0),
        flash: None,
        next_poll: Instant::now(),
        next_display: Instant::now() + DISPLAY_POLL,
        drawn_second: 0,
    };
    event_loop.run_app(&mut app).expect("event loop run");
}
