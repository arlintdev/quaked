//! Clipboard picker: the last few things you copied, so the knob can paste any of them.
//!
//! A thread watches NSPasteboard's change count every 400 ms and keeps the plain text of
//! each new copy, with the app that was in front and the time. History lives in memory
//! only (never on disk, never in the log, never in `quaked ctl status`). Copies that
//! password managers mark private (org.nspasteboard.ConcealedType and friends), copies
//! made in front of password apps, and (optionally) strings that look like keys or
//! tokens are skipped without reading their text.
//!
//! Pasting puts the chosen text back on the pasteboard and, 100 ms later, asks the app to
//! type ⌘V on the Quake's USB keyboard, so the app in front pastes it (no Accessibility
//! permission needed).

use crate::config::PasteSettings as Cfg;
use crate::device;
use crate::windows::{frontmost_name, ns_string, ns_strings};
use objc2::rc::autoreleasepool;
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};
use objc2_foundation::NSString;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

const POLL: Duration = Duration::from_millis(400);
/// Between putting the text on the pasteboard and typing ⌘V.
const PASTE_DELAY: Duration = Duration::from_millis(100);
/// Copies bigger than this aren't kept (the board pastes snippets, not files).
const MAX_BYTES: usize = 512 * 1024;
const MAX_HISTORY: usize = 100;
/// NSPasteboardTypeString.
const TEXT_TYPE: &str = "public.utf8-plain-text";
/// How long "Pasted" shows.
pub const FEEDBACK_MS: u64 = 1600;
/// Characters kept for the one-line previews.
const PREVIEW_CHARS: usize = 240;

/// What a copy looks like, for its icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Text,
    /// Several lines of prose or a list.
    Lines,
    Code,
    Link,
    Path,
    Email,
    Number,
}

/// One copy.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Clip {
    pub id: u64,
    pub text: Arc<str>,
    /// The first line with something on it, trimmed.
    pub first: String,
    /// What follows it, on one line.
    pub rest: String,
    pub chars: usize,
    pub lines: usize,
    pub kind: Kind,
    /// The app in front when it was copied ("" if unknown, e.g. already there at start).
    pub app: String,
    pub at_ms: u64,
}

impl Clip {
    pub fn new(id: u64, text: &str, app: &str, at_ms: u64) -> Clip {
        let lines = text.trim_end().lines().count().max(1);
        let mut it = text.lines().skip_while(|l| l.trim().is_empty());
        let first = squash(it.next().unwrap_or(""));
        let rest = squash(&it.collect::<Vec<_>>().join(" "));
        Clip { id, text: Arc::from(text), first, rest, chars: text.chars().count(), lines, kind: kind_of(text), app: app.to_string(), at_ms }
    }
}

/// Whitespace collapsed to single spaces, cut to the preview length.
fn squash(s: &str) -> String {
    let mut out = String::new();
    for w in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
        if out.len() > PREVIEW_CHARS * 4 {
            break;
        }
    }
    out.chars().take(PREVIEW_CHARS).collect()
}

/// A guess at what the text is.
pub fn kind_of(text: &str) -> Kind {
    let t = text.trim();
    let multi = t.lines().count() > 1;
    if !multi {
        let one_word = !t.contains(char::is_whitespace);
        let lower = t.to_ascii_lowercase();
        if one_word && (lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("www.")) {
            return Kind::Link;
        }
        if one_word && t.matches('@').count() == 1 && t.split('@').nth(1).is_some_and(|d| d.contains('.') && !d.ends_with('.')) && !t.starts_with('@') {
            return Kind::Email;
        }
        if (t.starts_with('/') || t.starts_with("~/")) && t.len() > 1 {
            return Kind::Path;
        }
        let digits = t.chars().filter(|c| c.is_ascii_digit()).count();
        if digits > 0 && t.len() <= 32 && t.chars().all(|c| c.is_ascii_digit() || " .,-+$€£%".contains(c)) {
            return Kind::Number;
        }
        return Kind::Text;
    }
    let code_lines = t
        .lines()
        .filter(|l| {
            let l = l.trim_end();
            l.ends_with(';') || l.ends_with('{') || l.ends_with('}') || l.ends_with(')') || l.starts_with("    ") || l.starts_with('\t') || l.trim_start().starts_with("//") || l.trim_start().starts_with("fn ") || l.trim_start().starts_with("def ")
        })
        .count();
    if code_lines * 3 >= t.lines().count() {
        Kind::Code
    } else {
        Kind::Lines
    }
}

