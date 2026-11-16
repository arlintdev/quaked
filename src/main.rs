//! quaked — drives the DECOKEE Quake desk screen without DK-Suite.
//!
//!   quaked [run] [--no-buzzer]   the board (what the LaunchAgent runs)
//!   quaked hook                  Claude Code hook: records session state from stdin
//!   quaked probe [secs]          prints raw HID traffic from the device
//!   quaked preview <out.png>     renders the board with sample sessions
//!   quaked beep                  plays the "needs you" chirp
//!   quaked luminance <0-255>     sets the screen luminance

mod app;
mod device;
mod proto;
mod render;
mod status;

use std::os::fd::AsRawFd;

fn single_instance() -> Option<std::fs::File> {
    let dir = status::dir();
    let lock = dir.parent().unwrap_or(&dir).join("daemon.lock");
    std::fs::create_dir_all(lock.parent()?).ok()?;
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&lock).ok()?;
    let ok = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    ok.then_some(f)
}

fn preview(out: &str) {
    use status::{Session, State};
    let now = status::now_ms();
    let m = 60_000;
    let s = |name: &str, state, since_min: u64, start: u64, detail: &str| Session {
        id: name.into(),
        name: name.into(),
        cwd: String::new(),
        state,
        since: now - since_min * m,
        started_at: start,
        updated_at: now,
        detail: detail.into(),
    };
    let sessions = vec![
        s("digin", State::Working, 4, 1, "Bash"),
        s("plcplayground", State::Waiting, 1, 2, ""),
        s("ibtg", State::Done, 2, 3, ""),
        s("quaked", State::Working, 75, 4, "Edit"),
        s("cacrelay", State::Idle, 300, 5, ""),
        s("mesh-remote", State::Done, 0, 6, ""),
    ];
    let fonts = render::Fonts::load();
    let mut c = render::Canvas::new(1920, 480, &fonts);
    render::draw(&mut c, &render::Board { sessions: &sessions, now_ms: now, clock: "4:21".into(), connected: true });
    c.pixmap.save_png(out).expect("write png");
    println!("wrote {out}");
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let sub = args.first().map(String::as_str).unwrap_or("run");
    match sub {
        "hook" => status::hook(),
        "probe" => device::probe(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(15)),
        "preview" => preview(args.get(1).map(String::as_str).unwrap_or("preview.png")),
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
            eprintln!("usage: quaked [run [--no-buzzer] | hook | probe [secs] | preview <out.png> | beep | luminance <n>]");
            std::process::exit(2);
        }
    }
}
