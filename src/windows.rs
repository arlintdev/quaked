//! Window switcher: the Mac's regular apps front to back, with their window counts,
//! window titles (when macOS shares them) and icons, so the knob can bring one forward.
//!
//! The thread only looks while the page is on screen: once when it's shown, then every
//! couple of seconds. Order comes from CGWindowListCopyWindowInfo's z-order (each app
//! goes where its frontmost normal window is); titles come from the same list and are
//! empty without Screen Recording permission, which quaked never asks for.

use crate::config::WindowsSettings as Cfg;
use crate::device;
use objc2::rc::autoreleasepool;
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;
use tiny_skia::{IntSize, Pixmap};

/// How often the list refreshes while the page is shown.
const REFRESH: Duration = Duration::from_secs(2);
/// App icons are rendered at the size the board draws them.
pub const ICON_PX: u32 = 112;
/// How long "Switched to …" shows.
pub const FEEDBACK_MS: u64 = 1600;
/// Window titles kept per app.
const MAX_TITLES: usize = 6;

/// An app icon (shared, so Status stays cheap to clone; equal when it's the same bitmap).
#[derive(Clone)]
pub struct Icon(pub Arc<Pixmap>);

impl PartialEq for Icon {
    fn eq(&self, other: &Icon) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for Icon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Icon({}x{})", self.0.width(), self.0.height())
    }
}

/// One app on the switcher.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppWin {
    pub pid: i32,
    pub name: String,
    pub bundle_id: String,
    /// Normal windows on screen (this Space, not minimized).
    pub windows: usize,
    /// Their titles, front to back (empty without Screen Recording permission).
    pub titles: Vec<String>,
    /// The app in front right now.
    pub front: bool,
    pub hidden: bool,
    pub icon: Option<Icon>,
    /// SF Symbol drawn when there's no icon (the preview's sample apps).
    pub symbol: String,
}

/// What the board shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// Switched off in the settings.
    pub off: bool,
    /// At least one look has finished.
    pub loaded: bool,
    /// Front to back.
    pub apps: Vec<AppWin>,
    /// Windows are on screen but macOS shares none of their titles.
    pub titles_hidden: bool,
    /// The app just brought forward from the board, and when (status::now_ms).
    pub switched: Option<(String, u64)>,
}

#[derive(Debug)]
pub enum Cmd {
    Configure(Cfg),
    /// The page was shown (look now, then every couple of seconds) or left (stop looking).
    Shown(bool),
    /// Bring this app (pid) forward with all its windows.
    Activate(i32),
}

/// What the module's thread sends the app (handled in app/windows.rs on the main thread).
#[derive(Debug)]
pub enum Update {
    Status(Status),
}

/// Starts the module's background work; `emit` hands updates to the app.
pub fn spawn(cfg: Cfg, emit: impl Fn(Update) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("windows".into()).spawn(move || run(cfg, rx, emit)).unwrap();
    tx
}

fn run(mut cfg: Cfg, rx: Receiver<Cmd>, emit: impl Fn(Update)) {
    let mut shown = false;
    let mut icons: HashMap<String, Icon> = HashMap::new();
    let mut sent: Option<Status> = None;
    let mut switched: Option<(String, u64)> = None;
    loop {
        // Only wake on a timer while the page is on screen.
        let cmd = if shown && cfg.enabled {
            match rx.recv_timeout(REFRESH) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        };
        match cmd {
            Some(Cmd::Configure(c)) => cfg = c,
            Some(Cmd::Shown(s)) => {
                let was = shown;
                shown = s;
                if !s && !was {
                    continue;
                }
            }
            Some(Cmd::Activate(pid)) => {
                if let Some(name) = activate(pid) {
                    switched = Some((name, crate::status::now_ms()));
                }
            }
            None => {}
        }
        let st = if !cfg.enabled {
            icons.clear();
            Status { off: true, loaded: true, ..Default::default() }
        } else if shown {
            let recent = switched.clone().filter(|(_, at)| crate::status::now_ms() < at + FEEDBACK_MS);
            Status { switched: recent, ..scan(&cfg, &mut icons) }
        } else {
            continue;
        };
        if sent.as_ref() != Some(&st) {
            emit(Update::Status(st.clone()));
            sent = Some(st);
        }
    }
}

