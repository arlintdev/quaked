//! quaked — drives the DECOKEE Quake desk screen without DK-Suite.
//!
//!   quaked [run]                 the board (what the LaunchAgent runs); settings live in
//!                                ~/Library/Application Support/Quaked/config.json
//!   quaked hook                  Claude Code hook: records session state from stdin
//!   quaked hooks                 adds those hooks to ~/.claude/settings.json (install.sh runs it)
//!   quaked ctl <cmd>             drive the running board: turn 1|-1, press, double, hold, tap X Y;
//!                                status, frame [page] (PNG on stdout), alert <page> [why]
//!   quaked probe [secs]          prints raw HID traffic from the device
//!   quaked preview <dir>         renders every view with sample data to PNGs
//!   quaked beep                  plays the "needs you" chirp
//!   quaked luminance <0-255>     sets the screen luminance

mod app;
mod audio;
mod config;
mod device;
mod docker;
mod github;
mod http;
mod keys;
mod league;
mod music;
mod ops;
mod proto;
mod render;
mod status;
mod symbols;
mod sync;

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
        machine: String::new(),
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
    c.machine = "MacBook Pro".into();
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
    let symbols = symbols::Symbols::default();
    let cfg = config::Config::default();
    let clip = |t: &str, color: &str, symbol: &str| audio::ClipInfo { title: t.into(), path: Default::default(), color: color.into(), symbol: symbol.into(), gain: 1.0 };
    let sounds = audio::Status {
        clips: vec![
            clip("Airhorn", "#FF9F0A", "megaphone.fill"),
            clip("Rimshot", "#4C8DFF", ""),
            clip("Sad trombone", "#B36BFF", "music.note"),
            clip("Applause", "#2F9E5B", "hands.clap.fill"),
            clip("Crickets", "#8C8C96", "ant.fill"),
            clip("Bruh", "#FF6B6B", ""),
            clip("Drumroll please, this title is long enough to wrap", "#E6B673", ""),
        ],
        playing: vec![0],
        live: true,
        listener: Some("us.zoom.xos".into()),
        mic_muted: false,
        error: None,
    };
    let lg_lobby = league::Status { running: true, phase: "Matchmaking".into(), summoner: Some(("Austin".into(), 187)), queue: Some("Ranked Solo/Duo".into()), search: Some((94.0, 120.0)), ..Default::default() };
    let lg_pop = league::Status { phase: "ReadyCheck".into(), ready: Some(league::ReadyCheck { state: "InProgress".into(), response: "Accepted".into(), timer: 3.0 }), accepted_by_quaked: true, ..lg_lobby.clone() };
    let dk = docker::Status {
        ok: true,
        socket: "~/.kiri/run/docker.sock".into(),
        engine: "Kiri 0.2.0".into(),
        ncpu: 2,
        mem_total: 8_408_313_856,
        images: 13,
        volumes: 16,
        containers: vec![
            docker::Container { id: "9b62926f50e3aa".into(), name: "digin-pg-dev".into(), image: "postgres:17-alpine".into(), state: "running".into(), status: "Up 2 days".into(), ports: vec!["55432→5432".into()], cpu: Some(3.4), mem: Some(109_559_808), mem_limit: Some(8_589_934_592) },
            docker::Container { id: "1a2b3c4d5e6f77".into(), name: "buildx_buildkit_kiri-build0".into(), image: "moby/buildkit:buildx-stable-1".into(), state: "paused".into(), status: "Up 2 days (Paused)".into(), ..Default::default() },
            docker::Container { id: "77aa88bb99cc00".into(), name: "digin-dev-db-tls".into(), image: "postgres:17".into(), state: "exited".into(), status: "Exited (0) 2 days ago".into(), ..Default::default() },
        ],
        logs: Some(("9b62926f50e3aa".into(), (1..40).map(|i| format!("2026-11-16 17:{i:02}:00 UTC [1] LOG:  checkpoint complete: wrote {i} buffers")).chain(["ERROR:  relation \"missing\" does not exist".to_string()]).collect())),
        error: None,
    };
    let host = |name: &str, address: &str, ms: Option<u32>| ops::Host { name: name.into(), address: address.into(), up: ms.is_some(), latency_ms: ms, ..Default::default() };
    let opsd = ops::Status {
        net: ops::Network { warp: Some(true), appgate: true, ip: "104.28.40.17".into(), colo: "SLC".into() },
        hosts: vec![
            ops::Host { load: Some(1.84), cpus: Some(16), mem: Some((12_884_901_888, 33_554_432_000)), disk: Some((1_210_000_000_000, 1_900_000_000_000)), uptime: "3 weeks, 2 days, 4 hours".into(), ..host("workbench", "workbench", Some(23)) },
            ops::Host { load: Some(7.9), cpus: Some(8), mem: Some((58_000_000_000, 64_000_000_000)), disk: Some((400_000_000_000, 1_000_000_000_000)), uptime: "11 days, 2 hours".into(), gpu: Some((87.0, 21_400.0, 24_576.0, 71.0)), ..host("shebash-gpu", "dev.d3d.dev", Some(41)) },
            host("spark.d3d.dev", "spark.d3d.dev", None),
            ops::Host { stats_error: Some("Permission denied (publickey,password)".into()), ..host("pi", "192.168.68.72", Some(4)) },
        ],
        sites: vec![
            ops::Site { url: "https://arlint.dev".into(), code: Some(200), ms: Some(182), tls_days: Some(54), error: None },
            ops::Site { url: "https://digin.work".into(), code: Some(200), ms: Some(240), tls_days: Some(71), error: None },
            ops::Site { url: "https://ibtg.notdone.dev".into(), code: Some(502), ms: Some(95), tls_days: Some(9), error: None },
            ops::Site { url: "https://plcplayground.notdone.dev".into(), code: None, ms: None, tls_days: None, error: Some("Could not resolve host".into()) },
        ],
    };
    let pr = |repo: &str, number: u64, title: &str, ci: &str, review: &str, mins: u64| github::Pr {
        repo: repo.into(),
        number,
        title: title.into(),
        url: format!("https://github.com/{repo}/pull/{number}"),
        ci: ci.into(),
        review: review.into(),
        mergeable: "MERGEABLE".into(),
        updated: now - mins * 60_000,
        ..Default::default()
    };
    let code = github::Status {
        login: "aarlint".into(),
        mine: vec![
            pr("shebashio/thestack", 412, "Move the ingest workers onto the shared job queue", "FAILURE", "REVIEW_REQUIRED", 14),
            pr("arlintdev/digin", 288, "Bot kinds: hands mode for runners", "SUCCESS", "APPROVED", 95),
            github::Pr { mergeable: "CONFLICTING".into(), ..pr("arlintdev/plcplayground", 61, "TON / TOF timers in the simulator", "PENDING", "", 300) },
            github::Pr { draft: true, ..pr("arlintdev/quaked", 3, "OPS and CODE pages", "", "", 2) },
            pr("shebashio/thestack", 405, "Bump tokio and hyper", "SUCCESS", "CHANGES_REQUESTED", 2 * 1440),
        ],
        reviews: vec![
            github::Pr { author: "kestrel-dev".into(), ..pr("shebashio/thestack", 418, "Retry S3 uploads with jitter", "SUCCESS", "", 40) },
            github::Pr { author: "jdoe".into(), gitlab: true, ..pr("platform/infra", 92, "Pin the runner image digest", "PENDING", "", 75) },
        ],
        error: None,
        ok: true,
        gitlab_hosts: vec!["gitlab.com".into()],
        gitlab_ok: true,
        gitlab_error: None,
    };
    let settings = render::SettingsView { screen: 200, ring: 169, idle: 45, idle_home: true, ring_on: true, alert: true, buzzer: true, mic: false, auto_accept: true, snap: true, blink: true, jump: true };
    let shots: [(&str, Page, Level, usize, usize, usize); 16] = [
        ("1-ai-top", Page::Ai, Level::Top, 0, 0, 0),
        ("2-ai-focus", Page::Ai, Level::Focus, 1, 0, 0),
        ("3-ai-detail", Page::Ai, Level::Detail("a".into()), 0, 0, 0),
        ("4-music-top", Page::Music, Level::Top, 0, 0, 0),
        ("5-music-focus", Page::Music, Level::Focus, 0, 0, 0),
        ("6-shortcuts", Page::Shortcuts, Level::Focus, 0, 0, 1),
        ("7-sounds", Page::Soundboard, Level::Focus, 0, 0, 2),
        ("8-sounds-empty", Page::Soundboard, Level::Top, 0, 0, 0),
        ("9-league-queue", Page::League, Level::Top, 0, 0, 0),
        ("10-league-pop", Page::League, Level::Focus, 0, 0, 0),
        ("11-docker", Page::Docker, Level::Focus, 0, 0, 0),
        ("13-ops", Page::Ops, Level::Focus, 0, 0, 2),
        ("14-code", Page::Code, Level::Focus, 0, 0, 1),
        ("15-settings", Page::Settings, Level::Focus, 0, 0, 6),
        ("0-dash", Page::Dash, Level::Top, 0, 0, 0),
        ("16-settings-adjust", Page::Settings, Level::Detail("adjust".into()), 0, 0, 0),
    ];
    let detail_shot = ("12-docker-detail", Page::Docker, Level::Detail("9b62926f50e3aa".into()), 0, 0, 0);
    let empty = audio::Status::default();
    for (name, page, level, sel, scroll, tile) in shots.into_iter().chain(std::iter::once(detail_shot)) {
        let mut c = render::Canvas::new(1920, 480, &fonts, &symbols);
        let view = View {
            pages: &cfg.pages,
            page,
            accent: config::hex(cfg.ring.color_for(page), 0),
            shortcuts: &cfg.shortcuts,
            sounds: if name.ends_with("empty") { &empty } else { &sounds },
            tile,
            league: if name.contains("pop") { &lg_pop } else { &lg_lobby },
            league_cfg: &cfg.league,
            docker: &dk,
            ops: &opsd,
            code: &code,
            settings,
            ring: &cfg.ring,
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
            notice: if name == "14-code" { Some(("review requested  ·  infra !92  Pin the runner image digest", config::hex(cfg.ring.color_for(Page::Code), 0), Page::Code)) } else { None },
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
    use std::io::Read;
    let line = args.join(" ");
    match std::os::unix::net::UnixStream::connect(app::ctl_socket_path()) {
        Ok(mut s) => {
            let _ = writeln!(s, "{line}");
            let mut reply = Vec::new();
            let _ = s.read_to_end(&mut reply);
            let _ = std::io::stdout().write_all(&reply);
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
        "hooks" => match status::install_hooks() {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!("quaked hooks: {e}");
                std::process::exit(1);
            }
        },
        "ctl" => ctl(&args[1..]),
        "via" => device::via_probe(),
        "symbol" => {
            // Debug: render an SF Symbol to PNG.
            let name = args.get(1).cloned().unwrap_or_else(|| "sparkles".into());
            let s = symbols::Symbols::default();
            match s.get(&name, 96.0) {
                Some(m) => {
                    let mut p = tiny_skia::Pixmap::new(m.w, m.h).unwrap();
                    for (i, a) in m.alpha.iter().enumerate() {
                        let px = &mut p.data_mut()[i * 4..i * 4 + 4];
                        px.copy_from_slice(&[*a, *a, *a, 255]);
                    }
                    let out = args.get(2).cloned().unwrap_or_else(|| format!("{name}.png"));
                    p.save_png(&out).unwrap();
                    println!("{name}: {}x{} -> {out}", m.w, m.h);
                }
                None => println!("{name}: not an SF Symbol"),
            }
        }
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
        "run" => {
            let Some(_lock) = single_instance() else {
                eprintln!("quaked is already running");
                std::process::exit(0);
            };
            device::log(format!("quaked {} starting", env!("CARGO_PKG_VERSION")));
            app::run();
        }
        _ => {
            eprintln!("usage: quaked [run | hook | ctl <cmd> | probe [secs] | via | preview <dir> | symbol <name> | beep | luminance <n>]");
            std::process::exit(2);
        }
    }
}
