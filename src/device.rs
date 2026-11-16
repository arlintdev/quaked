//! Background threads that own the Quake's HID interfaces. Both reconnect on their own
//! when the device is unplugged or the Mac sleeps.

use crate::proto::{self, Message, TouchPoint};
use hidapi::{HidApi, HidDevice};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

const QUAKE_VID: u16 = 0x4158;
const QUAKE_PID: u16 = 0x514B;
const QUAKE_USAGE_PAGE: u16 = 0xFF60;
const QUAKE_USAGE: u16 = 0x61;
const TOUCH_VID: u16 = 0x0712;
const TOUCH_PID: u16 = 0x0010;
const TOUCH_USAGE_PAGE: u16 = 0xFF73;
const TOUCH_USAGE: u16 = 0x71;

const PING_EVERY: Duration = Duration::from_secs(5);
const RETRY_EVERY: Duration = Duration::from_secs(3);

/// What the device threads report to the UI.
#[derive(Debug, Clone)]
pub enum Input {
    Connected { firmware: Option<String>, luminance: Option<u8> },
    Disconnected,
    KnobTurn(i8),
    KnobPress(u8),
    Tap { x: f64, y: f64 },
}

/// What the UI asks the control thread to do.
#[derive(Debug, Clone)]
pub enum Command {
    /// (level, milliseconds) steps; level 0 is silence.
    Tone(Vec<(u8, u64)>),
    Luminance(u8),
}

pub fn log(msg: impl AsRef<str>) {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    eprintln!("[{}.{:03}] {}", t.as_secs(), t.subsec_millis(), msg.as_ref());
}

fn open(api: &mut HidApi, vid: u16, pid: u16, page: u16, usage: u16) -> Option<HidDevice> {
    if let Err(e) = api.refresh_devices() {
        log(format!("hid: refresh failed: {e}"));
    }
    let info = api
        .device_list()
        .find(|d| d.vendor_id() == vid && d.product_id() == pid && d.usage_page() == page && d.usage() == usage)?;
    // Log each distinct open failure once, not on every retry.
    static LAST: std::sync::Mutex<Vec<(u16, String)>> = std::sync::Mutex::new(Vec::new());
    let result = info.open_device(api);
    let mut last = LAST.lock().unwrap();
    match result {
        Ok(d) => {
            last.retain(|(p, _)| *p != pid);
            Some(d)
        }
        Err(e) => {
            let msg = e.to_string();
            if !last.iter().any(|(p, m)| *p == pid && *m == msg) {
                log(format!("hid: open {vid:04x}:{pid:04x} failed: {msg}"));
                last.retain(|(p, _)| *p != pid);
                last.push((pid, msg));
            }
            None
        }
    }
}

fn new_api() -> HidApi {
    loop {
        match HidApi::new() {
            Ok(api) => {
                // Share the devices instead of seizing them.
                api.set_open_exclusive(false);
                return api;
            }
            Err(e) => {
                log(format!("hid: init failed: {e}"));
                std::thread::sleep(RETRY_EVERY);
            }
        }
    }
}

