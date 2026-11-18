//! Updates: new quaked releases on GitHub, and installing one on this Mac.
//!
//! The thread asks GitHub for the latest release a little after starting and then every
//! six hours (or right away when asked), and compares its version with this build's.
//!
//! Installing downloads the release's zip, checks it against the release's .sha256, unpacks
//! it, and checks that Quaked.app and Quaked Settings.app inside are signed by the same
//! Developer ID team as the quaked that's running (Apple-issued certificate, our bundle ids)
//! and carry the release's version. Then it swaps both apps in next to the running one,
//! updates the Claude Code hooks with the new binary, and has launchd restart quaked.
//! Nothing unsigned from the zip runs (its install.sh is for installing by hand).

use crate::config::UpdateSettings as Cfg;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// Where releases come from.
pub const REPO: &str = "arlintdev/quaked";
/// This build's version (Cargo.toml; a release's tag is "v" + it).
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

const FIRST_CHECK: Duration = Duration::from_secs(20);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
const APP_ID: &str = "dev.arlint.quaked";
const SETTINGS_ID: &str = "dev.arlint.quaked.settings";

/// The latest release on GitHub.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Release {
    /// "0.4.0"
    pub version: String,
    pub name: String,
    /// Its notes (Markdown).
    pub notes: String,
    /// Unix ms.
    pub published_ms: u64,
    /// Its page on GitHub.
    pub url: String,
    /// The Apple Silicon zip and its checksum file ("" if the release has none).
    pub zip_url: String,
    pub zip_size: u64,
    pub sha_url: String,
}

/// Installing, step by step.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Phase {
    #[default]
    Idle,
    Downloading,
    Verifying,
    Installing,
    /// Installed; launchd is restarting quaked (this process ends here).
    Restarting,
    Failed(String),
}

impl Phase {
    /// "Downloading 0.4.0…" for the board.
    pub fn label(&self, version: &str) -> Option<String> {
        match self {
            Phase::Idle | Phase::Failed(_) => None,
            Phase::Downloading => Some(format!("Downloading {version}…")),
            Phase::Verifying => Some("Checking the signature…".into()),
            Phase::Installing => Some("Installing…".into()),
            Phase::Restarting => Some("Restarting quaked…".into()),
        }
    }
}

/// What the board and the settings page show.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub latest: Option<Release>,
    /// When GitHub last answered (unix ms; 0 = not yet).
    pub checked_ms: u64,
    pub checking: bool,
    /// Why the last check failed.
    pub error: Option<String>,
    pub phase: Phase,
}

impl Status {
    /// The latest release when it's newer than this build.
    pub fn available(&self) -> Option<&Release> {
        self.latest.as_ref().filter(|r| newer(&r.version, CURRENT))
    }

    pub fn busy(&self) -> bool {
        !matches!(self.phase, Phase::Idle | Phase::Failed(_))
    }
}

/// "1.2.3" (or "v1.2.3", "1.2.3-beta") as numbers.
fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    let t = (it.next()??, it.next().unwrap_or(Some(0))?, it.next().unwrap_or(Some(0))?);
    it.next().is_none().then_some(t)
}

/// Is version `a` newer than `b`? (Unreadable versions never are.)
pub fn newer(a: &str, b: &str) -> bool {
    matches!((parse(a), parse(b)), (Some(x), Some(y)) if x > y)
}

#[derive(Debug)]
pub enum Cmd {
    Configure(Cfg),
    /// Ask GitHub now.
    Check,
    /// Install the latest release on this Mac (checks first if it hasn't yet).
    Install,
    /// Ask a paired Mac to install it: (name, host, port, key).
    InstallOn(String, String, u16, String),
    /// (internal) It answered.
    Peer(String, Result<(), String>),
}

/// What the thread tells the app.
#[derive(Debug)]
pub enum Update {
    Status(Status),
    /// A paired Mac answered Cmd::InstallOn: (name, Ok or why not).
    Peer(String, Result<(), String>),
}

/// Starts the module's background work; `emit` hands updates to the app.
pub fn spawn(cfg: Cfg, emit: impl Fn(Update) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    let back = tx.clone();
    std::thread::Builder::new().name("update".into()).spawn(move || watch(cfg, rx, back, emit)).unwrap();
    tx
}

