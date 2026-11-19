//! The daemon: a borderless window pinned to the "DK-QUAKE" display, fed by the device
//! threads, the session files, the music poller and the soundboard engine.
//!
//! Navigation (knob), the same on every page:
//!   Top     turn: switch page   press: focus the page   hold: dim / undim the screen
//!   Focus   turn moves the selection (session, shortcut, clip) or sets volume (music);
//!           press opens / runs / plays
//!   Detail  (AI) turn scrolls the steps, press opens Claude
//!   double-click goes back one level, hold goes back to Top; idle returns to Top.

// One file per feature: its knob / touch / ring / timer handling (see app/<feature>.rs).
mod calendar;
mod call;
mod deploy;
mod digin_actions;
mod relevance;
use relevance::Urgency;
mod focus;
mod health;
mod stocks;
mod update;
mod night;
mod paste;
mod viz;
mod windows;

use crate::audio::{self, Audio};
use crate::config::{self, Action, Config, PageId};
use crate::device::{self, Command, Input};
use crate::{digin, docker, github, league, ops, sync};
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
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
/// One short blip when something new shows up on another page.
const NEW_STUFF_TONE: &[(u8, u64)] = &[(55, 60)];
/// Ring blink half-period while something new is waiting to be seen.
const BLINK: Duration = Duration::from_millis(420);
/// Don't yank the board away if it was used this recently.
const JUMP_QUIET: Duration = Duration::from_secs(15);
/// How long a session waiting on you gets for its prompt to reach the Quake before the board
/// jumps to the AI page instead.
const WAITING_GRACE: Duration = Duration::from_secs(4);
const LOG_REFRESH: Duration = Duration::from_secs(3);
/// A question or permission prompt ignores presses this soon after it appears.
const ASK_GUARD: Duration = Duration::from_millis(1000);
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
    Ops(ops::Status),
    Code(github::Status),
    Sync(sync::Status),
    Digin(digin::Status),
    /// Signed in to a Digin server: add it to the settings.
    DiginAdded(config::DiginAccount),
    DiginRefresh,
    /// A paired Mac has newer settings: (its name, its config JSON).
    PeerConfig(String, String),
    /// A Mac asks to pair; answer true to give it the key.
    PairRequest(String, Sender<bool>),
    /// Chosen from the menu bar icon's menu.
    Menu(crate::menubar::Action),
    /// From a feature's background thread.
    Feature(FeatureEvent),
    /// A control-socket request and where to send the reply.
    Ctl(String, Sender<Vec<u8>>),
}

pub enum FeatureEvent {
    Call(crate::call::Update),
    Focus(crate::focus::Update),
    Deploy(crate::deploy::Update),
    Paste(crate::clipboard::Update),
    Windows(crate::windows::Update),
    Calendar(crate::calendar::Update),
    Health(crate::health::Update),
    Stocks(crate::stocks::Update),
    Update(crate::update::Update),
    Night(crate::night::Update),
    Viz(crate::viz::Update),
}

/// A multiple-choice question from Claude, shown over everything until you answer.
struct Ask {
    session: String,
    machine: String,
    /// (question, header, options as (label, description), multi-select)
    questions: Vec<(String, String, Vec<(String, String)>, bool)>,
    at: usize,
    sel: usize,
    picked: Vec<bool>,
    answers: serde_json::Map<String, serde_json::Value>,
    reply: Sender<Vec<u8>>,
    until: Instant,
    /// When it appeared: presses in the first second are ignored (no accidental approvals).
    shown: Instant,
    /// The session it came from, and how far along it was: once it moves on (you answered
    /// Claude's own prompt on the Mac), the Quake's copy clears itself.
    session_id: String,
    mark: Option<(u32, usize, bool)>,
    mark_after: Instant,
    /// A permission prompt: (tag, what exactly, Claude's description).
    perm: Option<(String, String, String)>,
    /// A next-step question (the desktop app's box): the session has already finished its
    /// turn, so it's answered elsewhere once you send the session something new.
    next: bool,
}

/// A Mac waiting for you to allow pairing (on the board or the settings page).
struct PairAsk {
    name: String,
    reply: Sender<bool>,
    until: Instant,
}