/// Owns the control interface: keepalive pings, buzzer and luminance commands, knob events.
pub fn control_thread(commands: Receiver<Command>, emit: impl Fn(Input) + Send + 'static) {
    let mut api = new_api();
    let mut was_connected = false;
    loop {
        let Some(dev) = open(&mut api, QUAKE_VID, QUAKE_PID, QUAKE_USAGE_PAGE, QUAKE_USAGE) else {
            if was_connected {
                emit(Input::Disconnected);
                was_connected = false;
            }
            // Drop commands queued while the device was away.
            while commands.try_recv().is_ok() {}
            std::thread::sleep(RETRY_EVERY);
            continue;
        };
        log("control: connected");
        was_connected = true;
        let (firmware, luminance) = handshake(&dev);
        emit(Input::Connected { firmware, luminance });
        if let Err(e) = serve(&dev, &commands, &emit) {
            log(format!("control: lost device: {e}"));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn send(dev: &HidDevice, ty: u8, payload: &[u8]) -> hidapi::HidResult<()> {
    dev.write(&proto::report(ty, payload)).map(|_| ())
}

/// Waits up to `timeout` for a reply to `cmd`, passing other messages through `other`.
fn await_reply(dev: &HidDevice, cmd: u8, timeout: Duration, mut other: impl FnMut(Message)) -> Option<Message> {
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 64];
    while Instant::now() < deadline {
        let wait = deadline.saturating_duration_since(Instant::now()).as_millis().max(1) as i32;
        match dev.read_timeout(&mut buf, wait) {
            Ok(0) => continue,
            Ok(n) => match proto::parse(&buf[..n]) {
                Some(m) if m.op == proto::OP_REPLY && m.cmd == cmd => return Some(m),
                Some(m) => other(m),
                None => {}
            },
            Err(_) => return None,
        }
    }
    None
}

fn handshake(dev: &HidDevice) -> (Option<String>, Option<u8>) {
    let mut firmware = None;
    let mut luminance = None;
    if send(dev, proto::TYPE_GET, &[proto::CMD_VERSION]).is_ok() {
        if let Some(m) = await_reply(dev, proto::CMD_VERSION, Duration::from_millis(500), |_| {}) {
            if m.data.len() >= 4 {
                firmware = Some(format!("{}.{}.{}", m.data[1], m.data[2], m.data[3]));
            }
        }
    }
    if send(dev, proto::TYPE_GET, &[proto::CMD_LUMINANCE]).is_ok() {
        if let Some(m) = await_reply(dev, proto::CMD_LUMINANCE, Duration::from_millis(500), |_| {}) {
            luminance = m.data.first().copied();
        }
    }
    log(format!("control: firmware {firmware:?}, luminance {luminance:?}"));
    (firmware, luminance)
}

fn serve(dev: &HidDevice, commands: &Receiver<Command>, emit: &impl Fn(Input)) -> Result<(), String> {
    let mut buf = [0u8; 64];
    let mut last_ping = Instant::now() - PING_EVERY;
    let mut last_pong = Instant::now();
    loop {
        if last_ping.elapsed() >= PING_EVERY {
            send(dev, proto::TYPE_GET, &[proto::CMD_PING]).map_err(|e| e.to_string())?;
            last_ping = Instant::now();
        }
        if last_pong.elapsed() > PING_EVERY * 4 {
            return Err("no pong for 20s".into());
        }
        match commands.try_recv() {
            Ok(Command::Tone(steps)) => {
                for (level, ms) in steps {
                    send(dev, proto::TYPE_SET, &[proto::CMD_BUZZER, level]).map_err(|e| e.to_string())?;
                    std::thread::sleep(Duration::from_millis(ms));
                }
                send(dev, proto::TYPE_SET, &[proto::CMD_BUZZER, 0]).map_err(|e| e.to_string())?;
            }
            Ok(Command::Luminance(v)) => {
                send(dev, proto::TYPE_SET, &[proto::CMD_LUMINANCE, v]).map_err(|e| e.to_string())?;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return Ok(()),
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        match dev.read_timeout(&mut buf, 100) {
            Ok(0) => {}
            Ok(n) => {
                if let Some(m) = proto::parse(&buf[..n]) {
                    match (m.op, m.cmd) {
                        (proto::OP_REPLY, proto::CMD_PING) => last_pong = Instant::now(),
                        (proto::OP_INPUT, proto::INPUT_KNOB_TURN) => {
                            emit(Input::KnobTurn(if m.data.first() == Some(&1) { 1 } else { -1 }))
                        }
                        (proto::OP_INPUT, proto::INPUT_KNOB_PRESS) => emit(Input::KnobPress(m.data.first().copied().unwrap_or(0))),
                        (proto::OP_REPLY, _) => {}
                        _ => log(format!("control: unhandled {m:02x?}")),
                    }
                }
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDCheckAccess(request: u32) -> u32;
    fn IOHIDRequestAccess(request: u32) -> bool;
}
const LISTEN_EVENT: u32 = 1; // kIOHIDRequestTypeListenEvent
const ACCESS_GRANTED: u32 = 0; // kIOHIDAccessTypeGranted

/// The touch panel counts as an input device, so macOS wants Input Monitoring for it.
/// Asks once (shows the system prompt and lists Quaked in System Settings).
fn ensure_input_monitoring() {
    static ASKED: std::sync::Once = std::sync::Once::new();
    ASKED.call_once(|| {
        if unsafe { IOHIDCheckAccess(LISTEN_EVENT) } != ACCESS_GRANTED {
            log("touch: needs Input Monitoring; turn on Quaked in System Settings > Privacy & Security > Input Monitoring");
            unsafe { IOHIDRequestAccess(LISTEN_EVENT) };
        }
    });
}

/// Owns the touch interface and turns touch frames into taps.
pub fn touch_thread(emit: impl Fn(Input) + Send + 'static) {
    let mut api = new_api();
    loop {
        let Some(dev) = open(&mut api, TOUCH_VID, TOUCH_PID, TOUCH_USAGE_PAGE, TOUCH_USAGE) else {
            ensure_input_monitoring();
            std::thread::sleep(RETRY_EVERY);
            continue;
        };
        log("touch: connected");
        if let Err(e) = read_touches(&dev, &emit) {
            log(format!("touch: lost device: {e}"));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// A press that lifts within this time and distance counts as a tap.
const TAP_MAX_MS: u128 = 900;
const TAP_MAX_MOVE: f64 = 60.0;
/// The panel sometimes stops reporting instead of sending a release.
const RELEASE_AFTER_SILENCE: Duration = Duration::from_millis(150);

fn read_touches(dev: &HidDevice, emit: &impl Fn(Input)) -> Result<(), String> {
    let mut buf = [0u8; 64];
    // (start time, start point, last point) of the current press
    let mut press: Option<(Instant, TouchPoint, TouchPoint, Instant)> = None;
    loop {
        let n = dev.read_timeout(&mut buf, 50).map_err(|e| e.to_string())?;
        let frame = if n > 0 { proto::parse_touch(&buf[..n]) } else { None };
        let first = frame.as_ref().and_then(|pts| pts.first().copied());
        match (press, first) {
            (None, Some(p)) if p.down => press = Some((Instant::now(), p, p, Instant::now())),
            (Some((t0, start, _, _)), Some(p)) if p.down => press = Some((t0, start, p, Instant::now())),
            (Some((t0, start, last, _)), Some(p)) if !p.down => {
                finish(t0, start, if p.x == 0 && p.y == 0 { last } else { p }, emit);
                press = None;
            }
            (Some((t0, start, last, seen)), None) if seen.elapsed() > RELEASE_AFTER_SILENCE => {
                finish(t0, start, last, emit);
                press = None;
            }
            _ => {}
        }
    }
}

fn finish(t0: Instant, start: TouchPoint, end: TouchPoint, emit: &impl Fn(Input)) {
    let moved = ((end.x as f64 - start.x as f64).powi(2) + (end.y as f64 - start.y as f64).powi(2)).sqrt();
    if t0.elapsed().as_millis() <= TAP_MAX_MS && moved <= TAP_MAX_MOVE {
        emit(Input::Tap { x: start.x as f64, y: start.y as f64 });
    }
}

/// Spawns both device threads. Returns the command sender for the control thread.
pub fn spawn(emit: impl Fn(Input) + Send + Clone + 'static) -> Sender<Command> {
    let (tx, rx) = std::sync::mpsc::channel();
    let e1 = emit.clone();
    std::thread::Builder::new().name("quake-control".into()).spawn(move || control_thread(rx, e1)).unwrap();
    std::thread::Builder::new().name("quake-touch".into()).spawn(move || touch_thread(emit)).unwrap();
    tx
}

/// `quaked probe`: prints raw reports from both interfaces for a few seconds.
pub fn probe(seconds: u64) {
    let mut api = new_api();
    let control = open(&mut api, QUAKE_VID, QUAKE_PID, QUAKE_USAGE_PAGE, QUAKE_USAGE);
    let touch = open(&mut api, TOUCH_VID, TOUCH_PID, TOUCH_USAGE_PAGE, TOUCH_USAGE);
    println!("control: {}", if control.is_some() { "open" } else { "NOT FOUND" });
    println!("touch:   {}", if touch.is_some() { "open" } else { "NOT FOUND" });
    if let Some(dev) = &control {
        let (fw, lum) = handshake(dev);
        println!("firmware {fw:?}, luminance {lum:?}");
        send(dev, proto::TYPE_GET, &[proto::CMD_PING]).ok();
        match await_reply(dev, proto::CMD_PING, Duration::from_millis(500), |_| {}) {
            Some(m) => println!("ping -> pong {:02x?}", m.data),
            None => println!("ping -> no reply"),
        }
    }
    println!("touch the screen / turn or press the knob for {seconds}s ...");
    let end = Instant::now() + Duration::from_secs(seconds);
    let mut buf = [0u8; 64];
    while Instant::now() < end {
        if let Some(dev) = &control {
            if let Ok(n) = dev.read_timeout(&mut buf, 20) {
                if n > 0 {
                    println!("control {:02x?} -> {:?}", &buf[..n.min(12)], proto::parse(&buf[..n]));
                }
            }
        }
        if let Some(dev) = &touch {
            if let Ok(n) = dev.read_timeout(&mut buf, 20) {
                if n > 0 {
                    println!("touch   {:02x?} -> {:?}", &buf[..n.min(16)], proto::parse_touch(&buf[..n]));
                }
            }
        }
    }
}