fn watch(mut cfg: Cfg, rx: Receiver<Cmd>, back: Sender<Cmd>, emit: impl Fn(Update)) {
    let mut st = Status::default();
    let mut next = Instant::now() + FIRST_CHECK;
    loop {
        let wait = if cfg.check { next.saturating_duration_since(Instant::now()) } else { Duration::from_secs(3600) };
        let mut check = false;
        let mut install = false;
        match rx.recv_timeout(wait) {
            Ok(Cmd::Configure(c)) => {
                if c.check && !cfg.check {
                    next = Instant::now();
                }
                cfg = c;
            }
            Ok(Cmd::Check) => check = true,
            Ok(Cmd::Install) => install = true,
            Ok(Cmd::InstallOn(name, host, port, key)) => {
                let back = back.clone();
                std::thread::Builder::new()
                    .name("update-peer".into())
                    .spawn(move || {
                        let done = crate::sync::request(&host, port, &key, "ctl update-install").and_then(|r| peer_reply(&r));
                        let _ = back.send(Cmd::Peer(name, done));
                    })
                    .ok();
            }
            Ok(Cmd::Peer(name, done)) => emit(Update::Peer(name, done)),
            Err(RecvTimeoutError::Timeout) => check = cfg.check && Instant::now() >= next,
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if check || (install && st.latest.is_none()) {
            st.checking = true;
            emit(Update::Status(st.clone()));
            match latest() {
                Ok(r) => {
                    if st.latest.as_ref().map(|l| &l.version) != Some(&r.version) {
                        crate::device::log(format!("update: latest release is {} (this is {CURRENT})", r.version));
                    }
                    st.latest = Some(r);
                    st.error = None;
                    st.checked_ms = crate::status::now_ms();
                }
                Err(e) => {
                    crate::device::log(format!("update: check failed: {e}"));
                    st.error = Some(e);
                }
            }
            st.checking = false;
            next = Instant::now() + CHECK_EVERY;
            emit(Update::Status(st.clone()));
        }
        if install && !st.busy() {
            match st.available().cloned() {
                Some(r) => install_release(&r, &mut st, &emit),
                None => {
                    st.phase = Phase::Failed(if st.latest.is_some() { "Already up to date".into() } else { st.error.clone().unwrap_or_else(|| "No release found".into()) });
                    emit(Update::Status(st.clone()));
                }
            }
        }
    }
}

/// A paired Mac's answer to `ctl update-install`.
pub fn peer_reply(reply: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(reply);
    let text = text.trim();
    if text == "ok" {
        Ok(())
    } else if text.starts_with("error: unknown command") {
        Err("its quaked is too old to update itself; update it by hand once".into())
    } else {
        Err(text.strip_prefix("error: ").unwrap_or(text).to_string())
    }
}

// ---------------------------------------------------------------- GitHub

fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    let ua = format!("quaked/{CURRENT}");
    let out = Command::new("/usr/bin/curl").args(["-fsSL", "-A", &ua]).args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.trim().trim_start_matches("curl: ").chars().take(120).collect())
    }
}

/// The latest release (GitHub leaves out drafts and pre-releases). QUAKED_UPDATE_FEED (a
/// URL or file:// path to a release's JSON, as GitHub's API writes it) stands in for GitHub,
/// to try a release before publishing it; what it points at is checked the same way.
fn latest() -> Result<Release, String> {
    let feed = std::env::var("QUAKED_UPDATE_FEED").ok().filter(|f| !f.is_empty()).unwrap_or_else(|| format!("https://api.github.com/repos/{REPO}/releases/latest"));
    let body = curl(&["--max-time", "15", "-H", "Accept: application/vnd.github+json", &feed])
        .map_err(|e| if e.contains("404") { "No releases yet".to_string() } else { format!("GitHub: {e}") })?;
    parse_release(&body)
}