/// Something new to look at: the ring blinks in its page's color until you touch the board.
struct Attention {
    page: PageId,
    reason: String,
    until: Instant,
    urgency: Urgency,
    /// The Mac it's about ("" = this one), for the arrow pointing at it; None = not a Mac's.
    machine: Option<String>,
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
    ops_cmd: Sender<ops::Cmd>,
    ops: ops::Status,
    ops_fast: Arc<AtomicBool>,
    code_cmd: Sender<github::Cmd>,
    code: github::Status,
    code_fast: Arc<AtomicBool>,
    digin_cmd: Sender<digin::Cmd>,
    digin: digin::Status,
    digin_fast: Arc<AtomicBool>,
    sync_cmd: Sender<sync::Cmd>,
    peers: sync::Status,
    /// Poll the other Macs only while the Quake is attached here.
    sync_want: Arc<AtomicBool>,
    /// `updatedAt` of the current settings, for the sync thread.
    config_at: Arc<AtomicU64>,
    pair_ask: Option<PairAsk>,
    // Dashboard graphs
    history: render::History,
    ai_total: Option<u64>,
    next_ai_sample: Instant,
    menubar: Option<crate::menubar::MenuBar>,
    proxy: winit::event_loop::EventLoopProxy<Event>,
    asks: std::collections::VecDeque<Ask>,
    // Features
    call: crate::call::Status,
    call_cmd: Sender<crate::call::Cmd>,
    focus: crate::focus::Status,
    focus_cmd: Sender<crate::focus::Cmd>,
    deploy: crate::deploy::Status,
    deploy_cmd: Sender<crate::deploy::Cmd>,
    paste: crate::clipboard::Status,
    paste_cmd: Sender<crate::clipboard::Cmd>,
    windows: crate::windows::Status,
    windows_cmd: Sender<crate::windows::Cmd>,
    calendar: crate::calendar::Status,
    calendar_cmd: Sender<crate::calendar::Cmd>,
    health: crate::health::Status,
    health_cmd: Sender<crate::health::Cmd>,
    stocks: crate::stocks::Status,
    stocks_cmd: Sender<crate::stocks::Cmd>,
    update: crate::update::Status,
    update_cmd: Sender<crate::update::Cmd>,
    /// Paired Macs asked to install a release: (the version, when). Done once they report it.
    update_peers: HashMap<String, (String, Instant)>,
    /// Why a paired Mac couldn't install.
    update_errors: HashMap<String, String>,
    /// The Mac row a first press armed (a second press within a few seconds installs).
    update_armed: Option<(usize, Instant)>,
    /// The knob's spin on the focus timer (how fast the length moves).
    focus_spin: crate::focus::Spin,
    /// The release already announced in the top bar.
    update_announced: String,
    /// The pages in the tab bar: `cfg.pages`, plus Update while it has something to show.
    pages: Vec<PageId>,
    night: crate::night::Status,
    night_cmd: Sender<crate::night::Cmd>,
    viz: crate::viz::Status,
    viz_cmd: Sender<crate::viz::Cmd>,
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
    /// When the page last changed (by you, an alert or a queue pop); idle counts from
    /// whichever of this and the last input is later.
    page_since: Instant,
    // Device
    connected: bool,
    ring_sent: Option<Command>,
    luminance: u8,
    undim_to: Option<u8>,
    cursor: (f64, f64),
    flash: Option<(Hit, Instant)>,
    attention: Option<Attention>,
    /// The page the board last jumped to for an alert, how urgent it was, and when.
    jumped: Option<(PageId, Urgency, Instant)>,
    /// An urgent alert that came while you were using the board: jump once you stop.
    pending_jump: Option<(PageId, Urgency, Instant)>,
    /// A session that just started waiting on you: (id, name, when to alert if no prompt for
    /// it reached the Quake by then).
    waiting_alert: Option<(String, String, Instant)>,
    /// Health problems already announced (app/health.rs `problems` keys).
    health_flags: std::collections::HashSet<String>,
    blink_on: bool,
    next_blink: Instant,
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
        let desk = self.desk();
        View {
            pages: &self.pages,
            page: self.page,
            accent: self.accent(),
            shortcuts: &self.cfg.shortcuts,
            sounds: &self.sounds,
            tile: self.tile,
            league: &self.league,
            league_cfg: &self.cfg.league,
            docker: &self.docker,
            ops: &self.ops,
            code: &self.code,
            digin: &self.digin,
            settings: render::SettingsView {
                screen: self.undim_to.unwrap_or(self.luminance),
                ring: self.cfg.ring.brightness,
                idle: self.cfg.idle_seconds,
                idle_home: self.cfg.idle_home,
                ring_on: self.cfg.ring.enabled,
                alert: self.cfg.ring.alert,
                blink: self.cfg.alerts.blink,
                jump: self.cfg.alerts.jump,
                buzzer: self.cfg.buzzer,
                mic: self.cfg.device.mic,
                auto_accept: self.cfg.league.auto_accept,
                snap: self.cfg.league.snap,
            },
            ring: &self.cfg.ring,
            history: &self.history,
            ask: self.asks.front().and_then(|a| {
                let (question, header, options, multi) = a.questions.get(a.at)?;
                Some(render::AskView {
                    session: &a.session,
                    machine: &a.machine,
                    header,
                    question,
                    options,
                    picked: &a.picked,
                    multi: *multi,
                    sel: a.sel,
                    index: a.at,
                    count: a.questions.len(),
                    secs_left: a.until.saturating_duration_since(Instant::now()).as_secs(),
                    queued: self.asks.len() - 1,
                    detail: a.perm.as_ref().map(|(t, w, d)| (t.as_str(), w.as_str(), d.as_str())),
                })
            }),
            call: &self.call,
            focus: &self.focus,
            deploy: &self.deploy,
            paste: &self.paste,
            windows: &self.windows,
            calendar: &self.calendar,
            health: &self.health,
            stocks: &self.stocks,
            update: render::update::UpdateView { status: &self.update, rows: self.update_rows(), armed: self.update_armed_row() },
            night: &self.night,
            viz: &self.viz,
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
            notice: self.attention.as_ref().map(|a| (a.reason.as_str(), config::hex(self.cfg.ring.color_for(a.page), 0xF2B705), a.page, a.machine.as_deref().and_then(|m| desk.side(m)))),
            desk,
            tick: second % 2 == 0,
        }
    }

    /// Which Mac is where, for the board's arrows and Join buttons.
    fn desk(&self) -> render::Desk<'_> {
        render::Desk { here: &self.peers.name, sides: &self.cfg.sides, macs: self.desk_macs(), join_on: self.cfg.calendar.join_on }
    }

    /// The Macs a meeting can be joined on: this one first, then the paired ones answering.
    pub(super) fn desk_macs(&self) -> Vec<String> {
        std::iter::once(self.peers.name.clone()).chain(self.peers.peers.iter().filter(|p| p.ok).map(|p| p.name.clone())).collect()
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
            _ if !self.asks.is_empty() => true, // the countdown
            _ if self.feature_animating() => true,
            (PageId::Dash, _) => self.track.as_ref().is_some_and(|t| t.playing) || self.sessions.iter().any(|s| s.state == State::Working),
            (PageId::Ai, Level::Detail(_)) => true,
            // The activity line's turn timer ticks every second.
            (PageId::Ai, _) => self.sessions.iter().any(|s| s.state == State::Working),
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
        let mut cfg = match config::try_load() {
            Ok(c) => c,
            Err(e) => {
                // Never fall back to defaults here: they'd be stamped and copied to the other Mac.
                device::log(format!("config: {e}; keeping the current settings"));
                return;
            }
        };
        // Changed outside quaked (by hand): stamp it, so the other Mac takes it too.
        if cfg.updated_at <= self.cfg.updated_at && !same_settings(&cfg, &self.cfg) {
            cfg.updated_at = status::now_ms();
            self.adopt(cfg, force);
            self.write_config();
            return;
        }
        self.adopt(cfg, force);
    }

    /// Applies settings: tells each part what changed, then keeps them.
    fn adopt(&mut self, cfg: Config, force: bool) {
        if cfg != self.cfg || force {
            if cfg.soundboard != self.cfg.soundboard || force {
                let _ = self.audio.cmd.send(audio::Cmd::Configure(cfg.soundboard.clone()));
            }
            if cfg.league != self.cfg.league || force {
                let _ = self.league_cmd.send(league::Cmd::Configure(cfg.league.clone()));
            }
            if cfg.device != self.cfg.device && self.connected {
                let _ = self.commands.send(Command::Mic(cfg.device.mic));
                if cfg.device.screen_brightness != self.cfg.device.screen_brightness {
                    if let Some(b) = cfg.device.screen_brightness {
                        self.undim_to = None;
                        self.set_luminance(b.max(render::SCREEN_MIN));
                    }
                }
            }
            if cfg.docker != self.cfg.docker || force {
                let _ = self.docker_cmd.send(docker::Cmd::Configure(cfg.docker.clone()));
            }
            if cfg.ops != self.cfg.ops || force {
                let _ = self.ops_cmd.send(ops::Cmd::Configure(cfg.ops.clone()));
            }
            if cfg.code != self.cfg.code || force {
                let _ = self.code_cmd.send(github::Cmd::Configure(cfg.code.clone()));
            }
            if cfg.digin != self.cfg.digin || force {
                let _ = self.digin_cmd.send(digin::Cmd::Configure(cfg.digin.clone()));
            }
            if cfg.call != self.cfg.call || force {
                let _ = self.call_cmd.send(crate::call::Cmd::Configure(cfg.call.clone()));
            }
            if cfg.focus != self.cfg.focus || force {
                let _ = self.focus_cmd.send(crate::focus::Cmd::Configure(cfg.focus.clone()));
            }
            if cfg.deploy != self.cfg.deploy || force {
                let _ = self.deploy_cmd.send(crate::deploy::Cmd::Configure(cfg.deploy.clone()));
            }
            if cfg.paste != self.cfg.paste || force {
                let _ = self.paste_cmd.send(crate::clipboard::Cmd::Configure(cfg.paste.clone()));
            }
            if cfg.windows != self.cfg.windows || force {
                let _ = self.windows_cmd.send(crate::windows::Cmd::Configure(cfg.windows.clone()));
            }
            if cfg.calendar != self.cfg.calendar || force {
                let _ = self.calendar_cmd.send(crate::calendar::Cmd::Configure(cfg.calendar.clone()));
            }
            if cfg.health != self.cfg.health || force {
                let _ = self.health_cmd.send(crate::health::Cmd::Configure(cfg.health.clone()));
            }
            if cfg.stocks != self.cfg.stocks || force {
                let _ = self.stocks_cmd.send(crate::stocks::Cmd::Configure(cfg.stocks.clone()));
            }
            if cfg.night != self.cfg.night || force {
                let _ = self.night_cmd.send(crate::night::Cmd::Configure(cfg.night.clone()));
            }
            if cfg.updates != self.cfg.updates || force {
                let _ = self.update_cmd.send(crate::update::Cmd::Configure(cfg.updates.clone()));
            }
            if cfg.viz != self.cfg.viz || force {
                let _ = self.viz_cmd.send(crate::viz::Cmd::Configure(cfg.viz.clone()));
            }
            if cfg.sync != self.cfg.sync || force {
                let _ = self.sync_cmd.send(sync::Cmd::Configure(cfg.sync.clone()));
            }
            self.cfg = cfg;
            device::set_touch_flip(self.cfg.touch.flip_x, self.cfg.touch.flip_y);
            if self.cfg.pages.is_empty() {
                self.cfg.pages = vec![PageId::Ai];
            }
            self.refresh_pages();
            if !self.pages.contains(&self.page) {
                self.set_page(self.pages[0]);
            }
            device::log("config: loaded");
            self.update_ring();
            self.redraw();
        }
        self.config_at.store(self.cfg.updated_at, Ordering::Relaxed);
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
        } else if let Some(cmd) = self.night_ring() {
            // Night mode keeps the ring dark (it never runs during ready checks, questions or alerts).
            cmd
        } else if self.league.phase == "ReadyCheck" {
            let (hue, sat) = config::hue_sat(config::hex(&r.league, 0xC89B3C));
            Command::Ring { effect: proto::EFFECT_BREATHING, hue, sat, brightness: r.brightness.max(180), speed: 220 }
        } else if !self.asks.is_empty() {
            let (hue, sat) = config::hue_sat(config::hex(&r.alert_color, 0xF2B705));
            Command::Ring { effect: proto::EFFECT_BREATHING, hue, sat, brightness: r.brightness.max(180), speed: 220 }
        } else if let Some(a) = self.attention.as_ref().filter(|_| self.cfg.alerts.blink) {
            let (hue, sat) = config::hue_sat(config::hex(r.color_for(a.page), 0xF2B705));
            let brightness = if self.blink_on { r.brightness.max(160) } else { 0 };
            Command::Ring { effect: proto::EFFECT_SOLID, hue, sat, brightness, speed: 128 }
        } else if let Some(cmd) = self.feature_ring() {
            cmd
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
        let mut sessions = status::load(status::now_ms());
        // Sessions on the other Macs, tagged with where they run.
        for p in &self.peers.peers {
            sessions.extend(p.sessions.iter().cloned().map(|mut s| {
                s.machine = p.name.clone();
                s
            }));
        }
        sessions.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)));
        // Sessions done or idle for a while drop off (never ones working or waiting on you).
        if self.cfg.session_idle_minutes > 0 {
            let limit = self.cfg.session_idle_minutes * 60_000;
            let now = status::now_ms();
            sessions.retain(|s| !matches!(s.state, State::Done | State::Idle) || now.saturating_sub(s.since) <= limit);
        }
        let states: HashMap<String, State> = sessions.iter().map(|s| (s.id.clone(), s.state)).collect();
        let chirp = self.loaded_once
            && states.iter().any(|(id, st)| *st == State::Waiting && self.states.get(id) != Some(&State::Waiting));
        if chirp && self.cfg.buzzer {
            let _ = self.commands.send(Command::Tone(NEEDS_YOU_TONE.to_vec()));
        }
        let newly_waiting = if self.loaded_once {
            sessions.iter().position(|s| s.state == State::Waiting && self.states.get(&s.id) != Some(&State::Waiting))
        } else {
            None
        };
        if sessions != self.sessions {
            let selected = self.sessions.get(self.sel).map(|s| s.id.clone());
            self.sessions = sessions;
            if let Some(id) = selected {
                if let Some(i) = self.sessions.iter().position(|s| s.id == id) {
                    self.sel = i;
                }
            }
            self.sel = self.sel.min(self.sessions.len().saturating_sub(1));
            // An opened session that's gone: back to the cards. Only on the AI page; other
            // pages' detail views (a stock, a Digin issue) aren't sessions.
            if let (PageId::Ai, Level::Detail(id)) = (self.page, &self.level) {
                if !self.sessions.iter().any(|s| &s.id == id) {
                    self.level = Level::Focus;
                }
            }
            self.redraw();
        }
        self.states = states;
        self.loaded_once = true;
        if !self.asks.is_empty() {
            self.drop_answered_elsewhere();
        }
        // Dashboard: Claude steps per 30 seconds, across every Mac.
        if Instant::now() >= self.next_ai_sample {
            let total: u64 = self.sessions.iter().map(|s| s.step_count as u64).sum();
            if let Some(prev) = self.ai_total {
                render::History::push(&mut self.history.ai_steps, total.saturating_sub(prev) as f32);
            }
            self.ai_total = Some(total);
            self.next_ai_sample = Instant::now() + Duration::from_secs(30);
        }
        if let Some(i) = newly_waiting {
            if let Some(s) = self.sessions.get(i) {
                // Its question or permission prompt usually shows on the Quake by itself: wait a
                // moment, and jump to the AI page only if nothing did (app/relevance.rs).
                self.waiting_alert = Some((s.id.clone(), s.name.clone(), Instant::now() + WAITING_GRACE));
            }
        }
        self.update_ring();
    }

    // ------------------------------------------------------------ new stuff

    /// Flags something new on `page`: blink the ring, show why, and jump there unless
    /// the board is in use. Returns whether it jumped.
    fn alert(&mut self, page: PageId, reason: String, chirp: bool) -> bool {
        self.alert_with(page, reason, chirp, Urgency::of(page))
    }

    /// `alert` with an urgency: what's on the board decides whether it may take it
    /// (see app/relevance.rs). Returns whether it jumped.
    fn alert_with(&mut self, page: PageId, reason: String, chirp: bool, urgency: Urgency) -> bool {
        self.alert_from(page, reason, chirp, urgency, None)
    }

    /// `alert_with` for something on one Mac (`machine`, "" = this one): the top bar points
    /// to the side that Mac sits on (config `sides`).
    fn alert_from(&mut self, page: PageId, reason: String, chirp: bool, urgency: Urgency, machine: Option<&str>) -> bool {
        let a = self.cfg.alerts.clone();
        let wanted = match page {
            PageId::Ai => a.ai,
            PageId::Code => a.code,
            PageId::Ops => a.ops,
            PageId::Docker => a.docker,
            PageId::Digin => a.digin,
            PageId::Health => a.health,
            _ => true,
        };
        if !wanted || !self.pages.contains(&page) {
            return false;
        }
        device::log(format!("new: {reason}"));
        // A more urgent notice that's still up keeps the top bar.
        if self.attention.as_ref().is_some_and(|cur| cur.urgency > urgency && cur.page != page && Instant::now() < cur.until) {
            return false;
        }
        let jump = if !self.may_jump(page, urgency) {
            false
        } else if self.last_input.elapsed() > JUMP_QUIET {
            self.jump_for(page, urgency);
            true
        } else {
            self.defer_jump(page, urgency);
            false
        };
        self.attention = Some(Attention { page, reason, until: Instant::now() + Duration::from_secs(a.seconds.max(10)), urgency, machine: machine.map(str::to_string) });
        if urgency > Urgency::Low {
            self.blink_on = true;
            self.next_blink = Instant::now() + BLINK;
            if chirp && self.cfg.buzzer {
                let _ = self.commands.send(Command::Tone(NEW_STUFF_TONE.to_vec()));
            }
        }
        self.update_ring();
        self.redraw();
        jump
    }

    /// One line for several new things: the first, plus how many more.
    fn summarize(mut news: Vec<String>) -> Option<String> {
        let n = news.len();
        match n {
            0 => None,
            1 => news.pop(),
            _ => Some(format!("{}  (+{} more)", news[0], n - 1)),
        }
    }

    // ------------------------------------------------------------ features (dispatch)

    fn on_feature(&mut self, f: FeatureEvent) {
        match f {
            FeatureEvent::Call(u) => self.on_call(u),
            FeatureEvent::Focus(u) => self.on_focus(u),
            FeatureEvent::Deploy(u) => self.on_deploy(u),
            FeatureEvent::Paste(u) => self.on_paste(u),
            FeatureEvent::Windows(u) => self.on_windows(u),
            FeatureEvent::Calendar(u) => self.on_calendar(u),
            FeatureEvent::Health(u) => self.on_health(u),
            FeatureEvent::Stocks(u) => self.on_stocks(u),
            FeatureEvent::Update(u) => self.on_update(u),
            FeatureEvent::Night(u) => self.on_night(u),
            FeatureEvent::Viz(u) => self.on_viz(u),
        }
        self.update_ring();
    }

    /// The first feature that wants the ring (call, focus, deploy, visualizer).
    fn feature_ring(&self) -> Option<Command> {
        self.call_ring().or_else(|| self.focus_ring()).or_else(|| self.deploy_ring()).or_else(|| self.viz_ring())
    }

    fn feature_tick(&mut self, now: Instant) -> Option<Instant> {
        [self.call_tick(now), self.focus_tick(now), self.deploy_tick(now), self.paste_tick(now), self.windows_tick(now), self.calendar_tick(now), self.health_tick(now), self.night_tick(now), self.viz_tick(now), self.update_tick(now)].into_iter().flatten().min()
    }

    fn feature_animating(&self) -> bool {
        self.call_animating() || self.focus_animating() || self.deploy_animating() || self.paste_animating() || self.windows_animating() || self.calendar_animating() || self.health_animating() || self.night_animating() || self.viz_animating()
    }

    /// A hold on a feature's page, if it wants it.
    fn feature_hold(&mut self) -> bool {
        match self.page {
            PageId::Focus => self.focus_hold(),
            PageId::Deploy => self.deploy_hold(),
            PageId::Paste => self.paste_hold(),
            PageId::Windows => self.windows_hold(),
            PageId::Calendar => self.calendar_hold(),
            PageId::Health => self.health_hold(),
            _ => false,
        }
    }

    fn feature_tap(&mut self, p: PageId, i: usize) {
        if self.call_active() {
            return self.call_tap(i);
        }
        match p {
            PageId::Focus => self.focus_tap(i),
            PageId::Deploy => self.deploy_tap(i),
            PageId::Paste => self.paste_tap(i),
            PageId::Windows => self.windows_tap(i),
            PageId::Calendar => self.calendar_tap(i),
            PageId::Health => self.health_tap(i),
            PageId::Stocks => self.stocks_tap(i),
            PageId::Update => self.update_tap(i),
            _ => {}
        }
    }

    fn features_json(&self) -> serde_json::Value {
        serde_json::json!({
            "call": self.call_json(),
            "focus": self.focus_json(),
            "deploy": self.deploy_json(),
            "paste": self.paste_json(),
            "windows": self.windows_json(),
            "calendar": self.calendar_json(),
            "health": self.health_json(),
            "stocks": self.stocks_json(),
            "update": self.update_json(),
            "night": self.night_json(),
            "viz": self.viz_json(),
        })
    }

    // ------------------------------------------------------------ menu bar

    /// Shows, hides and refreshes the menu bar icon (it only redraws when something changed).
    fn refresh_menubar(&mut self) {
        if !self.cfg.menu_bar {
            self.menubar = None;
            return;
        }
        if self.menubar.is_none() {
            let proxy = Mutex::new(self.proxy.clone());
            self.menubar = crate::menubar::MenuBar::new(move |a| {
                let _ = proxy.lock().unwrap().send_event(Event::Menu(a));
            });
        }
        let model = self.menu_model();
        if let Some(m) = &mut self.menubar {
            m.update(&model);
        }
    }

    fn menu_model(&self) -> crate::menubar::Model {
        let count = |st: State| self.sessions.iter().filter(|s| s.state == st).count();
        let (waiting, working) = (count(State::Waiting), count(State::Working));
        let remote = self.peers.peers.iter().find(|p| p.ok && p.board);
        let mut lines = vec![match (self.connected, remote) {
            (true, _) => "Quake connected to this Mac".to_string(),
            (false, Some(p)) => format!("Quake connected to {}", p.name),
            (false, None) => "Quake not connected".to_string(),
        }];
        lines.push(if self.sessions.is_empty() {
            "No Claude sessions".to_string()
        } else if waiting > 0 {
            format!("Claude: {waiting} need{} you · {working} working", if waiting == 1 { "s" } else { "" })
        } else {
            format!("Claude: {working} working · {} sessions", self.sessions.len())
        });
        if let Some(s) = self.sessions.iter().filter(|s| s.state == State::Working && !s.comment.is_empty()).max_by_key(|s| s.updated_at) {
            let said: String = s.comment.chars().take(80).collect();
            lines.push(format!("{}: {}{}", s.name, said, if s.comment.chars().count() > 80 { "…" } else { "" }));
        }
        if let Some(t) = self.track.as_ref().filter(|t| t.playing) {
            lines.push(format!("Playing: {} — {}", t.title, t.artist));
        }
        let paired: Vec<&str> = self.peers.peers.iter().filter(|p| p.ok).map(|p| p.name.as_str()).collect();
        if !paired.is_empty() {
            lines.push(format!("Paired with {}", paired.join(", ")));
        }
        crate::menubar::Model {
            board: self.connected || remote.is_some(),
            waiting,
            lines,
            pair: self.pair_ask.as_ref().map(|a| a.name.clone()),
            pages: if self.connected { self.pages.iter().map(|p| (p.title().to_string(), *p == self.page)).collect() } else { Vec::new() },
        }
    }

    fn on_menu(&mut self, action: crate::menubar::Action) {
        use crate::menubar::Action;
        extern "C" {
            fn getuid() -> u32;
        }
        let service = format!("gui/{}/dev.arlint.quaked", unsafe { getuid() });
        match action {
            Action::Settings => open_target(&crate::web::url()),
            Action::Restart => {
                device::log("menu: restart");
                let mut c = std::process::Command::new("/bin/launchctl");
                c.args(["kickstart", "-k", &service]);
                spawn_detached(c);
            }
            Action::Quit => {
                // Stops it until the next login (launchd would restart a plain exit... only
                // on failure, but bootout also keeps it from coming back right away).
                device::log("menu: quit");
                let mut c = std::process::Command::new("/bin/launchctl");
                c.args(["bootout", &service]);
                spawn_detached(c);
            }
            Action::Pair(allow) => self.answer_pair(allow),
            Action::Page(i) => {
                if let Some(p) = self.pages.get(i).copied() {
                    self.last_input = Instant::now();
                    self.set_page(p);
                    self.redraw();
                }
            }
        }
    }

    // ------------------------------------------------------------ Claude's questions

    /// A question from `quaked ask` (or another Mac): show it here, or pass it to the Mac
    /// that has the Quake. Answered later through `reply`.
    fn start_ask(&mut self, json: &str, reply: Sender<Vec<u8>>) {
        const SKIP: &[u8] = b"{\"skip\":true}\n";
        if !self.cfg.ask.enabled {
            let _ = reply.send(SKIP.to_vec());
            return;
        }
        let Ok(mut v) = serde_json::from_str::<serde_json::Value>(json) else {
            let _ = reply.send(SKIP.to_vec());
            return;
        };
        if !self.connected {
            match self.peers.peers.iter().find(|p| p.ok && p.board) {
                Some(p) => {
                    if v.get("machine").and_then(|m| m.as_str()).is_none_or(str::is_empty) {
                        v["machine"] = serde_json::Value::String(self.peers.name.clone());
                    }
                    let (host, port, key, line) = (p.host.clone(), p.port, self.cfg.sync.key.clone(), format!("ctl ask {v}"));
                    device::log(format!("ask: on {}'s Quake", p.name));
                    std::thread::spawn(move || {
                        let out = sync::request_long(&host, port, &key, &line, Duration::from_secs(590)).unwrap_or_else(|_| SKIP.to_vec());
                        let _ = reply.send(out);
                    });
                }
                None => {
                    let _ = reply.send(SKIP.to_vec());
                }
            }
            return;
        }
        let text = |q: &serde_json::Value, k: &str| q.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        if v["kind"] == "permission" {
            if !self.cfg.ask.permissions {
                let _ = reply.send(SKIP.to_vec());
                return;
            }
            let tool = text(&v, "tool");
            let what = match tool.as_str() {
                "Bash" => "run a command",
                "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => "change a file",
                "Read" | "Glob" | "Grep" => "read files",
                "WebFetch" => "fetch a web page",
                "WebSearch" => "search the web",
                t if t.starts_with("mcp__") => "use a connector tool",
                _ => "use a tool",
            };
            let short = tool.rsplit("__").next().unwrap_or(&tool).to_string();
            let mut options = vec![("Allow".to_string(), "just this once".to_string())];
            let rule = text(&v, "rule");
            if !rule.is_empty() {
                options.push(("Always allow".to_string(), format!("don't ask again for {rule}")));
            }
            options.push(("Deny".to_string(), "tell Claude no".to_string()));
            let ask = Ask {
                session: text(&v, "session"),
                machine: text(&v, "machine"),
                questions: vec![(format!("Claude wants to {what}  ·  {short}"), "Permission".to_string(), options, false)],
                at: 0,
                sel: 0,
                picked: Vec::new(),
                answers: serde_json::Map::new(),
                reply,
                until: Instant::now(),
                shown: Instant::now(),
                perm: Some((text(&v, "tag"), text(&v, "detail"), text(&v, "description"))),
                next: false,
                session_id: text(&v, "sessionId"),
                mark: None,
                mark_after: Instant::now(),
            };
            let mut ask = ask;
            self.watch_session(&mut ask);
            device::log(format!("ask: {} asks to {what}: {}", ask.session, text(&v, "detail")));
            self.asks.push_back(ask);
            if self.asks.len() == 1 {
                self.show_ask();
            }
            return;
        }
        let questions: Vec<(String, String, Vec<(String, String)>, bool)> = v["questions"]
            .as_array()
            .map(|qs| {
                qs.iter()
                    .map(|q| {
                        let options = q["options"]
                            .as_array()
                            .map(|os| os.iter().map(|o| match o.as_str() {
                                Some(label) => (label.to_string(), String::new()),
                                None => (text(o, "label"), text(o, "description")),
                            }).filter(|(l, _)| !l.is_empty()).take(5).collect())
                            .unwrap_or_default();
                        (text(q, "question"), text(q, "header"), options, q["multiSelect"].as_bool().unwrap_or(false))
                    })
                    .filter(|q: &(String, String, Vec<(String, String)>, bool)| !q.0.is_empty() && !q.2.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if questions.is_empty() {
            let _ = reply.send(SKIP.to_vec());
            return;
        }
        let picked = vec![false; questions[0].2.len()];
        let ask = Ask {
            session: text(&v, "session"),
            machine: text(&v, "machine"),
            questions,
            at: 0,
            sel: 0,
            picked,
            answers: serde_json::Map::new(),
            reply,
            until: Instant::now() + Duration::from_secs(self.cfg.ask.timeout_seconds.clamp(15, 580)),
            shown: Instant::now(),
            perm: None,
            next: v["kind"] == "next",
            session_id: text(&v, "sessionId"),
            mark: None,
            mark_after: Instant::now(),
        };
        let mut ask = ask;
        self.watch_session(&mut ask);
        device::log(format!("ask: {} asks \"{}\"", ask.session, ask.questions[0].0));
        self.asks.push_back(ask);
        if self.asks.len() == 1 {
            self.show_ask();
        }
    }

    fn show_ask(&mut self) {
        if self.cfg.buzzer {
            let _ = self.commands.send(Command::Tone(NEEDS_YOU_TONE.to_vec()));
        }
        // A fresh question gets the full timeout from when it shows.
        if let Some(a) = self.asks.front_mut() {
            a.until = Instant::now() + Duration::from_secs(self.cfg.ask.timeout_seconds.clamp(15, 580));
            a.shown = Instant::now();
        }
        self.update_ring();
        self.redraw();
    }

    /// Starts watching the asking session, so the Quake's copy clears when it moves on.
    fn watch_session(&self, ask: &mut Ask) {
        if ask.session_id.is_empty() {
            return;
        }
        if ask.machine.is_empty() {
            // This Mac's session: read it fresh (the hook wrote it just before asking).
            let next = ask.next;
            ask.mark = status::load(status::now_ms()).iter().find(|s| s.id == ask.session_id).map(|s| ask_mark(next, s));
        } else {
            // The other Mac's sessions sync every 2 seconds: start watching after that.
            ask.mark_after = Instant::now() + Duration::from_secs(4);
        }
    }

    /// Clears prompts you've already answered on the Mac (their session has moved on).
    fn drop_answered_elsewhere(&mut self) {
        let mut gone = Vec::new();
        for (i, a) in self.asks.iter_mut().enumerate() {
            let Some(s) = self.sessions.iter().find(|s| s.id == a.session_id) else { continue };
            let now = ask_mark(a.next, s);
            match a.mark {
                None if !a.session_id.is_empty() && Instant::now() >= a.mark_after => a.mark = Some(now),
                Some(m) if m != now => gone.push(i),
                _ => {}
            }
        }
        for i in gone.into_iter().rev() {
            if let Some(a) = self.asks.get(i) {
                device::log(format!("ask: {} answered in Claude; clearing the Quake", a.session));
            }
            if i == 0 {
                self.finish_ask(false);
            } else if let Some(a) = self.asks.remove(i) {
                let _ = a.reply.send(b"{\"skip\":true}\n".to_vec());
            }
        }
    }

    /// Card `i` pressed or tapped.
    fn ask_choose(&mut self, i: usize) {
        let Some(a) = self.asks.front_mut() else { return };
        let Some((question, _, options, multi)) = a.questions.get(a.at).cloned() else { return };
        let skip = render::ask_cards(options.len(), multi) - 1;
        if i == skip {
            return self.finish_ask(false);
        }
        let answer = if multi {
            if i < options.len() {
                a.picked[i] = !a.picked[i];
                self.redraw();
                return;
            }
            let labels: Vec<&str> = options.iter().zip(&a.picked).filter(|(_, p)| **p).map(|(o, _)| o.0.as_str()).collect();
            if labels.is_empty() {
                return; // Done with nothing picked: keep asking
            }
            labels.join(", ")
        } else {
            match options.get(i) {
                Some(o) => o.0.clone(),
                None => return,
            }
        };
        a.answers.insert(question, serde_json::Value::String(answer));
        a.at += 1;
        a.sel = 0;
        if let Some(next) = a.questions.get(a.at) {
            a.picked = vec![false; next.2.len()];
            self.redraw();
        } else {
            self.finish_ask(true);
        }
    }

    /// Sends the answers back (or hands the question back to Claude) and shows the next one.
    fn finish_ask(&mut self, answered: bool) {
        let Some(a) = self.asks.pop_front() else { return };
        let out = if answered { serde_json::json!({"answers": a.answers}) } else { serde_json::json!({"skip": true}) };
        device::log(format!("ask: {} {}", a.session, if answered { format!("answered on the Quake {out}") } else { "dismissed".into() }));
        let _ = a.reply.send((out.to_string() + "\n").into_bytes());
        self.last_input = Instant::now();
        if self.asks.is_empty() {
            self.update_ring();
            self.redraw();
        } else {
            self.show_ask();
        }
    }

    /// The pairing question in the top bar, with the ring blinking.
    fn show_pair_ask(&mut self) {
        let Some(a) = &self.pair_ask else { return };
        let reason = format!("{} wants to pair  ·  press to allow  ·  double-click to refuse", a.name);
        self.attention = Some(Attention { page: self.page, reason, until: a.until, urgency: Urgency::Critical, machine: None });
        self.blink_on = true;
        self.next_blink = Instant::now() + BLINK;
        if self.cfg.buzzer {
            let _ = self.commands.send(Command::Tone(NEW_STUFF_TONE.to_vec()));
        }
        self.update_ring();
        self.redraw();
    }

    fn clear_attention(&mut self) {
        if self.attention.take().is_some() {
            self.update_ring();
            self.redraw();
        }
    }

    // ------------------------------------------------------------ navigation

    fn set_page(&mut self, p: PageId) {
        if p != self.page {
            self.page_since = Instant::now();
        }
        self.page = p;
        self.level = Level::Top;
        self.scroll = 0;
        self.tile = 0;
        self.music_fast.store(p == PageId::Music || p == PageId::Dash, Ordering::Relaxed);
        self.docker_fast.store(p == PageId::Docker, Ordering::Relaxed);
        self.ops_fast.store(p == PageId::Ops, Ordering::Relaxed);
        self.code_fast.store(p == PageId::Code, Ordering::Relaxed);
        self.digin_fast.store(p == PageId::Digin, Ordering::Relaxed);
        self.focus_page_shown(p == PageId::Focus);
        self.deploy_page_shown(p == PageId::Deploy);
        self.paste_page_shown(p == PageId::Paste);
        self.windows_page_shown(p == PageId::Windows);
        self.calendar_page_shown(p == PageId::Calendar);
        self.health_page_shown(p == PageId::Health);
        self.stocks_page_shown(p == PageId::Stocks);
        self.update_page_shown(p == PageId::Update);
        if p == PageId::Code {
            let _ = self.code_cmd.send(github::Cmd::Refresh);
        }
        self.update_ring();
    }

    /// Writes the config (after a change made on the board, like the auto-accept toggle).
    fn save_config(&mut self) {
        self.cfg.updated_at = status::now_ms();
        self.write_config();
    }

    /// Writes the settings as they are (keeping their `updatedAt`).
    fn write_config(&mut self) {
        if config::save(&self.cfg) {
            self.cfg_mtime = config::modified();
        }
        self.config_at.store(self.cfg.updated_at, Ordering::Relaxed);
    }

    /// Settings from the settings page.
    fn config_put(&mut self, json: &str) -> Vec<u8> {
        match serde_json::from_str::<Config>(json) {
            Ok(mut c) => {
                c.updated_at = status::now_ms();
                self.adopt(c, false);
                self.write_config();
                b"ok\n".to_vec()
            }
            Err(e) => format!("error: {e}\n").into_bytes(),
        }
    }

    fn answer_pair(&mut self, allow: bool) {
        if let Some(a) = self.pair_ask.take() {
            let _ = a.reply.send(allow);
            device::log(format!("sync: {} pairing with {}", if allow { "allowed" } else { "refused" }, a.name));
            self.clear_attention();
        }
    }

    /// Everything the settings page needs, in one reply.
    fn state_json(&self) -> String {
        let route = if self.connected {
            serde_json::json!({"kind": "local"})
        } else {
            match self.peers.peers.iter().find(|p| p.ok && p.board) {
                Some(p) => serde_json::json!({"kind": "remote", "name": p.name, "host": p.host, "port": p.port}),
                None => serde_json::json!({"kind": "none"}),
            }
        };
        let folder = if self.cfg.soundboard.folder.is_empty() { config::Soundboard::default().folder } else { self.cfg.soundboard.folder.clone() };
        serde_json::json!({
            "machine": self.peers.name,
            "config": self.cfg,
            "status": serde_json::from_str::<serde_json::Value>(&self.status_json()).unwrap_or_default(),
            "peers": self.peers.peers.iter().map(|p| serde_json::json!({
                "name": p.name, "host": p.host, "port": p.port, "ok": p.ok, "error": p.error,
                "board": p.board, "sessions": p.sessions.len(), "configAt": p.config_at, "build": p.build,
            })).collect::<Vec<_>>(),
            "pairAsk": self.pair_ask.as_ref().map(|a| a.name.clone()),
            "route": route,
            "clipsFolder": config::expand(&folder),
        })
        .to_string()
            + "\n"
    }

    /// An SF Symbol as a white PNG with alpha, for the settings page.
    fn symbol_png(&self, name: &str, px: f32) -> Vec<u8> {
        let Some(m) = self.symbols.get(name, px.clamp(8.0, 256.0)) else { return Vec::new() };
        let Some(mut p) = Pixmap::new(m.w.max(1), m.h.max(1)) else { return Vec::new() };
        for (i, a) in m.alpha.iter().enumerate() {
            p.data_mut()[i * 4..i * 4 + 4].copy_from_slice(&[*a, *a, *a, *a]);
        }
        p.encode_png().unwrap_or_default()
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

    /// OPS: tile 0 toggles WARP, hosts open an SSH session, sites open in the browser.
    fn ops_activate(&mut self, tile: usize) {
        if tile == 0 {
            return self.toggle_warp();
        }
        if let Some(h) = self.ops.hosts.get(tile - 1) {
            device::log(format!("ops: ssh {}", h.name));
            open_target(&format!("ssh://{}", h.name));
        } else if let Some(s) = self.ops.sites.get(tile - 1 - self.ops.hosts.len()) {
            open_target(&s.url);
        }
    }

    fn toggle_warp(&mut self) {
        let Some(on) = self.ops.net.warp else { return };
        // Show the new state right away; the poller confirms it a few seconds later.
        self.ops.net.warp = Some(!on);
        let _ = self.ops_cmd.send(ops::Cmd::SetWarp(!on));
        device::log(format!("ops: warp {}", if on { "off" } else { "on" }));
    }

    fn open_pr(&mut self, i: usize) {
        if let Some(p) = self.code.all().get(i) {
            open_target(&p.url);
        }
    }

    // ------------------------------------------------------------ settings page

    /// Knob turn while adjusting a slider (screen, ring, idle timeout).
    fn adjust_setting(&mut self, i: usize, d: i32) {
        match i {
            0 => {
                let cur = self.undim_to.unwrap_or(self.luminance) as i32;
                self.set_screen((cur + d * 8).clamp(render::SCREEN_MIN as i32, 255) as u8);
            }
            1 => self.set_ring_brightness((self.cfg.ring.brightness as i32 + d * 8).clamp(0, 255) as u8),
            2 => {
                let k = (render::idle_index(self.cfg.idle_seconds) as i32 + d).clamp(0, render::IDLE_STEPS.len() as i32 - 1);
                self.cfg.idle_seconds = render::IDLE_STEPS[k as usize];
                self.save_config();
            }
            _ => {}
        }
    }

    /// Touch on a slider's bar, 0.0-1.0.
    fn set_setting_frac(&mut self, i: usize, f: f32) {
        match i {
            0 => self.set_screen((render::SCREEN_MIN as f32 + f * (255 - render::SCREEN_MIN) as f32).round() as u8),
            1 => self.set_ring_brightness((f * 255.0).round() as u8),
            2 => {
                self.cfg.idle_seconds = render::IDLE_STEPS[(f * (render::IDLE_STEPS.len() - 1) as f32).round() as usize];
                self.save_config();
            }
            _ => {}
        }
    }

    fn set_screen(&mut self, v: u8) {
        self.undim_to = None;
        self.set_luminance(v);
        self.cfg.device.screen_brightness = Some(v);
        self.save_config();
    }

    fn set_ring_brightness(&mut self, v: u8) {
        self.cfg.ring.brightness = v;
        self.update_ring();
        self.save_config();
    }

    fn toggle_setting(&mut self, i: usize) {
        match i {
            3 => self.cfg.idle_home = !self.cfg.idle_home,
            4 => self.cfg.ring.enabled = !self.cfg.ring.enabled,
            5 => self.cfg.ring.alert = !self.cfg.ring.alert,
            6 => self.cfg.alerts.blink = !self.cfg.alerts.blink,
            7 => self.cfg.alerts.jump = !self.cfg.alerts.jump,
            8 => self.cfg.buzzer = !self.cfg.buzzer,
            9 => {
                self.cfg.device.mic = !self.cfg.device.mic;
                let _ = self.commands.send(Command::Mic(self.cfg.device.mic));
            }
            10 => return self.toggle_auto_accept(),
            11 => {
                self.cfg.league.snap = !self.cfg.league.snap;
                let _ = self.league_cmd.send(league::Cmd::Configure(self.cfg.league.clone()));
            }
            _ => return,
        }
        device::log(format!("settings: {} toggled", render::setting_name(i)));
        self.update_ring();
        self.save_config();
    }

    fn set_luminance(&mut self, v: u8) {
        self.luminance = v;
        let _ = self.commands.send(Command::Luminance(v));
    }

    fn back(&mut self) {
        if self.page == PageId::Stocks && self.stocks_back() {
            return;
        }
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
            PageId::Ops => 1 + self.ops.hosts.len() + self.ops.sites.len(),
            PageId::Code => self.code.mine.len() + self.code.reviews.len(),
            PageId::Settings => render::SETTING_COUNT,
            PageId::Dash => render::dash_pages(&self.cfg.pages).len(),
            PageId::Digin => self.digin.issues.len() + self.digin.inbox.len() + self.digin.pages.len(),
            // Feature pages that keep their selection in `tile` (refreshes clamp to this).
            PageId::Deploy => self.deploy.projects.len(),
            PageId::Paste => self.paste.items.len(),
            PageId::Windows => self.windows.apps.len(),
            PageId::Health => self.health.macs.len(),
            PageId::Stocks => self.stocks.quotes.len(),
            PageId::Update => self.update_rows().len(),
            _ => 0,
        }
    }

    fn knob_turn(&mut self, d: i8) {
        let d = d as i32;
        if let Some(a) = self.asks.front_mut() {
            let n = a.questions.get(a.at).map(|q| render::ask_cards(q.2.len(), q.3)).unwrap_or(1) as i32;
            a.sel = (a.sel as i32 + d).clamp(0, n - 1) as usize;
            return;
        }
        if self.call_active() {
            return self.call_turn(d);
        }
        match (self.page, &self.level) {
            (_, Level::Top) => {
                let pages = &self.pages;
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
                // The board shows the new volume at once; the music thread glides the
                // player there (absolute, so a late reading can't make them drift apart).
                let v = (self.volume.unwrap_or(50) as i32 + d * VOLUME_STEP).clamp(0, 100) as u8;
                self.volume = Some(v);
                let _ = self.music_cmd.send(MusicCmd::SetVolume(v));
            }
            (PageId::Docker, Level::Detail(_)) => {
                // Logs: newest at the bottom; turning left goes back in time.
                self.scroll = (self.scroll as i32 - d).max(0) as usize;
            }
            (PageId::Settings, Level::Detail(_)) => self.adjust_setting(self.tile, d),
            (PageId::Digin, Level::Detail(_)) => self.digin_turn(d),
            (PageId::Shortcuts | PageId::Soundboard | PageId::Docker | PageId::Ops | PageId::Code | PageId::Settings | PageId::Dash | PageId::Digin, _) => {
                let n = self.tile_count() as i32;
                if n > 0 {
                    self.tile = (self.tile as i32 + d).clamp(0, n - 1) as usize;
                }
            }
            (PageId::League, _) => {}
            (PageId::Focus, _) => self.focus_turn(d),
            (PageId::Deploy, _) => self.deploy_turn(d),
            (PageId::Paste, _) => self.paste_turn(d),
            (PageId::Windows, _) => self.windows_turn(d),
            (PageId::Calendar, _) => self.calendar_turn(d),
            (PageId::Health, _) => self.health_turn(d),
            (PageId::Stocks, _) => self.stocks_turn(d),
            (PageId::Update, _) => self.update_turn(d),
        }
    }

    fn detail_session(&self) -> Option<&Session> {
        let Level::Detail(id) = &self.level else { return None };
        self.sessions.iter().find(|s| &s.id == id)
    }

    fn knob_press(&mut self, code: u8) {
        // A question from Claude: press answers, double-click or hold hands it back.
        if !self.asks.is_empty() {
            match code {
                KNOB_PRESS => {
                    let sel = self.asks.front().map(|a| a.sel).unwrap_or(0);
                    if self.asks.front().is_some_and(|a| a.shown.elapsed() >= ASK_GUARD) {
                        self.ask_choose(sel);
                    }
                }
                KNOB_DOUBLE | KNOB_HOLD => self.finish_ask(false),
                _ => {}
            }
            return;
        }
        if self.call_active() {
            return self.call_press(code);
        }
        // A Mac waiting to pair: press allows, double-click refuses.
        if self.pair_ask.is_some() && (code == KNOB_PRESS || code == KNOB_DOUBLE) {
            return self.answer_pair(code == KNOB_PRESS);
        }
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
                (PageId::Ai, Level::Detail(_)) => {
                    // A session on another Mac can't be opened from here.
                    if self.detail_session().is_some_and(|s| s.machine.is_empty()) {
                        open_target("claude://");
                    }
                }
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
                (PageId::Ops, _) => self.ops_activate(self.tile),
                (PageId::Code, _) => self.open_pr(self.tile),
                (PageId::Digin, _) => self.digin_press(),
                (PageId::Dash, _) => {
                    if let Some(p) = render::dash_pages(&self.cfg.pages).get(self.tile).copied() {
                        self.set_page(p);
                    }
                }
                (PageId::Focus, _) => self.focus_press(),
                (PageId::Deploy, _) => self.deploy_press(),
                (PageId::Paste, _) => self.paste_press(),
                (PageId::Windows, _) => self.windows_press(),
                (PageId::Calendar, _) => self.calendar_press(),
                (PageId::Health, _) => self.health_press(),
                (PageId::Stocks, _) => self.stocks_press(),
                (PageId::Update, _) => self.update_press(),
                (PageId::Settings, Level::Detail(_)) => self.level = Level::Focus,
                (PageId::Settings, _) => {
                    if self.tile < render::SETTING_SLIDERS {
                        self.level = Level::Detail("adjust".into());
                    } else {
                        self.toggle_setting(self.tile);
                    }
                }
            },
            KNOB_DOUBLE => self.back(),
            KNOB_HOLD => {
                if self.feature_hold() {
                    return;
                }
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

    /// Shuffle on or off; the board shows it at once, the next reading confirms it.
    fn toggle_shuffle(&mut self) {
        let Some(t) = &mut self.track else { return };
        t.shuffle = !t.shuffle;
        let _ = self.music_cmd.send(MusicCmd::Shuffle);
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
                "shuffle" => self.toggle_shuffle(),
                _ => self.play_pause(),
            },
            Action::Clip { title } => {
                let _ = self.audio.cmd.send(audio::Cmd::PlayTitle(title.clone()));
            }
            Action::Page { page } => {
                if self.pages.contains(page) {
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
            Hit::OpenClaude => {
                if self.detail_session().is_some_and(|s| s.machine.is_empty()) {
                    open_target("claude://");
                }
            }
            Hit::Prev => {
                let _ = self.music_cmd.send(MusicCmd::Previous);
            }
            Hit::PlayPause => self.play_pause(),
            Hit::Next => {
                let _ = self.music_cmd.send(MusicCmd::Next);
            }
            Hit::Shuffle => self.toggle_shuffle(),
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
            Hit::Warp => {
                self.tile = 0;
                self.toggle_warp();
            }
            Hit::Host(i) => {
                self.tile = 1 + i;
                self.ops_activate(self.tile);
            }
            Hit::Site(i) => {
                self.tile = 1 + self.ops.hosts.len() + i;
                self.ops_activate(self.tile);
            }
            Hit::Pr(i) => {
                self.tile = i;
                self.open_pr(i);
            }
            Hit::Pane(p) => self.set_page(p),
            Hit::Feature(p, i) => self.feature_tap(p, i),
            Hit::DiginItem(i) => self.digin_tap(i),
            Hit::AskCard(i) => {
                if let Some(a) = self.asks.front_mut() {
                    a.sel = i;
                }
                if self.asks.front().is_some_and(|a| a.shown.elapsed() >= ASK_GUARD) {
                    self.ask_choose(i);
                }
            }
            Hit::Setting(i, frac) => {
                self.tile = i;
                if i < render::SETTING_SLIDERS {
                    // Bar: set it there. Elsewhere on the card: the knob now adjusts it.
                    match frac {
                        Some(f) => self.set_setting_frac(i, f),
                        None => self.level = Level::Detail("adjust".into()),
                    }
                } else {
                    self.toggle_setting(i);
                }
            }
        }
    }

    fn on_device(&mut self, input: Input) {
        // The night screen wakes on any input and eats it.
        if matches!(input, Input::Tap { .. } | Input::KnobTurn(_) | Input::KnobPress(_)) && self.night_swallow() {
            self.last_input = Instant::now();
            self.redraw();
            return;
        }
        if matches!(input, Input::Tap { .. } | Input::KnobTurn(_) | Input::KnobPress(_)) {
            self.last_input = Instant::now();
            self.clear_attention();
        }
        // Presses in the log (like taps), to tell a quick press-press from a double-click.
        if let Input::KnobPress(code @ (KNOB_PRESS | KNOB_DOUBLE | KNOB_HOLD)) = input {
            let what = match code {
                KNOB_PRESS => "press",
                KNOB_DOUBLE => "double-click",
                _ => "hold",
            };
            let level = match &self.level {
                Level::Detail(d) => format!("detail {d}"),
                l => format!("{l:?}").to_lowercase(),
            };
            device::log(format!("knob: {what} on {} ({level})", self.page.title()));
        }
        match input {
            Input::Connected { luminance, firmware } => {
                device::log(format!("device: Quake firmware {}", firmware.as_deref().unwrap_or("?")));
                self.connected = true;
                self.sync_want.store(true, Ordering::Relaxed);
                self.ring_sent = None;
                let _ = self.commands.send(Command::Mic(self.cfg.device.mic));
                if let Some(b) = self.cfg.device.screen_brightness {
                    self.luminance = b.max(render::SCREEN_MIN);
                    let _ = self.commands.send(Command::Luminance(self.luminance));
                } else if let Some(l) = luminance {
                    self.luminance = l;
                }
                self.update_ring();
            }
            Input::Disconnected => {
                self.connected = false;
                self.sync_want.store(false, Ordering::Relaxed);
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
            "build": env!("QUAKED_BUILD"),
            "connected": self.connected,
            "page": self.page,
            "level": self.level_name(),
            "sessions": self.sessions.len(),
            "waiting": count(State::Waiting),
            "working": count(State::Working),
            "track": self.track.as_ref().map(|t| serde_json::json!({"title": t.title, "artist": t.artist, "playing": t.playing, "shuffle": t.shuffle})),
            "league": {"running": self.league.running, "phase": self.league.phase, "autoAccept": self.cfg.league.auto_accept},
            "docker": {"ok": self.docker.ok, "engine": self.docker.engine, "running": self.docker.containers.iter().filter(|c| c.state == "running").count(), "containers": self.docker.containers.len()},
            "ops": {
                "warp": self.ops.net.warp,
                "appgate": self.ops.net.appgate,
                "colo": self.ops.net.colo,
                "hosts": self.ops.hosts.iter().map(|h| serde_json::json!({"name": h.name, "up": h.up, "ms": h.latency_ms, "load": h.load, "reason": h.reason, "statsError": h.stats_error})).collect::<Vec<_>>(),
                "sites": self.ops.sites.iter().map(|s| serde_json::json!({"url": s.url, "code": s.code, "ms": s.ms, "tlsDays": s.tls_days, "error": s.error})).collect::<Vec<_>>(),
            },
            "sync": {
                "enabled": self.cfg.sync.enabled,
                "name": self.peers.name,
                "port": self.peers.port,
                "peers": self.peers.peers.iter().map(|p| serde_json::json!({"name": p.name, "host": p.host, "ok": p.ok, "error": p.error, "sessions": p.sessions.len()})).collect::<Vec<_>>(),
            },
            "features": self.features_json(),
            "digin": {
                "accounts": self.digin.accounts.iter().map(|a| serde_json::json!({"url": a.url, "label": a.label, "email": a.email, "signedIn": a.signed_in, "handle": a.handle, "name": a.name, "error": a.error})).collect::<Vec<_>>(),
                "issues": self.digin.issues.len(),
                "inbox": self.digin.inbox.len(),
                "chatUnread": self.digin.chat_unread,
                "chatMentions": self.digin.chat_mentions,
            },
            "code": {
                "ok": self.code.ok,
                "gitlabOk": self.code.gitlab_ok,
                "gitlabError": self.code.gitlab_error,
                "login": self.code.login,
                "open": self.code.mine.len(),
                "failing": self.code.mine.iter().filter(|p| matches!(p.ci.as_str(), "FAILURE" | "ERROR")).count(),
                "reviewRequests": self.code.reviews.len(),
                "error": self.code.error,
            },
            "soundboard": {
                "routing": self.sounds.live,
                "listener": self.sounds.listener,
                "error": self.sounds.error,
                "playing": playing,
                "micMuted": self.sounds.mic_muted,
                "clips": self.sounds.clips.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
                "clipFiles": self.sounds.clips.iter().map(|c| serde_json::json!({
                    "title": c.title,
                    "file": c.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
                    "color": c.color, "symbol": c.symbol, "gain": c.gain,
                })).collect::<Vec<_>>(),
                "live": self.cfg.soundboard.live,
            },
        })
        .to_string()
    }

    fn on_ctl(&mut self, line: &str) -> Vec<u8> {
        if let Some(json) = line.strip_prefix("config-put ") {
            return self.config_put(json);
        }
        if let Some(reply) = self.update_ctl(line.trim()) {
            return reply;
        }
        // From a paired Mac: join a meeting, or show an event, here.
        if let Some(url) = line.strip_prefix("join ") {
            return self.calendar_join_from_peer(url.trim());
        }
        if let Some(id) = line.strip_prefix("show-event ") {
            return self.calendar_show_from_peer(id.trim());
        }
        if let Some(key) = line.strip_prefix("set-key ") {
            self.cfg.sync.key = key.trim().to_string();
            self.save_config();
            let _ = self.sync_cmd.send(sync::Cmd::Configure(self.cfg.sync.clone()));
            return b"ok\n".to_vec();
        }
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
            ["state"] => self.state_json().into_bytes(),
            ["health"] => (self.health_peer_json() + "\n").into_bytes(),
            ["calendar"] => (crate::calendar::peer_json(&self.calendar, self.cfg.calendar.enabled) + "\n").into_bytes(),
            ["config"] => (serde_json::to_string(&self.cfg).unwrap_or_default() + "\n").into_bytes(),
            ["pair-answer", a] => {
                self.answer_pair(*a == "yes");
                ok
            }
            ["symbol", name, px] => self.symbol_png(name, px.parse().unwrap_or(32.0)),
            ["frame"] => self.render(render::W as u32, render::H as u32).pixmap.encode_png().unwrap_or_default(),
            // Renders another page at its top level without switching the board to it.
            ["frame", p] => match serde_json::from_value::<PageId>(serde_json::Value::String(p.to_string())) {
                Ok(page) => {
                    let (clock, second) = clock();
                    let mut canvas = Canvas::new(render::W as u32, render::H as u32, &self.fonts, &self.symbols);
                    let mut view = self.view(&clock, status::now_ms(), second);
                    let top = Level::Top;
                    if page != self.page {
                        view.page = page;
                        view.level = &top;
                        view.tile = 0;
                        view.accent = config::hex(self.cfg.ring.color_for(page), 0xD97757);
                    }
                    render::draw(&mut canvas, &view);
                    canvas.pixmap.encode_png().unwrap_or_default()
                }
                Err(_) => format!("error: no page {p}\n").into_bytes(),
            },
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
            // Try the "new stuff" alert: quaked ctl alert code "review requested"
            ["alert", p, ..] => match serde_json::from_value::<PageId>(serde_json::Value::String(p.to_string())) {
                Ok(page) => {
                    let why = if parts.len() > 2 { parts[2..].join(" ") } else { "test alert".into() };
                    self.alert(page, why, true);
                    ok
                }
                Err(_) => format!("error: no page {p}\n").into_bytes(),
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
        self.refresh_menubar();
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
                // Loading into a game: show it (unless you're using the board).
                let started = !matches!(self.league.phase.as_str(), "GameStart" | "InProgress") && matches!(st.phase.as_str(), "GameStart" | "InProgress");
                self.league = st;
                if started && self.cfg.league.snap && self.cfg.pages.contains(&PageId::League) && self.may_jump(PageId::League, Urgency::High) {
                    if self.last_input.elapsed() > JUMP_QUIET {
                        self.jump_for(PageId::League, Urgency::High);
                    }
                }
                self.update_ring();
                if self.page == PageId::League || self.page == PageId::Dash {
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
                let news = if self.docker.ok && st.ok { docker_news(&self.docker, &st) } else { Vec::new() };
                self.docker = st;
                let ids: Vec<String> = self.docker.containers.iter().map(|c| c.id.clone()).collect();
                self.history.docker_cpu.retain(|id, _| ids.contains(id));
                for ct in self.docker.containers.iter().filter(|c| c.state == "running") {
                    render::History::push(self.history.docker_cpu.entry(ct.id.clone()).or_default(), ct.cpu.unwrap_or(0.0));
                }
                if let Some(r) = Self::summarize(news) {
                    self.alert_from(PageId::Docker, r, true, Urgency::of(PageId::Docker), Some(""));
                }
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
            Event::Ops(st) => {
                let news = ops_news(&self.ops, &st);
                // Dashboard: a response time per site check (0 = it failed).
                for site in &st.sites {
                    let changed = self.ops.sites.iter().find(|s| s.url == site.url).is_none_or(|s| s.ms != site.ms || s.code != site.code);
                    if changed {
                        let ms = if site.code.is_some_and(|c| c < 400) { site.ms.unwrap_or(0) as f32 } else { 0.0 };
                        render::History::push(self.history.site_ms.entry(site.url.clone()).or_default(), ms);
                    }
                }
                self.ops = st;
                if let Some(r) = Self::summarize(news) {
                    self.alert(PageId::Ops, r, true);
                }
                self.tile = self.tile.min(self.tile_count().saturating_sub(1));
                if self.page == PageId::Ops {
                    self.redraw();
                }
            }
            Event::Code(st) => {
                let news = code_news(&self.code, &st);
                self.code = st;
                if let Some(r) = Self::summarize(news) {
                    self.alert(PageId::Code, r, true);
                }
                self.tile = self.tile.min(self.tile_count().saturating_sub(1));
                if self.page == PageId::Code {
                    self.redraw();
                }
            }
            Event::Sync(st) => {
                self.peers = st;
                self.health_peers_changed();
                self.calendar_peers_changed();
                self.refresh_pages();
                self.poll_status();
                self.redraw();
            }
            Event::PeerConfig(from, json) => match serde_json::from_str::<Config>(&json) {
                Ok(mut c) if c.updated_at > self.cfg.updated_at => {
                    c.sync = self.cfg.sync.clone(); // the key and name stay per Mac
                    device::log(format!("settings: took the newer settings from {from}"));
                    self.adopt(c, false);
                    self.write_config();
                }
                Ok(_) => {}
                Err(e) => device::log(format!("settings from {from}: {e}")),
            },
            Event::PairRequest(name, reply) => {
                if self.pair_ask.is_some() {
                    let _ = reply.send(false);
                } else {
                    self.pair_ask = Some(PairAsk { name, reply, until: Instant::now() + Duration::from_secs(60) });
                    self.show_pair_ask();
                }
            }
            Event::Menu(action) => self.on_menu(action),
            Event::Feature(f) => self.on_feature(f),
            Event::Digin(mut st) => {
                // The open item is the app's; the poller only answers about it.
                let reply = st.reply.take();
                st.detail = self.digin.detail.take();
                // New in the inbox since last time (not on the first load or after signing in).
                let known: std::collections::HashSet<(usize, i64)> = self.digin.inbox.iter().map(|n| (n.account, n.id)).collect();
                let was_in: Vec<bool> = self.digin.accounts.iter().map(|a| a.signed_in && a.error.is_none()).collect();
                let news: Vec<String> = st
                    .inbox
                    .iter()
                    .filter(|n| was_in.get(n.account).copied().unwrap_or(false) && !known.contains(&(n.account, n.id)))
                    .filter(|n| matches!(n.kind.as_str(), "mentioned" | "message" | "assigned" | "commented" | "due"))
                    .map(|n| if n.body.is_empty() { n.title.clone() } else { format!("{}: {}", n.title, n.body) })
                    .collect();
                self.digin = st;
                if let Some(r) = reply {
                    self.digin_reply(r);
                }
                self.tile = self.tile.min(self.tile_count().saturating_sub(1));
                if let Some(r) = Self::summarize(news) {
                    self.alert(PageId::Digin, r, true);
                }
                if self.page == PageId::Digin || self.page == PageId::Dash {
                    self.redraw();
                }
            }
            Event::DiginAdded(acct) => {
                match self.cfg.digin.accounts.iter_mut().find(|a| digin::normalize(&a.url) == digin::normalize(&acct.url)) {
                    Some(a) => a.email = acct.email,
                    None => self.cfg.digin.accounts.push(acct),
                }
                self.save_config();
                let _ = self.digin_cmd.send(digin::Cmd::Configure(self.cfg.digin.clone()));
            }
            Event::DiginRefresh => {
                let _ = self.digin_cmd.send(digin::Cmd::Refresh);
            }
            Event::Ctl(line, reply) if line.starts_with("ask ") => self.start_ask(&line[4..], reply),
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
        let idle_for = Duration::from_secs(self.cfg.idle_seconds.max(5));
        self.relevance_tick();
        // Reading a chart isn't being idle: an opened stock stays up for 10 minutes.
        let reading = self.page == PageId::Stocks && matches!(self.level, Level::Detail(_));
        let idle_for = if reading { idle_for.max(Duration::from_secs(600)) } else { idle_for };
        if self.asks.is_empty() && self.last_input.elapsed() > idle_for && self.page_since.elapsed() > idle_for {
            // Stay on League while a match is being accepted or picked.
            let league_busy = self.page == PageId::League && relevance::in_match(&self.league.phase);
            let home = if self.cfg.idle_home && !league_busy { self.idle_page() } else { None };
            if let Some(page) = home.filter(|p| *p != self.page) {
                self.set_page(page);
                self.redraw();
            } else if self.level != Level::Top {
                self.level = Level::Top;
                self.scroll = 0;
                self.redraw();
            }
        }
        let (_, second) = clock();
        if second != self.drawn_second && (self.animating() || second % 60 == 0) {
            self.redraw();
        }
        let mut wake = self.next_poll.min(self.next_display);
        if self.page == PageId::Docker && matches!(self.level, Level::Detail(_)) {
            wake = wake.min(self.next_logs);
        }
        if self.asks.front().is_some_and(|a| now >= a.until) {
            device::log("ask: no answer in time; back to Claude");
            self.finish_ask(false);
        }
        if let Some(a) = &self.pair_ask {
            if now >= a.until {
                self.answer_pair(false);
            } else if self.attention.is_none() {
                self.show_pair_ask();
            }
        }
        if let Some(a) = &self.attention {
            if now >= a.until {
                self.clear_attention();
            } else if self.cfg.alerts.blink && self.cfg.ring.enabled {
                if now >= self.next_blink {
                    self.blink_on = !self.blink_on;
                    self.next_blink = now + BLINK;
                    self.update_ring();
                }
                wake = wake.min(self.next_blink);
            }
        }
        if let Some(t) = self.feature_tick(now) {
            wake = wake.min(t);
        }
        if let Some((_, at)) = self.flash {
            if at.elapsed() >= FLASH {
                self.flash = None;
                self.redraw();
            } else {
                wake = wake.min(at + FLASH);
            }
        }
        self.refresh_menubar();
        el.set_control_flow(ControlFlow::WaitUntil(wake));
    }
}

/// How far along a session is, ignoring "needs your permission" notices: steps taken,
/// tools still running, turn finished.
/// How far along a session is, for telling when a prompt on the Quake was answered on the
/// Mac. A next-step question comes after the turn ends, so only something new you send
/// (your latest prompt's time) counts; anything else counts any progress.
fn ask_mark(next: bool, s: &Session) -> (u32, usize, bool) {
    if next {
        let last_prompt = s.steps.iter().rev().find(|st| st.tag == "you").map(|st| st.ts / 1000).unwrap_or(0);
        (last_prompt as u32, 0, false)
    } else {
        progress(s)
    }
}

fn progress(s: &Session) -> (u32, usize, bool) {
    (s.step_count, s.steps.iter().filter(|st| st.status == status::StepStatus::Running).count(), matches!(s.state, State::Done | State::Idle))
}

/// Same settings apart from when they were saved.
fn same_settings(a: &Config, b: &Config) -> bool {
    let (mut a, mut b) = (a.clone(), b.clone());
    a.updated_at = 0;
    b.updated_at = 0;
    a == b
}

// ---------------------------------------------------------------- what's new

fn pr_label(p: &github::Pr) -> String {
    let repo = p.repo.rsplit('/').next().unwrap_or(&p.repo);
    format!("{repo} {}{}", if p.gitlab { '!' } else { '#' }, p.number)
}

/// New review requests, and your PRs turning red, getting a review, or conflicting.
/// A source only counts once it had answered before, so signing in isn't news.
fn code_news(old: &github::Status, new: &github::Status) -> Vec<String> {
    let comparable = |p: &github::Pr| if p.gitlab { old.gitlab_ok } else { old.ok };
    let mut news = Vec::new();
    for p in new.reviews.iter().filter(|p| comparable(p)) {
        if !old.reviews.iter().any(|o| o.url == p.url) {
            news.push(format!("review requested  ·  {}  {}", pr_label(p), p.title));
        }
    }
    for p in new.mine.iter().filter(|p| comparable(p)) {
        let Some(o) = old.mine.iter().find(|o| o.url == p.url) else { continue };
        let red = |ci: &str| matches!(ci, "FAILURE" | "ERROR");
        if red(&p.ci) && !red(&o.ci) {
            news.push(format!("CI failed  ·  {}", pr_label(p)));
        }
        if p.review != o.review {
            match p.review.as_str() {
                "APPROVED" => news.push(format!("approved  ·  {}", pr_label(p))),
                "CHANGES_REQUESTED" => news.push(format!("changes requested  ·  {}", pr_label(p))),
                _ => {}
            }
        }
        if p.mergeable == "CONFLICTING" && o.mergeable != "CONFLICTING" {
            news.push(format!("merge conflict  ·  {}", pr_label(p)));
        }
    }
    news
}

/// Hosts going down and sites starting to fail (not while this Mac is offline).
fn ops_news(old: &ops::Status, new: &ops::Status) -> Vec<String> {
    if new.net.ip.is_empty() || old.net.ip.is_empty() {
        return Vec::new();
    }
    let mut news = Vec::new();
    for h in &new.hosts {
        if !h.up && old.hosts.iter().any(|o| o.name == h.name && o.up) {
            news.push(format!("{} is down", h.name));
        }
    }
    let ok = |s: &ops::Site| s.code.is_some_and(|c| c < 400);
    for s in &new.sites {
        if !ok(s) && old.sites.iter().any(|o| o.url == s.url && ok(o)) {
            let why = s.code.map(|c| c.to_string()).or(s.error.clone()).unwrap_or_default();
            news.push(format!("{} is failing  ·  {why}", s.url.trim_start_matches("https://").trim_start_matches("http://")));
        }
    }
    news
}

/// Running containers that stopped or started restarting.
fn docker_news(old: &docker::Status, new: &docker::Status) -> Vec<String> {
    new.containers
        .iter()
        .filter_map(|c| {
            let o = old.containers.iter().find(|o| o.id == c.id)?;
            (o.state == "running" && c.state != "running" && c.state != "paused").then(|| format!("{} {}", c.name, if c.state == "restarting" { "is restarting" } else { "stopped" }))
        })
        .collect()
}

pub fn ctl_socket_path() -> std::path::PathBuf {
    let dir = status::dir();
    dir.parent().unwrap_or(&dir).join("ctl.sock")
}

/// One request per connection: a line in, a reply out, then close.
/// Runs one control command (the `quaked ctl` lines) against the board, from any thread.
pub type Ctl = Arc<dyn Fn(&str) -> Vec<u8> + Send + Sync>;

fn ctl_caller(proxy: winit::event_loop::EventLoopProxy<Event>, audio: Sender<audio::Cmd>) -> Ctl {
    let proxy = Mutex::new(proxy);
    let audio = Mutex::new(audio);
    Arc::new(move |line: &str| {
        // Digin sign-in talks to the server: do it here, not on the main thread.
        if let Some(json) = line.strip_prefix("digin-auth ") {
            let (reply, added) = crate::digin::auth(json);
            let proxy = proxy.lock().unwrap();
            if let Some(a) = added {
                let _ = proxy.send_event(Event::DiginAdded(a));
            } else if json.contains("\"sign-out\"") {
                let _ = proxy.send_event(Event::DiginRefresh);
            }
            return (reply.to_string() + "\n").into_bytes();
        }
        if line == "audio-devices" {
            let (tx, rx) = std::sync::mpsc::channel();
            let _ = audio.lock().unwrap().send(audio::Cmd::Devices(tx));
            // The first enumeration can take seconds while devices wake up.
            return match rx.recv_timeout(Duration::from_secs(20)) {
                Ok((inputs, outputs)) => (serde_json::json!({"inputs": inputs, "outputs": outputs}).to_string() + "\n").into_bytes(),
                Err(_) => b"error: audio engine busy\n".to_vec(),
            };
        }
        let (tx, rx) = std::sync::mpsc::channel();
        if proxy.lock().unwrap().send_event(Event::Ctl(line.to_string(), tx)).is_err() {
            return b"error: quaked is stopping\n".to_vec();
        }
        let wait = if line.starts_with("ask ") { Duration::from_secs(600) } else { Duration::from_secs(5) };
        rx.recv_timeout(wait).unwrap_or_else(|_| b"error: timeout\n".to_vec())
    })
}

fn ctl_thread(ctl: Ctl) {
    let path = ctl_socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = match std::os::unix::net::UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => return device::log(format!("ctl: cannot listen on {}: {e}", path.display())),
    };
    for conn in listener.incoming().flatten() {
        let ctl = ctl.clone();
        std::thread::spawn(move || {
            let _ = conn.set_read_timeout(Some(Duration::from_secs(25)));
            let mut reader = BufReader::new(&conn);
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                return;
            }
            let reply = ctl(line.trim());
            let mut w = &conn;
            let _ = w.write_all(&reply);
        });
    }
}

pub fn run() {
    let mut cfg = config::load();
    if cfg.sync.key.is_empty() {
        // First run: make the pairing key the other Mac will need.
        cfg.sync.key = sync::new_key();
        if config::save(&cfg) {
            device::log("sync: created a pairing key");
        }
    }
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
    // Features: each starts its own background work and reports through Event::Feature.
    let pf = proxy.clone();
    let call_cmd = crate::call::spawn(cfg.call.clone(), move |u| {
        let _ = pf.send_event(Event::Feature(FeatureEvent::Call(u)));
    });
    let pf = proxy.clone();
    let focus_cmd = crate::focus::spawn(cfg.focus.clone(), move |u| {
        let _ = pf.send_event(Event::Feature(FeatureEvent::Focus(u)));
    });
    let pf = proxy.clone();
    let deploy_cmd = crate::deploy::spawn(cfg.deploy.clone(), move |u| {
        let _ = pf.send_event(Event::Feature(FeatureEvent::Deploy(u)));
    });
    let pf = proxy.clone();
    let paste_cmd = crate::clipboard::spawn(cfg.paste.clone(), move |u| {
        let _ = pf.send_event(Event::Feature(FeatureEvent::Paste(u)));
    });
    let pf = proxy.clone();
    let windows_cmd = crate::windows::spawn(cfg.windows.clone(), move |u| {
        let _ = pf.send_event(Event::Feature(FeatureEvent::Windows(u)));
    });
    let pf = proxy.clone();
    let calendar_cmd = crate::calendar::spawn(cfg.calendar.clone(), move |u| {
        let _ = pf.send_event(Event::Feature(FeatureEvent::Calendar(u)));
    });
    let pf = proxy.clone();
    let health_cmd = crate::health::spawn(cfg.health.clone(), move |u| {
        let _ = pf.send_event(Event::Feature(FeatureEvent::Health(u)));
    });
    let pf = proxy.clone();
    let stocks_cmd = crate::stocks::spawn(cfg.stocks.clone(), move |u| {
        let _ = pf.send_event(Event::Feature(FeatureEvent::Stocks(u)));
    });
    let pf = proxy.clone();
    let update_cmd = crate::update::spawn(cfg.updates.clone(), move |u| {
        let _ = pf.send_event(Event::Feature(FeatureEvent::Update(u)));
    });
    let pf = proxy.clone();
    let night_cmd = crate::night::spawn(cfg.night.clone(), move |u| {
        let _ = pf.send_event(Event::Feature(FeatureEvent::Night(u)));
    });
    let pf = proxy.clone();
    let viz_cmd = crate::viz::spawn(cfg.viz.clone(), move |u| {
        let _ = pf.send_event(Event::Feature(FeatureEvent::Viz(u)));
    });
    let digin_fast = Arc::new(AtomicBool::new(false));
    let p11 = proxy.clone();
    let digin_cmd = digin::spawn(cfg.digin.clone(), digin_fast.clone(), move |st| {
        let _ = p11.send_event(Event::Digin(st));
    });
    let ops_fast = Arc::new(AtomicBool::new(false));
    let p7 = proxy.clone();
    let ops_cmd = ops::spawn(cfg.ops.clone(), ops_fast.clone(), move |st| {
        let _ = p7.send_event(Event::Ops(st));
    });
    let code_fast = Arc::new(AtomicBool::new(false));
    let p8 = proxy.clone();
    let code_cmd = github::spawn(cfg.code.clone(), code_fast.clone(), move |st| {
        let _ = p8.send_event(Event::Code(st));
    });
    let ctl = ctl_caller(proxy.clone(), audio.cmd.clone());
    let sync_want = Arc::new(AtomicBool::new(false));
    let config_at = Arc::new(AtomicU64::new(cfg.updated_at));
    let p10 = Mutex::new(proxy.clone());
    let pair: Arc<dyn Fn(&str) -> bool + Send + Sync> = Arc::new(move |who: &str| {
        let (tx, rx) = std::sync::mpsc::channel();
        if p10.lock().unwrap().send_event(Event::PairRequest(who.to_string(), tx)).is_err() {
            return false;
        }
        rx.recv_timeout(Duration::from_secs(65)).unwrap_or(false)
    });
    let hooks = sync::Hooks { ctl: ctl.clone(), pair, board: sync_want.clone(), config_at: config_at.clone() };
    let p9 = proxy.clone();
    let sync_cmd = sync::spawn(cfg.sync.clone(), hooks, move |out| {
        let _ = p9.send_event(match out {
            sync::Out::Status(st) => Event::Sync(st),
            sync::Out::Config { from, json } => Event::PeerConfig(from, json),
        });
    });
    let ctl_socket = ctl.clone();
    std::thread::Builder::new().name("ctl".into()).spawn(move || ctl_thread(ctl_socket)).unwrap();
    crate::web::spawn(ctl.clone());
    let first_page = cfg.pages.first().copied().unwrap_or(PageId::Ai);
    let first_pages = cfg.pages.clone();
    // After an update this build installed: say how it went in the top bar.
    let just_updated = crate::update::just_updated().map(|r| Attention {
        page: first_page,
        reason: match r {
            Ok(v) => format!("Updated to quaked {v}"),
            Err(v) => format!("The update to quaked {v} didn't take: this is still {}", crate::update::CURRENT),
        },
        until: Instant::now() + Duration::from_secs(cfg.alerts.seconds.max(30)),
        urgency: Urgency::Low,
        machine: None,
    });
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
        ops_cmd,
        ops: ops::Status::default(),
        ops_fast,
        code_cmd,
        code: github::Status::default(),
        code_fast,
        digin_cmd,
        digin: digin::Status::default(),
        digin_fast,
        sync_cmd,
        peers: sync::Status::default(),
        sync_want,
        config_at,
        pair_ask: None,
        history: render::History::default(),
        ai_total: None,
        next_ai_sample: Instant::now(),
        menubar: None,
        proxy: proxy.clone(),
        asks: std::collections::VecDeque::new(),
        call: crate::call::Status::default(),
        call_cmd,
        focus: crate::focus::Status::default(),
        focus_cmd,
        deploy: crate::deploy::Status::default(),
        deploy_cmd,
        paste: crate::clipboard::Status::default(),
        paste_cmd,
        windows: crate::windows::Status::default(),
        windows_cmd,
        calendar: crate::calendar::Status::default(),
        calendar_cmd,
        health: crate::health::Status::default(),
        health_cmd,
        stocks: crate::stocks::Status::default(),
        stocks_cmd,
        update: crate::update::Status::default(),
        update_cmd,
        update_peers: HashMap::new(),
        update_errors: HashMap::new(),
        update_armed: None,
        focus_spin: Default::default(),
        update_announced: String::new(),
        pages: first_pages,
        night: crate::night::Status::default(),
        night_cmd,
        viz: crate::viz::Status::default(),
        viz_cmd,
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
        page_since: Instant::now(),
        connected: false,
        ring_sent: None,
        luminance: 255,
        undim_to: None,
        cursor: (0.0, 0.0),
        flash: None,
        attention: just_updated,
        jumped: None,
        pending_jump: None,
        health_flags: Default::default(),
        waiting_alert: None,
        blink_on: false,
        next_blink: Instant::now(),
        next_poll: Instant::now(),
        next_display: Instant::now() + DISPLAY_POLL,
        drawn_second: 0,
    };
    event_loop.run_app(&mut app).expect("event loop run");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(url: &str, ci: &str, review: &str) -> github::Pr {
        github::Pr { repo: "o/r".into(), number: 1, url: url.into(), ci: ci.into(), review: review.into(), mergeable: "MERGEABLE".into(), ..Default::default() }
    }

    #[test]
    fn code_news_spots_new_reviews_and_red_ci() {
        let old = github::Status { ok: true, mine: vec![pr("a", "PENDING", "")], reviews: vec![], ..Default::default() };
        let new = github::Status { ok: true, mine: vec![pr("a", "FAILURE", "APPROVED")], reviews: vec![pr("b", "", "")], ..Default::default() };
        let news = code_news(&old, &new);
        assert_eq!(news.len(), 3, "{news:?}");
        // The first answer after startup (or after signing in) is not news.
        assert!(code_news(&github::Status::default(), &new).is_empty());
        let gl = github::Pr { gitlab: true, ..pr("c", "", "") };
        let signed_in = github::Status { gitlab_ok: true, reviews: vec![gl], ..new.clone() };
        assert_eq!(code_news(&new, &signed_in).len(), 0);
    }

    #[test]
    fn ops_and_docker_news() {
        let host = |up| ops::Host { name: "workbench".into(), up, ..Default::default() };
        let net = ops::Network { ip: "1.2.3.4".into(), ..Default::default() };
        let old = ops::Status { net: net.clone(), hosts: vec![host(true)], sites: vec![ops::Site { url: "https://x.dev".into(), code: Some(200), ..Default::default() }] };
        let new = ops::Status { net, hosts: vec![host(false)], sites: vec![ops::Site { url: "https://x.dev".into(), code: Some(502), ..Default::default() }] };
        assert_eq!(ops_news(&old, &new), vec!["workbench is down".to_string(), "x.dev is failing  ·  502".to_string()]);
        let offline = ops::Status { net: ops::Network::default(), ..new.clone() };
        assert!(ops_news(&old, &offline).is_empty());

        let ct = |state: &str| docker::Container { id: "1".into(), name: "db".into(), state: state.into(), ..Default::default() };
        let d = |state| docker::Status { ok: true, containers: vec![ct(state)], ..Default::default() };
        assert_eq!(docker_news(&d("running"), &d("exited")), vec!["db stopped".to_string()]);
        assert!(docker_news(&d("exited"), &d("running")).is_empty());
    }
}