// ---------------------------------------------------------------- ordering (pure)

/// A running regular app, as AppKit reports it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Proc {
    pub pid: i32,
    pub name: String,
    pub bundle_id: String,
    pub hidden: bool,
    pub icon: Option<Icon>,
}

/// An on-screen window, front to back, as CGWindowListCopyWindowInfo reports it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Win {
    pub pid: i32,
    pub layer: i32,
    pub title: String,
    pub w: f64,
    pub h: f64,
    pub alpha: f64,
}

impl Win {
    /// A normal document window (not a menu, panel overlay, invisible helper or sliver).
    fn is_normal(&self) -> bool {
        self.layer == 0 && self.alpha > 0.01 && self.w >= 60.0 && self.h >= 40.0
    }
}

fn excluded(p: &Proc, exclude: &[String]) -> bool {
    exclude.iter().map(|e| e.trim()).filter(|e| !e.is_empty()).any(|e| e.eq_ignore_ascii_case(&p.name) || e.eq_ignore_ascii_case(&p.bundle_id))
}

/// Front to back: the app in front first, then each app by its frontmost normal window,
/// then apps with nothing on screen by name. Leaves out `me` (quaked) and `exclude`.
pub fn arrange(procs: Vec<Proc>, wins: &[Win], front: Option<i32>, me: i32, exclude: &[String]) -> Vec<AppWin> {
    let normal: Vec<&Win> = wins.iter().filter(|w| w.is_normal()).collect();
    let mut out: Vec<(usize, AppWin)> = procs
        .into_iter()
        .filter(|p| p.pid != me && !excluded(p, exclude))
        .map(|p| {
            let mine: Vec<(usize, &&Win)> = normal.iter().enumerate().filter(|(_, w)| w.pid == p.pid).collect();
            let z = mine.first().map(|(i, _)| *i).unwrap_or(usize::MAX);
            let mut titles: Vec<String> = Vec::new();
            for (_, w) in &mine {
                let t = w.title.trim();
                if !t.is_empty() && !titles.iter().any(|x| x == t) && titles.len() < MAX_TITLES {
                    titles.push(t.to_string());
                }
            }
            let app = AppWin { pid: p.pid, windows: mine.len(), titles, front: Some(p.pid) == front, hidden: p.hidden, icon: p.icon, name: p.name, bundle_id: p.bundle_id, symbol: String::new() };
            (z, app)
        })
        .collect();
    out.sort_by(|(za, a), (zb, b)| (!a.front, *za, a.name.to_lowercase()).cmp(&(!b.front, *zb, b.name.to_lowercase())));
    out.into_iter().map(|(_, a)| a).collect()
}

// ---------------------------------------------------------------- AppKit / CoreGraphics

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *const c_void);
}

const ON_SCREEN_ONLY: u32 = 1 << 0;
const EXCLUDE_DESKTOP: u32 = 1 << 4;
/// NSApplicationActivationPolicyRegular: apps with a Dock icon and windows.
const POLICY_REGULAR: isize = 0;
/// NSApplicationActivateAllWindows | NSApplicationActivateIgnoringOtherApps.
const ACTIVATE_ALL: usize = 1 << 0 | 1 << 1;

/// An NSString as a Rust string.
pub(crate) unsafe fn ns_string(obj: *mut AnyObject) -> Option<String> {
    if obj.is_null() {
        return None;
    }
    let utf8: *const std::ffi::c_char = msg_send![obj, UTF8String];
    (!utf8.is_null()).then(|| std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned())
}