/// The paste that just happened (shown as "Pasted" for a moment).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pasted {
    pub id: u64,
    /// The app in front, which got the ⌘V.
    pub into: String,
    pub at_ms: u64,
}

/// What the board shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// Switched off in the settings (nothing is watched or kept).
    pub off: bool,
    /// Newest first.
    pub items: Vec<Clip>,
    /// The copy on the pasteboard right now, if it's one of `items` (not after a skipped
    /// private copy, an image or a file).
    pub current: Option<u64>,
    /// Private copies skipped since quaked started (just a count).
    pub skipped: usize,
    /// The app in front (only kept up to date while the page is shown): where ⌘V goes.
    pub front: String,
    pub pasted: Option<Pasted>,
}

#[derive(Debug)]
pub enum Cmd {
    Configure(Cfg),
    /// Paste this copy (by id) into the app in front.
    Paste(u64),
    /// Drop this copy from the history.
    Forget(u64),
    /// The page was shown or left (the front app is only watched while it's shown).
    Shown(bool),
}

/// What the module's thread sends the app (handled in app/paste.rs on the main thread).
#[derive(Debug)]
pub enum Update {
    Status(Status),
    /// The text is on the pasteboard: type ⌘V now.
    TypePaste,
}

/// Starts the module's background work; `emit` hands updates to the app.
pub fn spawn(cfg: Cfg, emit: impl Fn(Update) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("clipboard".into()).spawn(move || run(cfg, rx, emit)).unwrap();
    tx
}

