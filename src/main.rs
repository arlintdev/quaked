//! quaked — drives the DECOKEE Quake desk screen without DK-Suite.
//!
//!   quaked [run]                 the board (what the LaunchAgent runs); settings live in
//!                                ~/Library/Application Support/Quaked/config.json
//!   quaked hook                  Claude Code hook: records session state from stdin
//!   quaked ask                   Claude Code hook: shows Claude's questions on the Quake to answer
//!   quaked permit                Claude Code hook: permission prompts on the Quake (allow / deny)
//!   quaked hooks                 adds those hooks to ~/.claude/settings.json (install.sh runs it)
//!   quaked settings              opens the settings page (http://127.0.0.1:47380)
//!   quaked ctl <cmd>             drive the running board: turn 1|-1, press, double, hold, tap X Y;
//!                                status, frame [page] (PNG on stdout), alert <page> [why]
//!   quaked probe [secs]          prints raw HID traffic from the device
//!   quaked preview <dir>         renders every view with sample data to PNGs
//!   quaked beep                  plays the "needs you" chirp
//!   quaked luminance <0-255>     sets the screen luminance

mod app;
mod audio;
mod calendar;
mod call;
mod clipboard;
mod config;
mod deploy;
mod device;
mod digin;
mod docker;
mod focus;
mod github;
mod health;
mod http;
mod keys;
mod league;
mod menubar;
mod music;
mod night;
mod ops;
mod proto;
mod render;
mod secret;
mod status;
mod symbols;
mod sync;
mod viz;
mod web;
mod windows;

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
        comment: String::new(),
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
    let duo = |name: &str, games: u32, wins: u32, friend: bool| league::Duo { puuid: name.into(), name: name.into(), games, wins, friend };
    let lg_lobby = league::Status {
        running: true,
        phase: "Matchmaking".into(),
        summoner: Some(("Austin".into(), 187)),
        queue: Some("Ranked Solo/Duo".into()),
        search: Some((94.0, 120.0)),
        record: (61, 48),
        duos: vec![duo("Sam", 31, 19, true), duo("Priya", 22, 10, true), duo("kestrel", 9, 6, true), duo("Dmitri", 4, 1, false), duo("Lou", 3, 2, true)],
        with_now: vec![duo("Sam", 31, 19, true), duo("Priya", 22, 10, true), duo("RandomTop", 0, 0, false)],
        ..Default::default()
    };
    let lg_live = league::Status {
        phase: "InProgress".into(),
        session: (3, 1),
        streak: 2,
        live: Some(league::LiveGame {
            time: 1122.0, mode: "CLASSIC".into(), map: 11, champion: "Ahri".into(), level: 13, gold: 1350.0, kills: 4, deaths: 2, assists: 7, cs: 142,
            ally_kills: 18, enemy_kills: 11,
            events: vec![
                (190.0, "First blood: Leona".into(), 1), (400.0, "Enemy team took the Infernal drake".into(), -1),
                (612.0, "Your team took a turret".into(), 1), (840.0, "You killed Jinx".into(), 1), (1101.0, "Your team took Baron".into(), 1),
            ],
            ..Default::default()
        }),
        ..lg_lobby.clone()
    };
    let lp = |champion: &str, name: &str, mine: bool, me: bool, level: u32, k: u32, d: u32, a: u32, dead: bool| league::LivePlayer { champion: champion.into(), name: name.into(), mine, me, level, kills: k, deaths: d, assists: a, cs: 0, dead, respawn: if dead { 14.0 } else { 0.0 } };
    let players = vec![
        lp("Ahri", "Austin", true, true, 13, 4, 2, 7, false), lp("Leona", "Sam", true, false, 12, 2, 4, 14, false), lp("Jinx", "x", true, false, 14, 8, 3, 5, false),
        lp("Malphite", "Priya", true, false, 12, 1, 3, 9, true), lp("Lux", "y", true, false, 13, 3, 2, 10, false),
        lp("Zed", "", false, false, 14, 6, 4, 2, false), lp("Brand", "", false, false, 13, 2, 5, 6, true), lp("Garen", "", false, false, 12, 2, 3, 3, false),
        lp("Sona", "", false, false, 11, 0, 4, 8, false), lp("Ezreal", "", false, false, 13, 1, 2, 4, false),
    ];
    let lg_live = league::Status {
        mode: league::Mode::Rift,
        mode_name: "Ranked Solo/Duo".into(),
        mode_record: (14, 12),
        live: lg_live.live.clone().map(|g| league::LiveGame { players: players.clone(), turrets: (4, 2), dragons: (vec!["Infernal".into(), "Ocean".into()], vec!["Mountain".into()]), barons: (1, 0), heralds: (1, 0), grubs: (3, 3), ward_score: 21.0, ..g }),
        ..lg_live
    };
    let lg_dead = league::Status {
        mode: league::Mode::Mayhem,
        mode_name: "ARAM: Mayhem".into(),
        mode_record: (20, 15),
        live: lg_live.live.clone().map(|g| league::LiveGame { dead: true, respawn: 23.4, killed_by: "Jinx".into(), map: 12, mode: "KIWI".into(), ..g }),
        ..lg_live.clone()
    };
    let lg_aram = league::Status {
        live: lg_dead.live.clone().map(|g| league::LiveGame { dead: false, events: vec![(400.0, "Your team took a turret".into(), 1), (512.0, "You killed Zed".into(), 1), (601.0, "Enemy team aced".into(), -1)], turrets: (2, 1), inhibs: (0, 0), ..g }),
        ..lg_dead.clone()
    };
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
    let dg = digin::Status {
        accounts: vec![digin::Account { url: "https://digin.work".into(), label: "digin.work".into(), signed_in: true, handle: "austin".into(), name: "Austin".into(), ..Default::default() }],
        issues: vec![
            digin::Issue { id: 4512, key: "DIG-612".into(), title: "Runner pool drains when the API restarts".into(), status: "in_progress".into(), status_name: "In progress".into(), priority: 1, url: String::new(), ..Default::default() },
            digin::Issue { key: "DIG-598".into(), title: "Bot kinds: per-kind rate limits".into(), status: "todo".into(), status_name: "Todo".into(), priority: 2, ..Default::default() },
            digin::Issue { key: "KES-41".into(), title: "Wiki: drag pages between projects".into(), status: "waiting".into(), status_name: "Waiting".into(), priority: 3, ..Default::default() },
            digin::Issue { key: "DIG-577".into(), title: "Calendar invites show the wrong time zone".into(), status: "backlog".into(), status_name: "Backlog".into(), priority: 4, ..Default::default() },
        ],
        inbox: vec![
            digin::Note { id: 1, kind: "mentioned".into(), title: "Sam in #release".into(), body: "@austin can you sign off on 0.47 before 5?".into(), created: now - 4 * 60_000, ..Default::default() },
            digin::Note { id: 2, kind: "message".into(), title: "Priya".into(), body: "pushed the fix for the runner drain, take a look".into(), created: now - 22 * 60_000, ..Default::default() },
            digin::Note { id: 3, kind: "assigned".into(), title: "DIG-612 assigned to you".into(), body: "Runner pool drains when the API restarts".into(), created: now - 70 * 60_000, ..Default::default() },
        ],
        pages: vec![
            digin::DocPage { id: 77, title: "Release checklist 0.47".into(), project: "DIG".into(), updated: now - 9 * 60_000, changed: true, ..Default::default() },
            digin::DocPage { title: "Runner architecture".into(), project: "DIG".into(), updated: now - 3 * 3_600_000, changed: false, ..Default::default() },
            digin::DocPage { title: "Onboarding".into(), project: "KES".into(), updated: now - 26 * 3_600_000, changed: false, ..Default::default() },
        ],
        chat_unread: 14,
        chat_mentions: 1,
        chat_channels: 3,
        detail: None,
        reply: None,
    };
    // Digin items open on the board: an issue with "Done" waiting for its second press, an
    // inbox mention in full, a page's text.
    let dg_open = |key: &str, body: digin::Body, focus: Option<usize>, armed: bool| digin::Status {
        detail: Some(digin::Detail { key: key.into(), account: 0, body, loading: false, error: None, focus, armed: armed.then(|| (focus.unwrap_or(0), std::time::Instant::now())), busy: None, said: None }),
        ..dg.clone()
    };
    let dg_status = |id: i64, name: &str, category: &str| digin::StatusOpt { id, name: name.into(), category: category.into() };
    let dg_issue = dg_open(
        "issue:0:4512",
        digin::Body::Issue(digin::IssueView {
            id: 4512,
            key: "DIG-612".into(),
            title: "Runner pool drains when the API restarts".into(),
            status: "in_progress".into(),
            status_id: 31,
            status_name: "In progress".into(),
            priority: 1,
            project: "Digin".into(),
            assignee: "you".into(),
            updated: now - 3 * 3_600_000,
            description: digin::markdown("When the API restarts, runners lose their lease and the pool drains to zero for about two minutes. Jobs queue up behind it and the dashboard shows every runner as **offline**.\n\n## What happens\n- the API exits on `SIGTERM` without handing leases over\n- runners back off for 120 s before they register again\n\n[[steps]]\n- [x] Reproduce on staging\n- [ ] Keep leases across restarts\n\n```\nlease.renew(ttl = 30s)\n```"),
            comments: vec![
                digin::Comment { author: "Priya".into(), created: now - 22 * 60_000, text: digin::markdown("Pushed a fix on `runner-lease`: leases now survive a restart. Can you take a look before 0.47?") },
                digin::Comment { author: "Sam".into(), created: now - 64 * 60_000, text: digin::markdown("Seeing this on prod too, three times this week.") },
            ],
            comment_count: 2,
            statuses: vec![dg_status(29, "Backlog", "backlog"), dg_status(30, "Todo", "todo"), dg_status(31, "In progress", "in_progress"), dg_status(32, "In review", "waiting"), dg_status(33, "Done", "done"), dg_status(34, "Canceled", "canceled")],
            ..Default::default()
        }),
        Some(4),
        true,
    );
    let dg_note = dg_open(
        "note:0:1",
        digin::Body::Note(digin::NoteView {
            id: 1,
            kind: "mentioned".into(),
            title: "Sam in #release".into(),
            author: "Sam Okafor".into(),
            created: now - 4 * 60_000,
            text: digin::markdown("@austin can you sign off on **0.47** before 5? Everything on the [release checklist](/DIG/pages/77) is green except the runner drain.\n\n- DIG-612 has a fix on `runner-lease`, Priya says it's ready\n- staging has been up on it since this morning\n\nIf you'd rather hold, say so and I'll move the release to Thursday."),
            ..Default::default()
        }),
        None,
        false,
    );
    let dg_page = dg_open(
        "page:0:77",
        digin::Body::Page(digin::PageView {
            id: 77,
            title: "Release checklist 0.47".into(),
            project: "Digin".into(),
            updated_by: "Priya".into(),
            updated: now - 9 * 60_000,
            changed: true,
            text: digin::markdown("# Before the release\n\n1. Merge everything tagged **0.47** into `main`\n2. Run the migrations on staging and check the runner pool\n3. Write the changelog: what changed for *people*, not for code\n\n## Sign-off\n\n| Area | Owner | State |\n|---|---|---|\n| Runners | Priya | ready |\n| Calendar | Sam | ready |\n\n> Nothing ships on a Friday.\n\n## Commands\n\n```sh\n./deploy.sh --stage\n./deploy.sh --prod\n```\n\nAfter the release, watch the [dashboard](https://digin.work/ops) for an hour and post in #release."),
            ..Default::default()
        }),
        None,
        false,
    );
    let mut history = render::History::default();
    for i in 0..34u32 {
        render::History::push(&mut history.ai_steps, [0.0, 2.0, 5.0, 9.0, 4.0, 0.0, 0.0, 3.0, 12.0, 7.0, 6.0, 1.0][(i % 12) as usize]);
        render::History::push(history.docker_cpu.entry("9b62926f50e3aa".into()).or_default(), 2.0 + ((i * 37) % 9) as f32 * 0.6);
        for (url, base) in [("https://arlint.dev", 180.0), ("https://digin.work", 240.0), ("https://ibtg.notdone.dev", 120.0)] {
            render::History::push(history.site_ms.entry(url.into()).or_default(), base + ((i * 53) % 13) as f32 * 9.0);
        }
    }
    let ask_options = vec![
        ("Merge".to_string(), "Squash-merge into main and delete the branch".to_string()),
        ("Hold".to_string(), "Leave it open; I'll ping you after the demo".to_string()),
        ("Merge as draft release".to_string(), "Merge, but tag it as a pre-release".to_string()),
    ];
    let permit_options = vec![
        ("Allow".to_string(), "just this once".to_string()),
        ("Always allow".to_string(), "don't ask again for Bash(./deploy.sh:*)".to_string()),
        ("Deny".to_string(), "tell Claude no".to_string()),
    ];
    // Features: each module's sample(); the call / night overlays only on their own shots.
    let (call_on, call_off) = (call::sample(), call::Status::default());
    let (night_on, night_off) = (night::sample(), night::Status::default());
    let (focus_s, deploy_s, paste_s, windows_s, calendar_s, health_s, viz_s) = (focus::sample(), deploy::sample(), clipboard::sample(), windows::sample(), calendar::sample(), health::sample(), viz::sample());
    let shots: [(&str, Page, Level, usize, usize, usize); 34] = [
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
        ("17-ask", Page::Ai, Level::Top, 0, 0, 0),
        ("18-permit", Page::Ai, Level::Top, 0, 0, 0),
        ("19-digin", Page::Digin, Level::Focus, 0, 0, 4),
        ("20-league-live", Page::League, Level::Top, 0, 0, 0),
        ("21-league-dead", Page::League, Level::Top, 0, 0, 0),
        ("22-league-aram", Page::League, Level::Top, 0, 0, 0),
        ("30-focus", Page::Focus, Level::Focus, 0, 0, 0),
        ("31-deploy", Page::Deploy, Level::Focus, 0, 0, 0),
        ("32-paste", Page::Paste, Level::Focus, 0, 0, 0),
        ("33-windows", Page::Windows, Level::Focus, 0, 0, 0),
        ("34-calendar", Page::Calendar, Level::Focus, 0, 0, 0),
        ("35-health", Page::Health, Level::Focus, 0, 0, 0),
        ("36-call", Page::Ai, Level::Top, 0, 0, 0),
        ("37-night", Page::Dash, Level::Top, 0, 0, 0),
        ("38-music-viz", Page::Music, Level::Top, 0, 0, 0),
        ("16-settings-adjust", Page::Settings, Level::Detail("adjust".into()), 0, 0, 0),
        ("25-digin-issue", Page::Digin, Level::Detail("issue:0:4512".into()), 0, 0, 0),
        ("26-digin-note", Page::Digin, Level::Detail("note:0:1".into()), 0, 0, 4),
        ("27-digin-page", Page::Digin, Level::Detail("page:0:77".into()), 0, 0, 7),
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
            league: if name.contains("pop") { &lg_pop } else if name.contains("live") { &lg_live } else if name.contains("dead") { &lg_dead } else if name.contains("aram") { &lg_aram } else { &lg_lobby },
            league_cfg: &cfg.league,
            docker: &dk,
            ops: &opsd,
            code: &code,
            digin: match name {
                "25-digin-issue" => &dg_issue,
                "26-digin-note" => &dg_note,
                "27-digin-page" => &dg_page,
                _ => &dg,
            },
            settings,
            ring: &cfg.ring,
            call: if name == "36-call" { &call_on } else { &call_off },
            night: if name == "37-night" { &night_on } else { &night_off },
            focus: &focus_s,
            deploy: &deploy_s,
            paste: &paste_s,
            windows: &windows_s,
            calendar: &calendar_s,
            health: &health_s,
            viz: &viz_s,
            history: &history,
            ask: (name == "17-ask").then(|| render::AskView {
                session: "quaked",
                machine: "AustinM5Max",
                header: "Release",
                question: "PR #4 is green and approved. Merge it now, or hold it until after the demo?",
                options: &ask_options,
                picked: &[],
                multi: false,
                sel: 0,
                index: 0,
                count: 1,
                secs_left: 104,
                queued: 0,
                detail: None,
            }).or_else(|| (name == "18-permit").then(|| render::AskView {
                session: "plcplayground",
                machine: "",
                header: "Permission",
                question: "Claude wants to run a command  ·  Bash",
                options: &permit_options,
                picked: &[],
                multi: false,
                sel: 0,
                index: 0,
                count: 1,
                secs_left: 97,
                queued: 1,
                detail: Some(("$", "./deploy.sh --prod && ssh workbench 'docker compose -f ~/Projects/plcplayground/compose.yml up -d'", "Deploy the site to production")),
            })),
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
        "ask" => status::ask_hook(),
        "permit" => status::permit_hook(),
        "settings" | "ui" => {
            let _ = std::process::Command::new("/usr/bin/open").arg(web::url()).status();
        }
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