/// An NSArray of NSStrings as Rust strings.
pub(crate) unsafe fn ns_strings(arr: *mut AnyObject) -> Vec<String> {
    if arr.is_null() {
        return Vec::new();
    }
    let n: usize = msg_send![arr, count];
    (0..n)
        .filter_map(|i| {
            let s: *mut AnyObject = msg_send![arr, objectAtIndex: i];
            ns_string(s)
        })
        .collect()
}

unsafe fn frontmost_app() -> *mut AnyObject {
    let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
    msg_send![ws, frontmostApplication]
}

/// The name of the app in front ("" if none).
pub fn frontmost_name() -> String {
    autoreleasepool(|_| unsafe {
        let app = frontmost_app();
        if app.is_null() {
            return String::new();
        }
        let name: *mut AnyObject = msg_send![app, localizedName];
        ns_string(name).unwrap_or_default()
    })
}

fn frontmost_pid() -> Option<i32> {
    autoreleasepool(|_| unsafe {
        let app = frontmost_app();
        if app.is_null() {
            return None;
        }
        let pid: i32 = msg_send![app, processIdentifier];
        Some(pid)
    })
}

/// Regular apps, with icons from (and added to) the cache.
unsafe fn running_apps(icons: &mut HashMap<String, Icon>) -> Vec<Proc> {
    let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
    let apps: *mut AnyObject = msg_send![ws, runningApplications];
    if apps.is_null() {
        return Vec::new();
    }
    let n: usize = msg_send![apps, count];
    let mut out = Vec::new();
    for i in 0..n {
        let app: *mut AnyObject = msg_send![apps, objectAtIndex: i];
        let policy: isize = msg_send![app, activationPolicy];
        let terminated: Bool = msg_send![app, isTerminated];
        if policy != POLICY_REGULAR || terminated.as_bool() {
            continue;
        }
        let pid: i32 = msg_send![app, processIdentifier];
        let name: *mut AnyObject = msg_send![app, localizedName];
        let bundle: *mut AnyObject = msg_send![app, bundleIdentifier];
        let hidden: Bool = msg_send![app, isHidden];
        let bundle_id = ns_string(bundle).unwrap_or_default();
        let key = icon_key(pid, &bundle_id);
        let icon = match icons.get(&key) {
            Some(icon) => Some(icon.clone()),
            None => {
                let img: *mut AnyObject = msg_send![app, icon];
                let icon = image_to_pixmap(img, ICON_PX).map(|p| Icon(Arc::new(p)));
                if let Some(icon) = &icon {
                    icons.insert(key, icon.clone());
                }
                icon
            }
        };
        out.push(Proc { pid, name: ns_string(name).unwrap_or_default(), bundle_id, hidden: hidden.as_bool(), icon });
    }
    out
}

fn icon_key(pid: i32, bundle_id: &str) -> String {
    if bundle_id.is_empty() { format!("pid:{pid}") } else { bundle_id.to_string() }
}

unsafe fn number(dict: *mut AnyObject, key: &str) -> Option<f64> {
    let k = NSString::from_str(key);
    let v: *mut AnyObject = msg_send![dict, objectForKey: &*k];
    if v.is_null() {
        return None;
    }
    let d: f64 = msg_send![v, doubleValue];
    Some(d)
}