fn run(mut cfg: Cfg, rx: Receiver<Cmd>, emit: impl Fn(Update)) {
    let mut st = Status { off: !cfg.enabled, ..Default::default() };
    // The change count last looked at; None = look at whatever is there now.
    let mut seen: Option<isize> = None;
    let mut next_id = 1u64;
    let mut shown = false;
    let mut sent: Option<Status> = None;
    loop {
        match rx.recv_timeout(POLL) {
            Ok(Cmd::Configure(c)) => {
                if !c.enabled {
                    st = Status { off: true, ..Default::default() };
                    seen = None;
                }
                cfg = c;
                st.off = !cfg.enabled;
                st.items.truncate(cfg.history.clamp(1, MAX_HISTORY));
            }
            Ok(Cmd::Paste(id)) => {
                if let Some(i) = st.items.iter().position(|c| c.id == id) {
                    let text = st.items[i].text.clone();
                    if write(&text) {
                        // Ours: don't take it for a new copy; move it to the top instead.
                        seen = Some(change_count());
                        let mut clip = st.items.remove(i);
                        clip.at_ms = crate::status::now_ms();
                        st.items.insert(0, clip);
                        st.current = Some(id);
                        st.front = frontmost_name();
                        st.pasted = Some(Pasted { id, into: st.front.clone(), at_ms: crate::status::now_ms() });
                        emit(Update::Status(st.clone()));
                        sent = Some(st.clone());
                        std::thread::sleep(PASTE_DELAY);
                        emit(Update::TypePaste);
                        device::log(format!("clipboard: pasted {} chars", text.chars().count()));
                    } else {
                        device::log("clipboard: couldn't write to the pasteboard");
                    }
                }
            }
            Ok(Cmd::Forget(id)) => {
                st.items.retain(|c| c.id != id);
                if st.current == Some(id) {
                    st.current = None;
                }
            }
            Ok(Cmd::Shown(s)) => shown = s,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if cfg.enabled {
            look(&cfg, &mut st, &mut seen, &mut next_id);
            if shown {
                st.front = frontmost_name();
            }
        }
        if st.pasted.as_ref().is_some_and(|p| crate::status::now_ms() >= p.at_ms + FEEDBACK_MS) {
            st.pasted = None;
        }
        if sent.as_ref() != Some(&st) {
            emit(Update::Status(st.clone()));
            sent = Some(st.clone());
        }
    }
}

/// Takes in a new copy, if there is one.
fn look(cfg: &Cfg, st: &mut Status, seen: &mut Option<isize>, next_id: &mut u64) {
    let n = change_count();
    if *seen == Some(n) {
        return;
    }
    // What's there when quaked starts was copied who knows where.
    let at_start = seen.is_none();
    *seen = Some(n);
    let app = frontmost_name();
    st.current = None;
    // Privacy first: decide from the app and the types before reading any text.
    if ignored_app(&app, &cfg.ignore_apps) || is_private(&types()) {
        st.skipped += 1;
        device::log("clipboard: skipped a private copy");
        return;
    }
    let Some(text) = read_text() else { return }; // an image, files, nothing
    if text.trim().is_empty() {
        return;
    }
    if text.len() > MAX_BYTES {
        device::log(format!("clipboard: not keeping a {} KB copy", text.len() / 1024));
        return;
    }
    if cfg.skip_secrets && looks_secret(&text) {
        st.skipped += 1;
        device::log("clipboard: skipped a copy that looks like a key");
        return;
    }
    let clip = Clip::new(*next_id, &text, if at_start { "" } else { &app }, crate::status::now_ms());
    *next_id += 1;
    device::log(format!("clipboard: copied {} chars", clip.chars));
    st.current = Some(clip.id);
    push(&mut st.items, clip, cfg.history.clamp(1, MAX_HISTORY));
}

/// Adds a copy at the top; copying something already kept moves it to the top instead.
pub fn push(items: &mut Vec<Clip>, clip: Clip, cap: usize) {
    items.retain(|c| c.text != clip.text);
    items.insert(0, clip);
    items.truncate(cap.max(1));
}

/// Pasteboard types that mean "don't keep this" (password managers, generated or
/// transient content). See nspasteboard.org.
pub fn is_private(types: &[String]) -> bool {
    const EXACT: [&str; 4] = ["org.nspasteboard.ConcealedType", "org.nspasteboard.TransientType", "org.nspasteboard.AutoGeneratedType", "com.agilebits.onepassword"];
    const CONTAINS: [&str; 8] = ["1password", "bitwarden", "keepass", "agilebits", "concealed", "transient", "password", "dashlane"];
    types.iter().any(|t| {
        let lower = t.to_lowercase();
        EXACT.iter().any(|e| t == e) || CONTAINS.iter().any(|c| lower.contains(c))
    })
}

/// True if copies made with this app in front are never kept.
pub fn ignored_app(app: &str, ignore: &[String]) -> bool {
    let app = app.to_lowercase();
    !app.is_empty() && ignore.iter().map(|i| i.trim().to_lowercase()).any(|i| !i.is_empty() && app.contains(&i))
}

/// A single token that looks like an API key, access token or private key.
pub fn looks_secret(text: &str) -> bool {
    let t = text.trim();
    if t.contains("-----BEGIN") && t.contains("PRIVATE KEY") {
        return true;
    }
    if t.contains(char::is_whitespace) || t.len() < 16 || t.len() > 4096 {
        return false;
    }
    const PREFIXES: [&str; 22] = [
        "sk-", "sk_live_", "sk_test_", "rk_live_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "glpat-", "xoxb-", "xoxp-", "xoxa-", "xoxs-", "xapp-", "AIza", "ya29.", "npm_", "pypi-", "shpat_", "dop_v1_",
    ];
    if PREFIXES.iter().any(|p| t.starts_with(p)) {
        return true;
    }
    // AWS access key ids.
    if (t.starts_with("AKIA") || t.starts_with("ASIA")) && t.len() == 20 && t.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
        return true;
    }
    // JWTs: three base64url parts, the first a JSON header.
    let parts: Vec<&str> = t.split('.').collect();
    parts.len() == 3 && t.starts_with("eyJ") && parts.iter().all(|p| p.len() >= 8 && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
}

// ---------------------------------------------------------------- NSPasteboard

unsafe fn pasteboard() -> *mut AnyObject {
    msg_send![class!(NSPasteboard), generalPasteboard]
}

fn change_count() -> isize {
    autoreleasepool(|_| unsafe {
        let pb = pasteboard();
        if pb.is_null() {
            return 0;
        }
        msg_send![pb, changeCount]
    })
}

fn types() -> Vec<String> {
    autoreleasepool(|_| unsafe {
        let pb = pasteboard();
        if pb.is_null() {
            return Vec::new();
        }
        let types: *mut AnyObject = msg_send![pb, types];
        ns_strings(types)
    })
}

fn read_text() -> Option<String> {
    autoreleasepool(|_| unsafe {
        let pb = pasteboard();
        if pb.is_null() {
            return None;
        }
        let ty = NSString::from_str(TEXT_TYPE);
        let s: *mut AnyObject = msg_send![pb, stringForType: &*ty];
        ns_string(s)
    })
}

fn write(text: &str) -> bool {
    autoreleasepool(|_| unsafe {
        let pb = pasteboard();
        if pb.is_null() {
            return false;
        }
        let _: isize = msg_send![pb, clearContents];
        let s = NSString::from_str(text);
        let ty = NSString::from_str(TEXT_TYPE);
        let ok: Bool = msg_send![pb, setString: &*s, forType: &*ty];
        ok.as_bool()
    })
}

/// Sample data for `quaked preview` (make it look like a real moment).
pub fn sample() -> Status {
    let now = crate::status::now_ms();
    let min = 60_000;
    let items = vec![
        Clip::new(8, "cargo build --release -j 4 && ./target/release/quaked preview /tmp/quaked-preview", "Terminal", now - 2 * min),
        Clip::new(7, "https://plcplayground.notdone.dev/?sim=ton-demo&rung=4", "Safari", now - 6 * min),
        Clip::new(
            6,
            "fn list_window(n: usize, sel: usize, rows: usize) -> usize {\n    if n <= rows {\n        0\n    } else {\n        sel.saturating_sub(rows - 1).min(n - rows)\n    }\n}\n",
            "Zed",
            now - 14 * min,
        ),
        Clip::new(5, "Hey Sam, 0.47 is green. I'll merge right after the demo; the release notes are on the checklist page.", "Messages", now - 31 * min),
        Clip::new(4, "~/ArlintDev/quaked/src/render/paste.rs", "Finder", now - 58 * min),
        Clip::new(3, "Release checklist\n- bump the version to 0.47\n- run the 0256 migration on staging\n- post the notes in #release", "Notes", now - 2 * 60 * min - 5 * min),
        Clip::new(2, "1,284.50", "Numbers", now - 3 * 60 * min - 40 * min),
        Clip::new(1, "invoices@example.com", "Mail", now - 26 * 60 * min),
    ];
    Status { off: false, current: Some(8), items, skipped: 2, front: "Safari".into(), pasted: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn password_manager_copies_are_private() {
        assert!(is_private(&types(&["public.utf8-plain-text", "org.nspasteboard.ConcealedType"])));
        assert!(is_private(&types(&["org.nspasteboard.TransientType", "public.utf8-plain-text"])));
        assert!(is_private(&types(&["org.nspasteboard.AutoGeneratedType"])));
        assert!(is_private(&types(&["com.agilebits.onepassword", "public.utf8-plain-text"])));
        assert!(is_private(&types(&["com.1password.secret"])));
        assert!(is_private(&types(&["com.bitwarden.desktop"])));
        assert!(is_private(&types(&["org.keepassxc.KeePassXC"])));
        assert!(is_private(&types(&["de.petermaurer.TransientPasteboardType"])));
    }

    #[test]
    fn ordinary_copies_are_not_private() {
        assert!(!is_private(&types(&["public.utf8-plain-text", "NSStringPboardType"])));
        assert!(!is_private(&types(&["public.html", "public.rtf", "public.utf8-plain-text", "com.apple.webarchive"])));
        assert!(!is_private(&types(&[])));
    }

    #[test]
    fn password_apps_are_ignored() {
        let ignore = Cfg::default().ignore_apps;
        assert!(ignored_app("Passwords", &ignore));
        assert!(ignored_app("1Password 7", &ignore));
        assert!(ignored_app("KeePassXC", &ignore));
        assert!(!ignored_app("Safari", &ignore));
        assert!(!ignored_app("", &ignore));
        assert!(!ignored_app("Safari", &["".to_string(), "  ".to_string()]));
    }

    #[test]
    fn recopying_moves_to_the_top_and_history_is_capped() {
        let mut items = Vec::new();
        for (id, t) in [(1, "one"), (2, "two"), (3, "three")] {
            push(&mut items, Clip::new(id, t, "App", id), 3);
        }
        let texts = |items: &Vec<Clip>| items.iter().map(|c| c.text.to_string()).collect::<Vec<_>>();
        assert_eq!(texts(&items), ["three", "two", "one"]);
        push(&mut items, Clip::new(4, "one", "Other", 4), 3);
        assert_eq!(texts(&items), ["one", "three", "two"]);
        assert_eq!(items[0].id, 4);
        assert_eq!(items[0].app, "Other");
        push(&mut items, Clip::new(5, "four", "App", 5), 3);
        assert_eq!(texts(&items), ["four", "one", "three"]);
        push(&mut items, Clip::new(6, "five", "App", 6), 0);
        assert_eq!(texts(&items), ["five"], "a cap of zero still keeps the newest");
    }

    #[test]
    fn previews_and_counts() {
        let c = Clip::new(1, "\n\n  First   line \n second\tline\n\nthird\n", "Notes", 0);
        assert_eq!(c.first, "First line");
        assert_eq!(c.rest, "second line third");
        assert_eq!(c.lines, 6);
        let one = Clip::new(2, "just one", "", 0);
        assert_eq!((one.first.as_str(), one.rest.as_str(), one.lines, one.chars), ("just one", "", 1, 8));
        let long = Clip::new(3, &"x".repeat(5000), "", 0);
        assert_eq!(long.first.chars().count(), PREVIEW_CHARS);
        assert_eq!(long.chars, 5000);
    }

    #[test]
    fn kinds() {
        assert_eq!(kind_of("https://example.com/a?b=c"), Kind::Link);
        assert_eq!(kind_of("  www.example.com "), Kind::Link);
        assert_eq!(kind_of("sam@example.com"), Kind::Email);
        assert_eq!(kind_of("~/Projects/quaked"), Kind::Path);
        assert_eq!(kind_of("/usr/local/bin"), Kind::Path);
        assert_eq!(kind_of("1,284.50"), Kind::Number);
        assert_eq!(kind_of("Hello there, Sam"), Kind::Text);
        assert_eq!(kind_of("@austin"), Kind::Text);
        assert_eq!(kind_of("fn main() {\n    println!(\"hi\");\n}"), Kind::Code);
        assert_eq!(kind_of("Groceries\nmilk\neggs\nbread"), Kind::Lines);
    }

    #[test]
    fn secrets() {
        assert!(looks_secret("sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789"));
        assert!(looks_secret("ghp_0123456789abcdefghijABCDEFGHIJ012345"));
        assert!(looks_secret("  xoxb-1234567890-abcdefghijkl  "));
        assert!(looks_secret("AKIAIOSFODNN7EXAMPLE"));
        assert!(looks_secret("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U"));
        assert!(looks_secret("-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----"));
        assert!(!looks_secret("https://plcplayground.notdone.dev/?sim=ton"));
        assert!(!looks_secret("sk-short"));
        assert!(!looks_secret("Let's ship it: sk-ant is the prefix, not a key"));
        assert!(!looks_secret("AKIA is how AWS key ids start"));
        assert!(!looks_secret("cargo build --release"));
    }
}
