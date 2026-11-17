//! quaked — drives the DECOKEE Quake desk screen without DK-Suite.
//!
//!   quaked [run] [--no-buzzer]   the board (what the LaunchAgent runs)
//!   quaked hook                  Claude Code hook: records session state from stdin
//!   quaked ctl <cmd>             drive the running board: turn 1|-1, press, double, hold, tap X Y
//!   quaked probe [secs]          prints raw HID traffic from the device
//!   quaked preview <dir>         renders every view with sample data to PNGs
//!   quaked beep                  plays the "needs you" chirp
//!   quaked luminance <0-255>     sets the screen luminance

mod app;
mod device;
mod music;
mod proto;
mod render;
mod status;

use std::io::Write;
use std::os::fd::AsRawFd;

fn single_instance() -> Option<std::fs::File> {
    let dir = status::dir();
    let lock = dir.parent().unwrap_or(&dir).join("daemon.lock");
    std::fs::create_dir_all(lock.parent()?).ok()?;
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&lock).ok()?;
    let ok = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    ok.then_some(f)
}

fn sample_sessions(now: u64) -> Vec<status::Session> {
    use status::{Session, State, Step, StepStatus};
    let s = 1000;
    let m = 60 * s;
    let step = |ago: u64, tag: &str, text: &str, status: StepStatus| Step { ts: now - ago, tag: tag.into(), text: text.into(), status, tool_id: String::new() };
    let base = |id: &str, name: &str, state: State, since: u64| Session {
        id: id.into(),
        name: name.into(),
        cwd: format!("/Users/me/ArlintDev/{name}"),
        state,
        since: now - since,
        started_at: now - 3 * 60 * m + since,
        updated_at: now,
        prompt: String::new(),
        notice: String::new(),
        last_message: String::new(),
        step_count: 0,
        steps: Vec::new(),
        transcript: String::new(),
    };
    let mut a = base("a", "digin", State::Working, 4 * m);
    a.prompt = "The publish job OOMs on workbench again, cap its memory and rerun with -p 4".into();
    a.step_count = 38;
    a.steps = vec![
        step(5 * m, "you", &a.prompt.clone(), StepStatus::Info),
        step(4 * m, "read", "scripts/publish.sh", StepStatus::Ok),
        step(3 * m, "grep", "systemd-run  in scripts", StepStatus::Ok),
        step(2 * m, "edit", "scripts/publish.sh", StepStatus::Ok),
        step(90 * s, "$", "ssh workbench 'systemd-run --user --scope -p MemoryMax=6G ./publish.sh -p 4'", StepStatus::Ok),
        step(40 * s, "$", "ssh workbench 'journalctl --user -u publish -n 50'", StepStatus::Ok),
        step(12 * s, "$", "cargo test --release -p digin-publish", StepStatus::Running),
    ];
    let mut b = base("b", "plcplayground", State::Waiting, 50 * s);
    b.prompt = "Add a timer instruction (TON) to the simulator and wire it into the toolbox".into();
    b.notice = "Claude needs your permission to use Bash".into();
    b.step_count = 12;
    b.steps = vec![
        step(6 * m, "read", "src/sim/instructions.ts", StepStatus::Ok),
        step(4 * m, "write", "src/sim/timer.ts", StepStatus::Ok),
        step(3 * m, "edit", "src/ui/Toolbox.tsx", StepStatus::Ok),
        step(60 * s, "$", "./deploy.sh", StepStatus::Running),
        step(50 * s, "needs", "Claude needs your permission to use Bash", StepStatus::Info),
    ];
    let mut c = base("c", "ibtg", State::Done, 2 * m);
    c.prompt = "Why does the shotgun pickup double-fire on mobile?".into();
    c.last_message = "Found it: the touchstart and click handlers both fire. I removed the click listener on touch devices and added a test.".into();
    c.step_count = 21;
    c.steps = vec![
        step(9 * m, "grep", "addEventListener('click'", StepStatus::Ok),
        step(7 * m, "edit", "src/input/touch.ts", StepStatus::Ok),
        step(5 * m, "$", "npm test -- touch", StepStatus::Error),
        step(4 * m, "edit", "src/input/touch.ts", StepStatus::Ok),
        step(3 * m, "$", "npm test -- touch", StepStatus::Ok),
    ];
    let mut d = base("d", "quaked", State::Working, 30 * s);
    d.prompt = "Make the interface tighter and layered, knob switches AI and music".into();
    d.step_count = 140;
    d.steps = vec![
        step(50 * s, "write", "src/render.rs", StepStatus::Ok),
        step(20 * s, "write", "src/app.rs", StepStatus::Ok),
        step(3 * s, "$", "cargo build --release", StepStatus::Running),
    ];
    vec![a, b, c, d]
}