/// A GitHub release (the API's JSON) as a Release.
pub fn parse_release(body: &[u8]) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|_| "GitHub sent something unreadable".to_string())?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let tag = s("tag_name");
    let version = tag.trim_start_matches('v').to_string();
    if parse(&version).is_none() {
        return Err(format!("Unexpected release tag {tag:?}"));
    }
    let assets = v.get("assets").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    let name_of = |a: &serde_json::Value| a.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
    let zip = assets.iter().find(|a| name_of(a).ends_with("-macos-arm64.zip"));
    let zip_name = zip.map(name_of).unwrap_or_default();
    let url_of = |a: &serde_json::Value| a.get("browser_download_url").and_then(|u| u.as_str()).unwrap_or("").to_string();
    Ok(Release {
        name: s("name"),
        notes: s("body"),
        published_ms: crate::github::iso_ms(&s("published_at")),
        url: s("html_url"),
        zip_url: zip.map(url_of).unwrap_or_default(),
        zip_size: zip.and_then(|a| a.get("size")).and_then(|x| x.as_u64()).unwrap_or(0),
        sha_url: assets.iter().find(|a| !zip_name.is_empty() && name_of(a) == format!("{zip_name}.sha256")).map(url_of).unwrap_or_default(),
        version,
    })
}

// ---------------------------------------------------------------- installing

/// A release downloaded and checked, ready to swap in.
struct Verified {
    dir: PathBuf,
    app: PathBuf,
    settings: PathBuf,
    /// The Quaked.app running now (the new one goes in its place).
    running: PathBuf,
}

/// Downloads `r` and checks it (checksum, signatures against the running app's team,
/// version); `step` hears each phase.
fn fetch_and_verify(r: &Release, step: &mut dyn FnMut(Phase)) -> Result<Verified, String> {
    if std::env::consts::ARCH != "aarch64" {
        return Err("Releases are for Apple Silicon Macs".into());
    }
    if r.zip_url.is_empty() || r.sha_url.is_empty() {
        return Err("The release has no Apple Silicon zip and checksum".into());
    }
    let running = running_app()?;
    let team = team_of(&running)?;
    step(Phase::Downloading);
    let dir = cache_dir().join(&r.version);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let zip = dir.join("quaked.zip");
    curl(&["--max-time", "600", "-o", &zip.to_string_lossy(), &r.zip_url]).map_err(|e| format!("Download failed: {e}"))?;
    let want = String::from_utf8_lossy(&curl(&["--max-time", "30", &r.sha_url]).map_err(|e| format!("Checksum download failed: {e}"))?).split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    step(Phase::Verifying);
    if want.len() != 64 || sha256(&zip)? != want {
        return Err("The download doesn't match the release's checksum".into());
    }
    let unpacked = dir.join("unpacked");
    run("/usr/bin/ditto", &["-x", "-k", &zip.to_string_lossy(), &unpacked.to_string_lossy()])?;
    let folder = std::fs::read_dir(&unpacked).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).find(|p| p.join("Quaked.app").is_dir()).ok_or("The zip has no Quaked.app")?;
    let (app, settings) = (folder.join("Quaked.app"), folder.join("Quaked Settings.app"));
    verify(&app, APP_ID, &team)?;
    verify(&settings, SETTINGS_ID, &team)?;
    let version = run("/usr/bin/plutil", &["-extract", "CFBundleShortVersionString", "raw", &app.join("Contents/Info.plist").to_string_lossy()])?;
    if version.trim() != r.version {
        return Err(format!("The app inside says {}, not {}", version.trim(), r.version));
    }
    Ok(Verified { dir, app, settings, running })
}

/// `quaked update verify`: download the latest release and check it, without installing.
pub fn verify_latest() -> Result<String, String> {
    let r = latest()?;
    let v = fetch_and_verify(&r, &mut |p| {
        if let Some(l) = p.label(&r.version) {
            println!("{l}");
        }
    })?;
    let _ = std::fs::remove_dir_all(&v.dir);
    Ok(format!("quaked {} checks out: checksum matches, both apps signed by this quaked's team (this is {CURRENT})", r.version))
}