/// On-screen windows, front to back.
unsafe fn on_screen_windows() -> Vec<Win> {
    let list = CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0);
    if list.is_null() {
        return Vec::new();
    }
    // CFArray / CFDictionary are toll-free bridged with NSArray / NSDictionary.
    let arr = list as *mut AnyObject;
    let n: usize = msg_send![arr, count];
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let d: *mut AnyObject = msg_send![arr, objectAtIndex: i];
        let Some(pid) = number(d, "kCGWindowOwnerPID") else { continue };
        let name_key = NSString::from_str("kCGWindowName");
        let title: *mut AnyObject = msg_send![d, objectForKey: &*name_key];
        let bounds_key = NSString::from_str("kCGWindowBounds");
        let bounds: *mut AnyObject = msg_send![d, objectForKey: &*bounds_key];
        let (w, h) = if bounds.is_null() { (0.0, 0.0) } else { (number(bounds, "Width").unwrap_or(0.0), number(bounds, "Height").unwrap_or(0.0)) };
        out.push(Win {
            pid: pid as i32,
            layer: number(d, "kCGWindowLayer").unwrap_or(0.0) as i32,
            title: ns_string(title).unwrap_or_default(),
            w,
            h,
            alpha: number(d, "kCGWindowAlpha").unwrap_or(1.0),
        });
    }
    CFRelease(list);
    out
}

/// One look at the apps and windows.
fn scan(cfg: &Cfg, icons: &mut HashMap<String, Icon>) -> Status {
    autoreleasepool(|_| unsafe {
        let me = std::process::id() as i32;
        let procs = running_apps(icons);
        let wins = on_screen_windows();
        let front = frontmost_pid();
        // Forget icons of apps that quit.
        let keys: std::collections::HashSet<String> = procs.iter().map(|p| icon_key(p.pid, &p.bundle_id)).collect();
        icons.retain(|k, _| keys.contains(k));
        let others: Vec<&Win> = wins.iter().filter(|w| w.pid != me && w.is_normal()).collect();
        let titles_hidden = !others.is_empty() && others.iter().all(|w| w.title.trim().is_empty());
        Status { off: false, loaded: true, apps: arrange(procs, &wins, front, me, &cfg.exclude), titles_hidden, switched: None }
    })
}

/// Brings an app forward with all its windows (unhiding it). Returns its name if it's
/// running. If macOS doesn't let the activation through (a background app asking),
/// falls back to LaunchServices, which reopens it the way clicking its Dock icon does.
fn activate(pid: i32) -> Option<String> {
    let (name, bundle_id) = autoreleasepool(|_| unsafe {
        let app: *mut AnyObject = msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
        if app.is_null() {
            return None;
        }
        let name: *mut AnyObject = msg_send![app, localizedName];
        let bundle: *mut AnyObject = msg_send![app, bundleIdentifier];
        let hidden: Bool = msg_send![app, isHidden];
        if hidden.as_bool() {
            let _: Bool = msg_send![app, unhide];
        }
        let _: Bool = msg_send![app, activateWithOptions: ACTIVATE_ALL];
        Some((ns_string(name).unwrap_or_default(), ns_string(bundle).unwrap_or_default()))
    })?;
    device::log(format!("windows: bringing {name} forward"));
    std::thread::sleep(Duration::from_millis(250));
    if frontmost_pid() != Some(pid) && !bundle_id.is_empty() {
        device::log(format!("windows: {name} didn't come forward; asking LaunchServices"));
        let child = std::process::Command::new("/usr/bin/open")
            .args(["-b", &bundle_id])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match child {
            Ok(mut c) => {
                let _ = c.wait();
            }
            Err(e) => device::log(format!("windows: open -b failed: {e}")),
        }
    }
    Some(name)
}