fn preview(dir: &str) {
    use render::{Level, Page, View};
    std::fs::create_dir_all(dir).expect("create preview dir");
    let now = status::now_ms();
    let sessions = sample_sessions(now);
    let track = music::Track {
        app: "Spotify".into(),
        playing: true,
        title: "Pâine Cu Tristețe - Romanian Remix".into(),
        artist: "Wayne Johnny".into(),
        album: "Pâine Cu Tristețe (Romanian Remix)".into(),
        duration: 159.0,
        position: 86.0,
        art_url: String::new(),
    };
    let fonts = render::Fonts::load();
    let shots: [(&str, Page, Level, usize, usize); 5] = [
        ("1-ai-top", Page::Ai, Level::Top, 0, 0),
        ("2-ai-focus", Page::Ai, Level::Focus, 1, 0),
        ("3-ai-detail", Page::Ai, Level::Detail("a".into()), 0, 0),
        ("4-music-top", Page::Music, Level::Top, 0, 0),
        ("5-music-focus", Page::Music, Level::Focus, 0, 0),
    ];
    for (name, page, level, sel, scroll) in shots {
        let mut c = render::Canvas::new(1920, 480, &fonts);
        let view = View {
            page,
            level: &level,
            sessions: &sessions,
            sel,
            scroll,
            track: Some(&track),
            position: track.position,
            volume: Some(45),
            problem: None,
            art: None,
            now_ms: now,
            clock: "4:21",
            connected: true,
            flash: None,
            tick: true,
        };
        render::draw(&mut c, &view);
        let path = format!("{dir}/{name}.png");
        c.pixmap.save_png(&path).expect("write png");
        println!("wrote {path}");
    }
}

fn control_once(cmd: device::Command) {
    let (tx, rx) = std::sync::mpsc::channel();
    let tx2 = tx.clone();
    let commands = device::spawn(move |i| {
        if let device::Input::Connected { .. } = i {
            let _ = tx2.send(());
        }
    });
    if rx.recv_timeout(std::time::Duration::from_secs(5)).is_err() {
        eprintln!("Quake not found");
        std::process::exit(1);
    }
    commands.send(cmd).ok();
    std::thread::sleep(std::time::Duration::from_millis(600));
    drop(tx);
}

fn ctl(args: &[String]) {
    let line = args.join(" ");
    match std::os::unix::net::UnixStream::connect(app::ctl_socket_path()) {
        Ok(mut s) => {
            let _ = writeln!(s, "{line}");
        }
        Err(e) => {
            eprintln!("quaked is not running ({e})");
            std::process::exit(1);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let sub = args.first().map(String::as_str).unwrap_or("run");
    match sub {
        "hook" => status::hook(),
        "ctl" => ctl(&args[1..]),
        "probe" => device::probe(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(15)),
        "preview" => preview(args.get(1).map(String::as_str).unwrap_or("preview")),
        "beep" => control_once(device::Command::Tone(vec![(70, 70), (0, 60), (70, 70)])),
        "luminance" => {
            let v: u8 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or_else(|| {
                eprintln!("usage: quaked luminance <0-255>");
                std::process::exit(2)
            });
            control_once(device::Command::Luminance(v));
        }
        "run" | "--no-buzzer" => {
            let Some(_lock) = single_instance() else {
                eprintln!("quaked is already running");
                std::process::exit(0);
            };
            device::log(format!("quaked {} starting", env!("CARGO_PKG_VERSION")));
            app::run(app::Options { buzzer: !args.iter().any(|a| a == "--no-buzzer") });
        }
        _ => {
            eprintln!("usage: quaked [run [--no-buzzer] | hook | ctl <cmd> | probe [secs] | preview <dir> | beep | luminance <n>]");
            std::process::exit(2);
        }
    }
}