fn install_release(r: &Release, st: &mut Status, emit: &impl Fn(Update)) {
    let step = |p: Phase, st: &mut Status| {
        st.phase = p;
        emit(Update::Status(st.clone()));
    };
    crate::device::log(format!("update: installing {} over {CURRENT}", r.version));
    let result = (|| -> Result<(), String> {
        let Verified { dir, app, settings, running } = fetch_and_verify(r, &mut |p| step(p, st))?;
        step(Phase::Installing, st);
        let apps = running.parent().ok_or("Can't tell where Quaked.app lives")?;
        swap_in(&settings, &apps.join("Quaked Settings.app"))?;
        swap_in(&app, &running)?;
        let _ = std::fs::remove_dir_all(&dir);
        // The hooks, as the new build writes them.
        if let Err(e) = run(&running.join("Contents/MacOS/quaked").to_string_lossy(), &["hooks"]) {
            crate::device::log(format!("update: hooks: {e}"));
        }
        let marker = serde_json::json!({ "from": CURRENT, "to": r.version, "at": crate::status::now_ms() });
        let _ = std::fs::write(marker_path(), marker.to_string());
        if std::env::var("XPC_SERVICE_NAME").ok().as_deref() != Some(APP_ID) {
            return Err(format!("Installed {}; quit quaked and start it again to finish", r.version));
        }
        step(Phase::Restarting, st);
        crate::device::log(format!("update: installed {}, restarting", r.version));
        extern "C" {
            fn getuid() -> u32;
        }
        run("/bin/launchctl", &["kickstart", "-k", &format!("gui/{}/{APP_ID}", unsafe { getuid() })])?;
        // launchd ends this process; if it's still here a moment later, say so.
        std::thread::sleep(Duration::from_secs(10));
        Err(format!("Installed {}, but quaked didn't restart; restart it from the menu bar", r.version))
    })();
    if let Err(e) = result {
        crate::device::log(format!("update: {e}"));
        step(Phase::Failed(e), st);
    }
}

/// Runs a program and returns its output (stdout), or why it failed.
fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program).args(args).output().map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let name = Path::new(program).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Err(format!("{name}: {}", String::from_utf8_lossy(&out.stderr).trim().chars().take(160).collect::<String>()))
    }
}

fn sha256(path: &Path) -> Result<String, String> {
    let out = run("/usr/bin/shasum", &["-a", "256", &path.to_string_lossy()])?;
    Ok(out.split_whitespace().next().unwrap_or("").to_ascii_lowercase())
}

/// The Quaked.app this process runs from.
fn running_app() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().and_then(|p| p.canonicalize()).map_err(|e| e.to_string())?;
    let app = exe.parent().and_then(Path::parent).and_then(Path::parent).filter(|a| a.extension().is_some_and(|x| x == "app")).ok_or("quaked isn't running from Quaked.app; install the release by hand")?;
    Ok(app.to_path_buf())
}

/// The Developer ID team that signed `app`.
fn team_of(app: &Path) -> Result<String, String> {
    let out = Command::new("/usr/bin/codesign").args(["-dv", "--verbose=2"]).arg(app).output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stderr);
    text.lines()
        .find_map(|l| l.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|t| !t.is_empty() && *t != "not set" && t.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(str::to_string)
        .ok_or_else(|| "This quaked isn't signed with a Developer ID, so a download can't be checked against it; install the release by hand".into())
}

/// `app` is intact, signed with an Apple-issued certificate of `team`, and is `id`.
fn verify(app: &Path, id: &str, team: &str) -> Result<(), String> {
    let req = format!("=anchor apple generic and identifier \"{id}\" and certificate leaf[subject.OU] = \"{team}\"");
    let name = app.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    run("/usr/bin/codesign", &["--verify", "--strict", "--deep", "-R", &req, &app.to_string_lossy()]).map(|_| ()).map_err(|_| format!("{name} in the release isn't signed by the same developer as this quaked"))
}

/// Puts a copy of `src` at `dest`, replacing what's there in one rename.
fn swap_in(src: &Path, dest: &Path) -> Result<(), String> {
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or("bad path")?;
    let tmp = dest.with_file_name(format!(".{name}.new"));
    let old = dest.with_file_name(format!(".{name}.old"));
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(&old);
    run("/usr/bin/ditto", &[&src.to_string_lossy(), &tmp.to_string_lossy()])?;
    if dest.exists() {
        std::fs::rename(dest, &old).map_err(|e| format!("{name}: {e}"))?;
    }
    if let Err(e) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::rename(&old, dest);
        return Err(format!("{name}: {e}"));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

fn cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join("Library/Caches/Quaked/update")
}

/// Written just before the restart, read by the new build to say how it went.
fn marker_path() -> PathBuf {
    crate::config::dir().join("updated.json")
}