/// Draws an NSImage into a square, full-color bitmap.
unsafe fn image_to_pixmap(img: *mut AnyObject, px: u32) -> Option<Pixmap> {
    if img.is_null() || px == 0 {
        return None;
    }
    let side = px as isize;
    let rep: *mut AnyObject = msg_send![class!(NSBitmapImageRep), alloc];
    let space = NSString::from_str("NSDeviceRGBColorSpace");
    let rep: *mut AnyObject = msg_send![rep,
        initWithBitmapDataPlanes: std::ptr::null_mut::<*mut u8>(),
        pixelsWide: side,
        pixelsHigh: side,
        bitsPerSample: 8isize,
        samplesPerPixel: 4isize,
        hasAlpha: Bool::YES,
        isPlanar: Bool::NO,
        colorSpaceName: &*space,
        bytesPerRow: 0isize,
        bitsPerPixel: 0isize];
    if rep.is_null() {
        return None;
    }
    let ctx: *mut AnyObject = msg_send![class!(NSGraphicsContext), graphicsContextWithBitmapImageRep: rep];
    if ctx.is_null() {
        let _: () = msg_send![rep, release];
        return None;
    }
    let _: () = msg_send![class!(NSGraphicsContext), saveGraphicsState];
    let _: () = msg_send![class!(NSGraphicsContext), setCurrentContext: ctx];
    let _: () = msg_send![ctx, setImageInterpolation: 3usize]; // NSImageInterpolationHigh
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(px as f64, px as f64));
    let zero = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
    let _: () = msg_send![img, drawInRect: rect, fromRect: zero, operation: 2usize, fraction: 1.0f64];
    let _: () = msg_send![ctx, flushGraphics];
    let _: () = msg_send![class!(NSGraphicsContext), restoreGraphicsState];
    let data: *mut u8 = msg_send![rep, bitmapData];
    let stride: isize = msg_send![rep, bytesPerRow];
    let mut out = None;
    if !data.is_null() && stride >= side * 4 {
        let mut rgba = Vec::with_capacity((px * px * 4) as usize);
        for y in 0..side {
            let row = std::slice::from_raw_parts(data.offset(y * stride), (side * 4) as usize);
            for p in row.chunks_exact(4) {
                // Premultiplied already (NSBitmapImageRep's default); keep it valid.
                let a = p[3];
                rgba.extend_from_slice(&[p[0].min(a), p[1].min(a), p[2].min(a), a]);
            }
        }
        out = IntSize::from_wh(px, px).and_then(|size| Pixmap::from_vec(rgba, size));
    }
    let _: () = msg_send![rep, release];
    out
}

/// The Finder icon of an app bundle on disk (for the preview's sample apps).
fn icon_for_path(path: &str) -> Option<Icon> {
    // Resolved, or a symlinked app (Safari) gets an alias badge.
    let path = std::fs::canonicalize(path).ok()?;
    autoreleasepool(|_| unsafe {
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let p = NSString::from_str(&path.to_string_lossy());
        let img: *mut AnyObject = msg_send![ws, iconForFile: &*p];
        image_to_pixmap(img, ICON_PX).map(|p| Icon(Arc::new(p)))
    })
}

/// Sample data for `quaked preview` (make it look like a real moment).
pub fn sample() -> Status {
    let app = |pid: i32, name: &str, bundle: &str, path: &str, symbol: &str, windows: usize, titles: &[&str]| AppWin {
        pid,
        name: name.into(),
        bundle_id: bundle.into(),
        windows,
        titles: titles.iter().map(|t| t.to_string()).collect(),
        front: false,
        hidden: false,
        icon: icon_for_path(path),
        symbol: symbol.into(),
    };
    let mut apps = vec![
        app(501, "Safari", "com.apple.Safari", "/Applications/Safari.app", "safari.fill", 3, &["PLC Playground — ladder logic in the browser", "arlintdev/quaked · Pull Request #12", "DECOKEE Quake — specs"]),
        app(502, "Terminal", "com.apple.Terminal", "/System/Applications/Utilities/Terminal.app", "terminal.fill", 2, &["quaked — cargo build --release", "workbench — docker compose logs -f"]),
        app(503, "Messages", "com.apple.MobileSMS", "/System/Applications/Messages.app", "message.fill", 1, &["Sam"]),
        app(504, "Notes", "com.apple.Notes", "/System/Applications/Notes.app", "note.text", 1, &["Release checklist 0.47"]),
        app(505, "Finder", "com.apple.finder", "/System/Library/CoreServices/Finder.app", "folder.fill", 2, &["quaked", "Downloads"]),
        app(506, "Music", "com.apple.Music", "/System/Applications/Music.app", "music.note", 1, &["Music"]),
        app(507, "Mail", "com.apple.mail", "/System/Applications/Mail.app", "envelope.fill", 0, &[]),
        app(508, "Calendar", "com.apple.iCal", "/System/Applications/Calendar.app", "calendar", 0, &[]),
    ];
    apps[0].front = true;
    apps[6].hidden = true;
    Status { off: false, loaded: true, apps, titles_hidden: false, switched: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: i32, name: &str) -> Proc {
        Proc { pid, name: name.into(), bundle_id: format!("com.example.{}", name.to_lowercase()), ..Default::default() }
    }

    fn win(pid: i32, title: &str) -> Win {
        Win { pid, layer: 0, title: title.into(), w: 800.0, h: 600.0, alpha: 1.0 }
    }

    fn names(apps: &[AppWin]) -> Vec<&str> {
        apps.iter().map(|a| a.name.as_str()).collect()
    }

    #[test]
    fn apps_follow_their_frontmost_window() {
        let procs = vec![proc(1, "Alpha"), proc(2, "Bravo"), proc(3, "Charlie")];
        let wins = [win(3, "c1"), win(1, "a1"), win(3, "c2"), win(2, "b1")];
        let apps = arrange(procs, &wins, Some(3), 99, &[]);
        assert_eq!(names(&apps), ["Charlie", "Alpha", "Bravo"]);
        assert_eq!(apps[0].windows, 2);
        assert_eq!(apps[0].titles, ["c1", "c2"]);
        assert!(apps[0].front && !apps[1].front);
    }

    #[test]
    fn front_app_leads_even_without_windows_and_the_rest_go_by_name() {
        let procs = vec![proc(1, "zed"), proc(2, "Finder"), proc(3, "Bravo"), proc(4, "alpha")];
        let wins = [win(3, "")];
        let apps = arrange(procs, &wins, Some(2), 99, &[]);
        // Finder is in front (desktop clicked); Bravo has the only window; then by name.
        assert_eq!(names(&apps), ["Finder", "Bravo", "alpha", "zed"]);
        assert_eq!(apps[0].windows, 0);
        assert!(apps[1].titles.is_empty(), "untitled windows count but add no title");
        assert_eq!(apps[1].windows, 1);
    }

    #[test]
    fn leaves_out_quaked_and_excluded_apps() {
        let procs = vec![proc(1, "Alpha"), proc(2, "quaked"), proc(3, "Bravo"), proc(4, "Charlie")];
        let exclude = ["bravo".to_string(), "COM.EXAMPLE.CHARLIE".to_string(), " ".to_string()];
        let apps = arrange(procs, &[win(2, "board")], None, 2, &exclude);
        assert_eq!(names(&apps), ["Alpha"]);
    }

    #[test]
    fn only_normal_windows_count() {
        let procs = vec![proc(1, "Alpha"), proc(2, "Bravo")];
        let menu = Win { layer: 25, ..win(2, "status item") };
        let ghost = Win { alpha: 0.0, ..win(2, "invisible") };
        let sliver = Win { w: 20.0, ..win(2, "sliver") };
        let wins = [menu, ghost, sliver, win(1, "a1"), win(2, "b1"), win(1, "a1")];
        let apps = arrange(procs, &wins, None, 99, &[]);
        assert_eq!(names(&apps), ["Alpha", "Bravo"]);
        assert_eq!(apps[1].windows, 1);
        assert_eq!(apps[1].titles, ["b1"]);
        assert_eq!(apps[0].windows, 2);
        assert_eq!(apps[0].titles, ["a1"], "duplicate titles collapse");
    }

    #[test]
    fn icons_compare_by_identity() {
        let p = Arc::new(Pixmap::new(2, 2).unwrap());
        let (a, b) = (Icon(p.clone()), Icon(p));
        let c = Icon(Arc::new(Pixmap::new(2, 2).unwrap()));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