/// After an update this build installed: Ok(new version) when this is it, Err(the version
/// it meant to install) when it isn't. Once (the note is removed).
pub fn just_updated() -> Option<Result<String, String>> {
    let text = std::fs::read_to_string(marker_path()).ok()?;
    let _ = std::fs::remove_file(marker_path());
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let to = v.get("to")?.as_str()?.to_string();
    // A note from long ago (a crash before the restart) says nothing.
    let at = v.get("at").and_then(|a| a.as_u64()).unwrap_or(0);
    if crate::status::now_ms().saturating_sub(at) > 15 * 60_000 {
        return None;
    }
    Some(if to == CURRENT { Ok(to) } else { Err(to) })
}

/// For the preview: a release newer than any build, with notes.
pub fn sample() -> Status {
    Status {
        latest: Some(Release {
            version: "9.1.0".into(),
            name: "quaked 9.1.0 (Apple Silicon)".into(),
            notes: "**New:** an Update pane that shows up when a release is out, and installs it on this Mac or the paired one.\n\n- Macs sit on the Quake's left or right: notices and meetings point at theirs\n- Meetings join on the Mac they came from\n- Stocks show pre-market and after hours\n- Music has a shuffle button".into(),
            published_ms: crate::status::now_ms() - 2 * 3_600_000,
            url: format!("https://github.com/{REPO}/releases/tag/v9.1.0"),
            zip_url: "https://example.invalid/quaked-9.1.0-macos-arm64.zip".into(),
            zip_size: 2_812_000,
            sha_url: "https://example.invalid/quaked-9.1.0-macos-arm64.zip.sha256".into(),
        }),
        checked_ms: crate::status::now_ms(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert!(newer("0.4.0", "0.3.0"));
        assert!(newer("v0.10.0", "0.9.9"));
        assert!(newer("1.0", "0.99.1"));
        assert!(!newer("0.3.0", "0.3.0"));
        assert!(!newer("0.3.0", "0.4.0"));
        assert!(!newer("0.4.0-beta", "0.4.0"));
        assert!(!newer("garbage", "0.1.0"));
        assert!(!newer("0.4.0.1", "0.1.0"));
    }

    #[test]
    fn reads_a_github_release() {
        let body = br#"{"tag_name":"v0.4.0","name":"quaked 0.4.0 (Apple Silicon)","body":"Notes","html_url":"https://github.com/arlintdev/quaked/releases/tag/v0.4.0","published_at":"2026-11-18T20:00:00Z",
            "assets":[{"name":"quaked-0.4.0-macos-arm64.zip.sha256","size":100,"browser_download_url":"https://x/z.sha256"},{"name":"quaked-0.4.0-macos-arm64.zip","size":2800000,"browser_download_url":"https://x/z.zip"}]}"#;
        let r = parse_release(body).unwrap();
        assert_eq!(r.version, "0.4.0");
        assert_eq!(r.zip_url, "https://x/z.zip");
        assert_eq!(r.sha_url, "https://x/z.sha256");
        assert_eq!(r.zip_size, 2_800_000);
        assert!(r.published_ms > 0);
        assert!(parse_release(br#"{"tag_name":"nightly"}"#).is_err());
        let st = Status { latest: Some(Release { version: "999.0.0".into(), ..r.clone() }), ..Default::default() };
        assert!(st.available().is_some());
        let st = Status { latest: Some(Release { version: CURRENT.into(), ..r }), ..Default::default() };
        assert!(st.available().is_none());
    }

    #[test]
    fn swaps_an_app_in_place() {
        let root = std::env::temp_dir().join(format!("quaked-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (src, dest) = (root.join("new/Thing.app"), root.join("apps/Thing.app"));
        for (dir, text) in [(&src, "new"), (&dest, "old")] {
            std::fs::create_dir_all(dir.join("Contents")).unwrap();
            std::fs::write(dir.join("Contents/v"), text).unwrap();
        }
        std::fs::write(dest.join("Contents/stale"), "x").unwrap();
        swap_in(&src, &dest).unwrap();
        assert_eq!(std::fs::read_to_string(dest.join("Contents/v")).unwrap(), "new");
        assert!(!dest.join("Contents/stale").exists());
        // Nothing left over beside it.
        let names: Vec<String> = std::fs::read_dir(root.join("apps")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["Thing.app"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn peer_answers() {
        assert_eq!(peer_reply(b"ok\n"), Ok(()));
        assert_eq!(peer_reply(b"error: Already up to date\n"), Err("Already up to date".into()));
        assert!(peer_reply(b"error: unknown command \"update-install\"\n").unwrap_err().contains("by hand"));
    }
}
