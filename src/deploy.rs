//! SHIP page data: your deployable projects (git branch, ahead / behind, dirty files, last
//! commit), their latest GitHub Actions run through the `gh` CLI's existing login (with the
//! live job and step while one runs), and the deploy runner.
//!
//! A deploy only ever starts from `Cmd::Deploy`, which the app sends after the two-step
//! confirmation on the board (arm, then press again within a few seconds). It runs the
//! project's command with `/bin/sh -c` in the project folder, streams stdout + stderr back
//! line by line, and can be cancelled (the whole process group gets SIGTERM, then SIGKILL).
//! One deploy at a time.

use crate::config::{DeployProject, DeploySettings as Cfg};
use crate::device::log;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

const GH: [&str; 2] = ["/opt/homebrew/bin/gh", "/usr/local/bin/gh"];
const GIT: [&str; 3] = ["/opt/homebrew/bin/git", "/usr/local/bin/git", "/usr/bin/git"];
/// Lines of deploy output kept (and sent to the board).
pub const LOG_LINES: usize = 300;
/// Live output reaches the board at most this often.
const EMIT_EVERY: Duration = Duration::from_millis(250);
/// After the shell exits, wait this long for its last lines before calling it done.
const SETTLE: Duration = Duration::from_millis(300);
/// After SIGTERM, how long a cancelled deploy gets before SIGKILL.
const KILL_AFTER: Duration = Duration::from_secs(4);
/// Re-scan the projects folder this often (only when no projects are configured).
const RESCAN: Duration = Duration::from_secs(10 * 60);
const GIT_TIMEOUT: Duration = Duration::from_secs(6);
const GH_TIMEOUT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------- what the board shows

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub projects: Vec<Project>,
    /// The current or last deploy (from the board, this run of quaked).
    pub run: Option<Run>,
    /// The project list has been built at least once.
    pub loaded: bool,
    /// Where projects were looked for ("~/ArlintDev"), or "" when they're configured.
    pub scan: String,
    // Board state, kept by the app (never by the thread):
    /// A deploy (or cancel) armed and waiting for the second press.
    pub armed: Option<Armed>,
    /// The ring pulses green (true) or red until this time (unix ms).
    pub pulse: Option<(bool, u64)>,
    /// The finished deploy's log was put away (hold, or Close).
    pub log_hidden: bool,
    /// A short message in the strip, until this time (unix ms).
    pub toast: Option<(String, u64)>,
}

impl Status {
    /// A deploy is running right now.
    pub fn running(&self) -> Option<&Run> {
        self.run.as_ref().filter(|r| r.running())
    }

    /// The deploy armed for the second press (not a cancel), if it's still waiting.
    pub fn armed_deploy(&self, now_ms: u64) -> Option<&Armed> {
        self.armed.as_ref().filter(|a| !a.cancel && now_ms < a.until_ms)
    }

    /// The log panel is up (a deploy is running, or the last one hasn't been put away).
    pub fn log_shown(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.running() || !self.log_hidden)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Project {
    pub name: String,
    /// As configured / found, with ~ for home ("~/ArlintDev/plcplayground").
    pub path: String,
    pub command: String,
    /// "owner/name" on GitHub, or "" (no CI).
    pub repo: String,
    pub branch: String,
    /// The branch tracks an upstream (so ahead / behind mean something).
    pub upstream: bool,
    pub ahead: u32,
    pub behind: u32,
    /// Changed + untracked files.
    pub dirty: u32,
    pub sha: String,
    pub subject: String,
    /// Unix ms of the last commit.
    pub commit_ms: u64,
    pub git_error: Option<String>,
    /// The latest workflow run.
    pub ci: Option<Ci>,
    pub ci_error: Option<String>,
    /// The last deploy from the board.
    pub last: Option<LastDeploy>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ci {
    pub id: u64,
    /// queued, in_progress, waiting, requested, pending, completed
    pub status: String,
    /// success, failure, cancelled, skipped, timed_out, action_required, startup_failure, neutral
    pub conclusion: String,
    /// The workflow's name ("build").
    pub workflow: String,
    pub branch: String,
    /// The commit / PR title the run is for.
    pub title: String,
    pub url: String,
    pub created_ms: u64,
    /// While running: the current job and step, and how far along.
    pub job: String,
    pub step: String,
    pub step_no: u32,
    pub steps: u32,
    pub jobs_done: u32,
    pub jobs: u32,
}

impl Ci {
    pub fn running(&self) -> bool {
        self.status != "completed"
    }
    pub fn passed(&self) -> bool {
        !self.running() && matches!(self.conclusion.as_str(), "success" | "neutral" | "skipped")
    }
    pub fn failed(&self) -> bool {
        !self.running() && matches!(self.conclusion.as_str(), "failure" | "timed_out" | "startup_failure" | "action_required")
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LastDeploy {
    pub ok: bool,
    pub cancelled: bool,
    /// The exit code (None when it was killed by a signal).
    pub exit: Option<i32>,
    pub ended_ms: u64,
    pub secs: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Run {
    pub id: u64,
    /// Index into `Status::projects` when it started.
    pub index: usize,
    pub project: String,
    pub command: String,
    pub started_ms: u64,
    /// None while it runs.
    pub ended_ms: Option<u64>,
    pub exit: Option<i32>,
    pub signal: Option<i32>,
    /// Cancel was asked for.
    pub cancelled: bool,
    /// The last `LOG_LINES` lines of stdout + stderr.
    pub lines: Vec<String>,
    /// Lines seen in all.
    pub total: usize,
}

impl Run {
    pub fn running(&self) -> bool {
        self.ended_ms.is_none()
    }
    pub fn ok(&self) -> bool {
        !self.running() && !self.cancelled && self.exit == Some(0)
    }
    /// How long it ran (or has been running), in seconds.
    pub fn secs(&self, now_ms: u64) -> f64 {
        self.ended_ms.unwrap_or(now_ms).saturating_sub(self.started_ms) as f64 / 1000.0
    }
}

/// The first press of the two-step confirmation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Armed {
    pub index: usize,
    pub name: String,
    /// Arming the Cancel button (touch) rather than a deploy.
    pub cancel: bool,
    pub since_ms: u64,
    pub until_ms: u64,
    /// Next countdown redraw.
    pub frame_ms: u64,
}

/// What a `Hit::Feature(Page::Deploy, i)` touched (`i` is `Target::code`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A project card / row: arm its deploy.
    Card(usize),
    /// A project's CI badge: open the run.
    Ci(usize),
    /// The armed card's confirm button.
    Confirm,
    /// The log panel's Cancel button (tap twice).
    Cancel,
    /// Put the finished log away.
    Close,
}

impl Target {
    pub fn code(self) -> usize {
        match self {
            Target::Card(i) => i,
            Target::Ci(i) => 10_000 + i,
            Target::Confirm => 20_000,
            Target::Cancel => 20_001,
            Target::Close => 20_002,
        }
    }
    pub fn from_code(i: usize) -> Target {
        match i {
            20_000 => Target::Confirm,
            20_001 => Target::Cancel,
            20_002 => Target::Close,
            i if i >= 10_000 => Target::Ci(i - 10_000),
            i => Target::Card(i),
        }
    }
}

#[derive(Debug)]
pub enum Cmd {
    Configure(Cfg),
    /// The SHIP page was shown / left (CI polls faster while it's up).
    PageShown(bool),
    /// Run project `index`'s deploy; the name must still match (the list may have moved).
    /// Only ever sent after the two-step confirmation on the board.
    Deploy(usize, String),
    /// Stop the running deploy.
    Cancel,
}

/// Something finished while quaked was watching.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    pub project: String,
    /// A CI run (otherwise a deploy from the board).
    pub ci: bool,
    pub ok: bool,
    pub cancelled: bool,
    /// The workflow's name, for CI.
    pub what: String,
}

#[derive(Debug)]
pub enum Update {
    /// The projects (and where they were looked for).
    Projects(Vec<Project>, String),
    /// The running / last deploy (throttled while it streams).
    Run(Run),
    Finished(Finished),
    /// A deploy couldn't start (or similar): a short message for the board.
    Notice(String),
}

// ---------------------------------------------------------------- the thread

/// Starts the module's background work; `emit` hands updates to the app.
pub fn spawn(cfg: Cfg, emit: impl Fn(Update) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("deploy".into()).spawn(move || run(cfg, rx, emit)).unwrap();
    tx
}

/// Everything the worker waits on, in one channel.
enum Msg {
    Cmd(Cmd),
    Polled(Polled),
    Line(u64, String),
    /// (run id, exit code, signal)
    Exit(u64, Option<i32>, Option<i32>),
    Quit,
}

/// One project's git (and maybe CI) check, done on the poll thread.
struct Job {
    gen: u64,
    index: usize,
    dir: PathBuf,
    /// The configured repo, or "" to read it from the folder's git remote.
    repo: String,
    ci: bool,
    /// A run seen in progress: report it if it has finished.
    watch: Option<u64>,
}

struct Polled {
    gen: u64,
    index: usize,
    git: Git,
    repo: String,
    /// Some when CI was checked: the latest run (None = no runs), or why not.
    ci: Option<Result<Option<Ci>, String>>,
    /// The watched run, now finished.
    finished: Option<Ci>,
}

struct Slot {
    view: Project,
    dir: PathBuf,
    /// The repo as configured ("" = from the git remote).
    repo_cfg: String,
    next_git: Instant,
    next_ci: Instant,
    last_ci: Option<Instant>,
    pending: bool,
    /// The latest run, while it's in progress.
    watch: Option<u64>,
}

struct Active {
    run: Run,
    buf: VecDeque<String>,
    pid: i32,
    dirty: bool,
    last_emit: Instant,
    settle_at: Option<Instant>,
    kill_at: Option<Instant>,
}

struct Worker<E: Fn(Update)> {
    cfg: Cfg,
    slots: Vec<Slot>,
    gen: u64,
    shown: bool,
    jobs: Sender<Job>,
    itx: Sender<Msg>,
    active: Option<Active>,
    next_run_id: u64,
    scanned: Instant,
    history: HashMap<String, LastDeploy>,
    history_path: Option<PathBuf>,
    sent: Option<(Vec<Project>, String)>,
    emit: E,
}

fn run(cfg: Cfg, rx: Receiver<Cmd>, emit: impl Fn(Update)) {
    let (itx, irx) = std::sync::mpsc::channel::<Msg>();
    // The app's commands, forwarded into the one channel the worker waits on.
    let fwd = itx.clone();
    std::thread::Builder::new()
        .name("deploy-cmd".into())
        .spawn(move || {
            for c in rx {
                if fwd.send(Msg::Cmd(c)).is_err() {
                    return;
                }
            }
            let _ = fwd.send(Msg::Quit);
        })
        .unwrap();
    // git / gh checks, one at a time, off the worker so a slow `gh` never delays a cancel.
    let (jobs, jrx) = std::sync::mpsc::channel::<Job>();
    let ptx = itx.clone();
    std::thread::Builder::new()
        .name("deploy-poll".into())
        .spawn(move || {
            for job in jrx {
                if ptx.send(Msg::Polled(poll(job))).is_err() {
                    return;
                }
            }
        })
        .unwrap();
    let history_path = if cfg!(test) { None } else { Some(crate::config::dir().join("deploy-history.json")) };
    let mut w = Worker {
        cfg: Cfg::default(),
        slots: Vec::new(),
        gen: 0,
        shown: false,
        jobs,
        itx,
        active: None,
        next_run_id: 1,
        scanned: Instant::now(),
        history: history_path.as_deref().map(load_history).unwrap_or_default(),
        history_path,
        sent: None,
        emit,
    };
    w.configure(cfg);
    loop {
        let wait = w.next_wake().saturating_duration_since(Instant::now());
        match irx.recv_timeout(wait) {
            Ok(Msg::Cmd(c)) => w.command(c),
            Ok(Msg::Polled(p)) => w.polled(p),
            Ok(Msg::Line(id, line)) => w.line(id, line),
            Ok(Msg::Exit(id, code, signal)) => w.exited(id, code, signal),
            Ok(Msg::Quit) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        w.tick();
    }
}

impl<E: Fn(Update)> Worker<E> {
    fn configure(&mut self, cfg: Cfg) {
        self.cfg = cfg;
        self.gen += 1;
        let found = projects(&self.cfg);
        self.scanned = Instant::now();
        let mut old = std::mem::take(&mut self.slots);
        self.slots = found
            .into_iter()
            .map(|p| {
                let mut fresh = self.slot(p);
                // Same folder as before: keep what we already know until the next check.
                if let Some(k) = old.iter().position(|s| s.dir == fresh.dir) {
                    let prev = old.swap_remove(k);
                    let keep = Project { name: fresh.view.name.clone(), path: fresh.view.path.clone(), command: fresh.view.command.clone(), ..prev.view };
                    fresh.view = keep;
                    fresh.watch = prev.watch;
                    fresh.last_ci = prev.last_ci;
                }
                fresh
            })
            .collect();
        self.send_projects();
    }

    fn slot(&self, p: DeployProject) -> Slot {
        let dir = crate::config::expand(&p.path);
        let last = self.history.get(&dir.display().to_string()).cloned();
        Slot {
            view: Project { name: p.name, path: p.path, command: p.command, repo: p.repo.clone(), last, ..Default::default() },
            dir,
            repo_cfg: p.repo,
            next_git: Instant::now(),
            next_ci: Instant::now(),
            last_ci: None,
            pending: false,
            watch: None,
        }
    }

    /// New folders with a deploy.sh show up (and gone ones drop off) without reordering the rest.
    fn rescan(&mut self) {
        self.scanned = Instant::now();
        let found = projects(&self.cfg);
        let same = found.len() == self.slots.len() && found.iter().all(|f| self.slots.iter().any(|s| s.view.path == f.path));
        if same {
            return;
        }
        self.gen += 1;
        let mut slots: Vec<Slot> = Vec::new();
        let old = std::mem::take(&mut self.slots);
        for s in old {
            if found.iter().any(|f| f.path == s.view.path) {
                slots.push(Slot { pending: false, next_git: Instant::now(), ..s });
            }
        }
        for f in found {
            if !slots.iter().any(|s| s.view.path == f.path) {
                slots.push(self.slot(f));
            }
        }
        self.slots = slots;
        self.send_projects();
    }

    fn send_projects(&mut self) {
        let list: Vec<Project> = self.slots.iter().map(|s| s.view.clone()).collect();
        let scan = if self.cfg.projects.is_empty() { self.cfg.scan.clone() } else { String::new() };
        let now = (list, scan);
        if self.sent.as_ref() != Some(&now) {
            self.sent = Some(now.clone());
            (self.emit)(Update::Projects(now.0, now.1));
        }
    }

    fn command(&mut self, c: Cmd) {
        match c {
            Cmd::Configure(cfg) => {
                if cfg != self.cfg {
                    self.configure(cfg);
                }
            }
            Cmd::PageShown(shown) => {
                self.shown = shown;
                if shown {
                    let now = Instant::now();
                    for s in &mut self.slots {
                        s.next_git = now;
                        if s.last_ci.is_none_or(|t| t.elapsed() > Duration::from_secs(15)) {
                            s.next_ci = now;
                        }
                    }
                }
            }
            Cmd::Deploy(index, name) => self.start(index, &name),
            Cmd::Cancel => self.cancel(),
        }
    }

    fn start(&mut self, index: usize, name: &str) {
        if let Some(a) = &self.active {
            return (self.emit)(Update::Notice(format!("{} is still deploying", a.run.project)));
        }
        let Some(slot) = self.slots.get(index).filter(|s| s.view.name == name) else {
            return (self.emit)(Update::Notice(format!("{name} isn't in the list any more")));
        };
        let command = slot.view.command.trim().to_string();
        if command.is_empty() {
            return (self.emit)(Update::Notice(format!("{name} has no deploy command")));
        }
        if !slot.dir.is_dir() {
            return (self.emit)(Update::Notice(format!("{} doesn't exist", slot.view.path)));
        }
        let id = self.next_run_id;
        self.next_run_id += 1;
        log(format!("deploy: {name}: running {command:?} in {}", slot.dir.display()));
        match start_process(&command, &slot.dir, id, self.itx.clone()) {
            Ok(pid) => {
                let run = Run { id, index, project: name.to_string(), command, started_ms: crate::status::now_ms(), ..Default::default() };
                self.active = Some(Active { run, buf: VecDeque::new(), pid, dirty: false, last_emit: Instant::now(), settle_at: None, kill_at: None });
                self.send_run();
            }
            Err(e) => {
                log(format!("deploy: {name}: could not start: {e}"));
                (self.emit)(Update::Notice(format!("Couldn't start {name}'s deploy: {e}")));
            }
        }
    }

    fn cancel(&mut self) {
        let Some(a) = &mut self.active else { return };
        if a.run.ended_ms.is_some() || a.run.cancelled {
            return;
        }
        log(format!("deploy: {}: cancelled from the board", a.run.project));
        a.run.cancelled = true;
        signal_group(a.pid, libc::SIGTERM);
        a.kill_at = Some(Instant::now() + KILL_AFTER);
        push_line(&mut a.buf, &mut a.run.total, "── cancelled from the Quake ──".into());
        a.dirty = true;
    }

    fn line(&mut self, id: u64, line: String) {
        if let Some(a) = self.active.as_mut().filter(|a| a.run.id == id) {
            push_line(&mut a.buf, &mut a.run.total, line);
            a.dirty = true;
        }
    }

    fn exited(&mut self, id: u64, code: Option<i32>, signal: Option<i32>) {
        if let Some(a) = self.active.as_mut().filter(|a| a.run.id == id) {
            a.run.exit = code;
            a.run.signal = signal;
            a.kill_at = None;
            a.settle_at = Some(Instant::now() + SETTLE);
        }
    }

    fn send_run(&mut self) {
        let Some(a) = &mut self.active else { return };
        a.run.lines = a.buf.iter().cloned().collect();
        a.dirty = false;
        a.last_emit = Instant::now();
        (self.emit)(Update::Run(a.run.clone()));
    }

    /// The deploy is over (and its last lines are in): tell the board, remember it.
    fn finish(&mut self) {
        let Some(mut a) = self.active.take() else { return };
        a.run.ended_ms = Some(crate::status::now_ms());
        a.run.lines = a.buf.iter().cloned().collect();
        let r = &a.run;
        let last = LastDeploy { ok: r.ok(), cancelled: r.cancelled, exit: r.exit, ended_ms: r.ended_ms.unwrap_or(0), secs: r.secs(r.ended_ms.unwrap_or(0)) as u64 };
        log(format!("deploy: {}: {} after {}s (exit {:?}, signal {:?})", r.project, if last.ok { "done" } else if last.cancelled { "cancelled" } else { "failed" }, last.secs, r.exit, r.signal));
        (self.emit)(Update::Run(a.run.clone()));
        (self.emit)(Update::Finished(Finished { project: r.project.clone(), ci: false, ok: last.ok, cancelled: last.cancelled, what: String::new() }));
        if let Some(s) = self.slots.iter_mut().find(|s| s.view.name == r.project) {
            s.view.last = Some(last.clone());
            s.next_git = Instant::now();
            s.next_ci = s.next_ci.min(Instant::now() + Duration::from_secs(5));
            self.history.insert(s.dir.display().to_string(), last);
            if let Some(p) = &self.history_path {
                save_history(p, &self.history);
            }
        }
        self.send_projects();
    }

    fn polled(&mut self, p: Polled) {
        if p.gen != self.gen {
            return;
        }
        let shown = self.shown;
        let Some(s) = self.slots.get_mut(p.index) else { return };
        s.pending = false;
        let now = Instant::now();
        let v = &mut s.view;
        let g = p.git;
        v.branch = g.branch;
        v.upstream = g.upstream;
        v.ahead = g.ahead;
        v.behind = g.behind;
        v.dirty = g.dirty;
        v.sha = g.sha;
        v.subject = g.subject;
        v.commit_ms = g.commit_ms;
        v.git_error = g.error;
        v.repo = p.repo;
        s.next_git = now + if shown { Duration::from_secs(10) } else { Duration::from_secs(60) };
        let mut finished = p.finished;
        if let Some(ci) = p.ci {
            s.last_ci = Some(now);
            match ci {
                Ok(latest) => {
                    v.ci_error = None;
                    // The run we were watching finished (and is still the latest): that's news.
                    if let (Some(w), Some(l)) = (s.watch, &latest) {
                        if l.id == w && !l.running() {
                            finished = Some(l.clone());
                        }
                    }
                    s.watch = latest.as_ref().filter(|l| l.running()).map(|l| l.id);
                    v.ci = latest;
                }
                Err(e) => v.ci_error = Some(e),
            }
            let fast = shown && v.ci.as_ref().is_some_and(Ci::running);
            s.next_ci = now + if fast { Duration::from_secs(10) } else { Duration::from_secs(60) };
        }
        let name = v.name.clone();
        if v.repo.is_empty() {
            s.next_ci = now + Duration::from_secs(60);
        }
        // Skipped / neutral runs aren't news.
        if let Some(f) = finished.filter(|f| f.conclusion == "success" || f.failed() || f.conclusion == "cancelled") {
            log(format!("deploy: {name}: CI {} finished: {}", f.workflow, f.conclusion));
            (self.emit)(Update::Finished(Finished { project: name, ci: true, ok: f.conclusion == "success", cancelled: f.conclusion == "cancelled", what: f.workflow }));
        }
        self.send_projects();
    }

    /// Hands due checks to the poll thread, streams the running deploy, escalates a cancel.
    fn tick(&mut self) {
        let now = Instant::now();
        if self.cfg.projects.is_empty() && now.duration_since(self.scanned) >= RESCAN {
            self.rescan();
        }
        for (i, s) in self.slots.iter_mut().enumerate() {
            if s.pending || now < s.next_git.min(s.next_ci) {
                continue;
            }
            let ci = now >= s.next_ci;
            let job = Job { gen: self.gen, index: i, dir: s.dir.clone(), repo: s.repo_cfg.clone(), ci, watch: if ci { s.watch } else { None } };
            if self.jobs.send(job).is_ok() {
                s.pending = true;
            }
        }
        let mut send = false;
        let mut done = false;
        if let Some(a) = &mut self.active {
            if a.dirty && a.last_emit.elapsed() >= EMIT_EVERY {
                send = true;
            }
            if a.kill_at.is_some_and(|t| now >= t) {
                signal_group(a.pid, libc::SIGKILL);
                a.kill_at = None;
            }
            done = a.settle_at.is_some_and(|t| now >= t);
        }
        if done {
            self.finish();
        } else if send {
            self.send_run();
        }
    }

    fn next_wake(&self) -> Instant {
        let now = Instant::now();
        let mut t = now + Duration::from_secs(30);
        if self.cfg.projects.is_empty() {
            t = t.min(self.scanned + RESCAN);
        }
        for s in self.slots.iter().filter(|s| !s.pending) {
            t = t.min(s.next_git.min(s.next_ci));
        }
        if let Some(a) = &self.active {
            if a.dirty {
                t = t.min(a.last_emit + EMIT_EVERY);
            }
            if let Some(k) = a.kill_at {
                t = t.min(k);
            }
            if let Some(k) = a.settle_at {
                t = t.min(k);
            }
        }
        t.max(now)
    }
}

// ---------------------------------------------------------------- projects

/// The configured projects, or the folders under `scan` with an executable deploy.sh
/// (most recently committed first).
fn projects(cfg: &Cfg) -> Vec<DeployProject> {
    if !cfg.projects.is_empty() {
        return cfg
            .projects
            .iter()
            .filter(|p| !p.path.trim().is_empty())
            .map(|p| DeployProject {
                name: if p.name.trim().is_empty() { folder_name(&p.path) } else { p.name.clone() },
                path: p.path.clone(),
                command: if p.command.trim().is_empty() { "./deploy.sh".into() } else { p.command.clone() },
                repo: p.repo.trim().to_string(),
            })
            .collect();
    }
    if cfg.scan.trim().is_empty() {
        return Vec::new();
    }
    let mut found = discover(&crate::config::expand(cfg.scan.trim()));
    let label = cfg.scan.trim().trim_end_matches('/').to_string();
    for p in &mut found {
        p.path = format!("{label}/{}", p.name);
    }
    found
}

fn folder_name(path: &str) -> String {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path).to_string()
}

/// Folders directly under `root` that have an executable deploy.sh.
pub fn discover(root: &Path) -> Vec<DeployProject> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut found: Vec<(u64, DeployProject)> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter(|e| std::fs::metadata(e.path().join("deploy.sh")).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            if name.starts_with('.') {
                return None;
            }
            let dir = e.path();
            let when = git(&dir, &["log", "-1", "--format=%ct"]).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
            Some((when, DeployProject { name, path: dir.display().to_string(), command: "./deploy.sh".into(), repo: String::new() }))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    found.into_iter().map(|(_, p)| p).collect()
}

// ---------------------------------------------------------------- git

#[derive(Debug, Default, PartialEq)]
struct Git {
    branch: String,
    upstream: bool,
    ahead: u32,
    behind: u32,
    dirty: u32,
    sha: String,
    subject: String,
    commit_ms: u64,
    error: Option<String>,
}

/// Runs a program with a timeout; Ok(stdout) on success, Err(stderr / why) otherwise.
/// stdout and stderr are drained on their own threads so a chatty command can't stall.
fn run_cmd(prog: &str, args: &[&str], dir: Option<&Path>, timeout: Duration) -> Result<String, String> {
    let mut cmd = Command::new(prog);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).env("GIT_OPTIONAL_LOCKS", "0").env("GH_PROMPT_DISABLED", "1").env("NO_COLOR", "1");
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let drain = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut s);
            }
            String::from_utf8_lossy(&s).into_owned()
        })
    };
    let out = drain(child.stdout.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
    let err = drain(child.stderr.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed out".into());
            }
        }
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    if status.success() {
        Ok(out)
    } else {
        let msg = if err.trim().is_empty() { out } else { err };
        Err(crate::status::squash(msg.lines().find(|l| !l.trim().is_empty()).unwrap_or("failed"), 90))
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let prog = GIT.iter().find(|p| Path::new(p).exists()).ok_or("git not found")?;
    let mut all = vec!["--no-optional-locks", "-C", dir.to_str().ok_or("bad path")?];
    all.extend_from_slice(args);
    run_cmd(prog, &all, None, GIT_TIMEOUT)
}

fn git_info(dir: &Path) -> Git {
    let mut g = match git(dir, &["status", "--porcelain=v1", "--branch"]) {
        Ok(s) => parse_status(&s),
        Err(e) => {
            let error = if e.contains("not a git repository") { "not a git repository".to_string() } else { e };
            return Git { error: Some(error), ..Default::default() };
        }
    };
    if let Ok(s) = git(dir, &["log", "-1", "--format=%h%x1f%ct%x1f%s"]) {
        let mut it = s.trim_end_matches('\n').splitn(3, '\u{1f}');
        g.sha = it.next().unwrap_or("").to_string();
        g.commit_ms = it.next().and_then(|t| t.parse::<u64>().ok()).unwrap_or(0) * 1000;
        g.subject = it.next().unwrap_or("").to_string();
    }
    g
}

/// `git status --porcelain=v1 --branch`: the "## branch...upstream [ahead 1, behind 2]"
/// header, then one line per changed / untracked file.
fn parse_status(s: &str) -> Git {
    let mut g = Git::default();
    for line in s.lines() {
        let Some(h) = line.strip_prefix("## ") else {
            if !line.trim().is_empty() {
                g.dirty += 1;
            }
            continue;
        };
        if let Some(b) = h.strip_prefix("No commits yet on ").or_else(|| h.strip_prefix("Initial commit on ")) {
            g.branch = b.trim().to_string();
            continue;
        }
        if h.starts_with("HEAD (no branch)") {
            g.branch = "detached".into();
            continue;
        }
        let (refs, track) = match h.split_once(" [") {
            Some((r, t)) => (r, t.trim_end_matches(']')),
            None => (h, ""),
        };
        match refs.split_once("...") {
            Some((b, _up)) => {
                g.branch = b.to_string();
                g.upstream = !track.contains("gone");
            }
            None => g.branch = refs.trim().to_string(),
        }
        for part in track.split(", ") {
            if let Some(n) = part.strip_prefix("ahead ") {
                g.ahead = n.trim().parse().unwrap_or(0);
            } else if let Some(n) = part.strip_prefix("behind ") {
                g.behind = n.trim().parse().unwrap_or(0);
            }
        }
    }
    g
}

/// "owner/name" from a GitHub remote URL (https, ssh or scp-style), else None.
fn github_repo(url: &str) -> Option<String> {
    let u = url.trim();
    let rest = u
        .strip_prefix("git@github.com:")
        .or_else(|| u.split_once("github.com/").map(|(_, r)| r))
        .or_else(|| u.split_once("github.com:").map(|(_, r)| r))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut it = rest.split('/');
    let (owner, name) = (it.next()?, it.next()?);
    (!owner.is_empty() && !name.is_empty()).then(|| format!("{owner}/{name}"))
}

// ---------------------------------------------------------------- CI (gh)

fn gh(args: &[&str]) -> Result<String, String> {
    let prog = GH.iter().find(|p| Path::new(p).exists()).ok_or("gh CLI not found")?;
    run_cmd(prog, args, None, GH_TIMEOUT).map_err(|e| {
        if e.contains("auth login") || e.contains("not logged") {
            "gh isn't signed in (gh auth login)".to_string()
        } else if e.contains("404") {
            "no Actions access to this repo".to_string()
        } else {
            e
        }
    })
}

fn ci_from(v: &Value) -> Option<Ci> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    Some(Ci {
        id: v.get("databaseId")?.as_u64()?,
        status: s("status"),
        conclusion: s("conclusion"),
        workflow: s("name"),
        branch: s("headBranch"),
        title: s("displayTitle"),
        url: s("url"),
        created_ms: crate::github::iso_ms(&s("createdAt")),
        ..Default::default()
    })
}

/// `gh run list --json ...`: the newest run, or None when the repo has none.
fn parse_runs(json: &str) -> Result<Option<Ci>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(v.as_array().and_then(|a| a.first()).and_then(ci_from))
}

/// `gh run view --json jobs`: fills in the current job and step of a running run.
fn apply_jobs(ci: &mut Ci, json: &str) {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return };
    let Some(jobs) = v.get("jobs").and_then(Value::as_array) else { return };
    let st = |j: &Value| j.get("status").and_then(Value::as_str).unwrap_or("").to_string();
    ci.jobs = jobs.len() as u32;
    ci.jobs_done = jobs.iter().filter(|j| st(j) == "completed").count() as u32;
    let current = jobs.iter().find(|j| st(j) == "in_progress").or_else(|| jobs.iter().find(|j| st(j) != "completed"));
    let Some(job) = current else { return };
    ci.job = job.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let steps = job.get("steps").and_then(Value::as_array).cloned().unwrap_or_default();
    ci.steps = steps.len() as u32;
    let step = steps.iter().find(|s| st(s) == "in_progress").or_else(|| steps.iter().find(|s| st(s) != "completed"));
    if let Some(s) = step {
        ci.step = s.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        ci.step_no = s.get("number").and_then(Value::as_u64).unwrap_or(0) as u32;
    } else {
        ci.step.clear();
        ci.step_no = 0;
    }
}

const RUN_FIELDS: &str = "databaseId,status,conclusion,name,headBranch,createdAt,url,displayTitle";

fn latest_run(repo: &str) -> Result<Option<Ci>, String> {
    let out = gh(&["run", "list", "--repo", repo, "--limit", "1", "--json", RUN_FIELDS])?;
    let mut ci = parse_runs(&out)?;
    if let Some(c) = ci.as_mut().filter(|c| c.running()) {
        if let Ok(jobs) = gh(&["run", "view", &c.id.to_string(), "--repo", repo, "--json", "jobs"]) {
            apply_jobs(c, &jobs);
        }
    }
    Ok(ci)
}

fn one_run(repo: &str, id: u64) -> Option<Ci> {
    let out = gh(&["run", "view", &id.to_string(), "--repo", repo, "--json", RUN_FIELDS]).ok()?;
    ci_from(&serde_json::from_str::<Value>(&out).ok()?)
}

fn poll(job: Job) -> Polled {
    let info = git_info(&job.dir);
    let repo = if !job.repo.is_empty() {
        job.repo.clone()
    } else {
        git(&job.dir, &["remote", "get-url", "origin"]).ok().and_then(|u| github_repo(&u)).unwrap_or_default()
    };
    let mut finished = None;
    let ci = (job.ci && !repo.is_empty()).then(|| {
        let latest = latest_run(&repo);
        // The run we watched isn't the latest any more: look it up on its own.
        if let (Some(w), Ok(l)) = (job.watch, &latest) {
            if l.as_ref().is_none_or(|l| l.id != w) {
                finished = one_run(&repo, w).filter(|c| !c.running());
            }
        }
        latest
    });
    Polled { gen: job.gen, index: job.index, git: info, repo, ci, finished }
}

// ---------------------------------------------------------------- the runner

/// Starts `/bin/sh -c command` in `dir` in its own process group; its output arrives as
/// `Msg::Line(id, ..)` and its end as `Msg::Exit(id, ..)`. Returns the process group id.
fn start_process(command: &str, dir: &Path, id: u64, tx: Sender<Msg>) -> std::io::Result<i32> {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut dirs: Vec<&str> = path.split(':').filter(|d| !d.is_empty()).collect();
    for d in ["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PATH", dirs.join(":"))
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0")
        .process_group(0)
        .spawn()?;
    let pid = child.id() as i32;
    let reader = |r: Box<dyn Read + Send>, tx: Sender<Msg>| {
        std::thread::spawn(move || {
            let mut r = BufReader::new(r);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match r.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {
                        if tx.send(Msg::Line(id, clean_line(&String::from_utf8_lossy(&buf)))).is_err() {
                            return;
                        }
                    }
                }
            }
        })
    };
    if let Some(out) = child.stdout.take() {
        reader(Box::new(out), tx.clone());
    }
    if let Some(err) = child.stderr.take() {
        reader(Box::new(err), tx.clone());
    }
    std::thread::Builder::new().name("deploy-wait".into()).spawn(move || {
        let (code, signal) = match child.wait() {
            Ok(st) => (st.code(), st.signal()),
            Err(_) => (None, None),
        };
        let _ = tx.send(Msg::Exit(id, code, signal));
    })?;
    Ok(pid)
}

fn signal_group(pgid: i32, sig: i32) {
    if pgid > 1 {
        unsafe {
            libc::kill(-pgid, sig);
        }
    }
}

fn push_line(buf: &mut VecDeque<String>, total: &mut usize, line: String) {
    buf.push_back(line);
    *total += 1;
    while buf.len() > LOG_LINES {
        buf.pop_front();
    }
}

/// One line of output as a terminal would leave it: escape codes gone, carriage-return
/// progress collapsed to its last state, tabs expanded, and not absurdly long.
pub fn clean_line(raw: &str) -> String {
    let raw = raw.trim_end_matches(['\n', '\r']);
    let raw = raw.rsplit('\r').find(|s| !s.trim().is_empty()).unwrap_or("");
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    // CSI: parameters, then one final byte in @..~
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: up to BEL or ESC \
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.peek() == Some(&'\\')) {
                            if c == '\u{1b}' {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            },
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    if out.chars().count() > 400 {
        out = out.chars().take(399).collect::<String>() + "…";
    }
    out
}

// ---------------------------------------------------------------- history

/// The last deploy per project folder, so cards still say "deployed 2d ago" after a restart.
fn load_history(path: &Path) -> HashMap<String, LastDeploy> {
    let Ok(text) = std::fs::read_to_string(path) else { return HashMap::new() };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) else { return HashMap::new() };
    map.into_iter()
        .map(|(k, v)| {
            let b = |f: &str| v.get(f).and_then(Value::as_bool).unwrap_or(false);
            let n = |f: &str| v.get(f).and_then(Value::as_u64).unwrap_or(0);
            let exit = v.get("exit").and_then(Value::as_i64).map(|x| x as i32);
            (k, LastDeploy { ok: b("ok"), cancelled: b("cancelled"), exit, ended_ms: n("endedMs"), secs: n("secs") })
        })
        .collect()
}

fn save_history(path: &Path, h: &HashMap<String, LastDeploy>) {
    let map: serde_json::Map<String, Value> = h
        .iter()
        .map(|(k, d)| (k.clone(), serde_json::json!({ "ok": d.ok, "cancelled": d.cancelled, "exit": d.exit, "endedMs": d.ended_ms, "secs": d.secs })))
        .collect();
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_vec_pretty(&Value::Object(map)).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

// ---------------------------------------------------------------- preview

/// Sample data for `quaked preview`: one project deploying, one with CI running, one green.
pub fn sample() -> Status {
    let now = crate::status::now_ms();
    let (s, m, h) = (1000, 60_000, 3_600_000);
    let lines = [
        "$ ./deploy.sh",
        "==> building plcplayground (vite, production)",
        "vite v6.2.1 building for production...",
        "transforming (412) src/sim/timer.ts",
        "✓ 418 modules transformed.",
        "dist/index.html                   0.62 kB │ gzip:  0.38 kB",
        "dist/assets/index-BqK3d9.css     18.40 kB │ gzip:  4.71 kB",
        "dist/assets/index-C1x7Lw.js     402.18 kB │ gzip: 121.06 kB",
        "(!) Some chunks are larger than 400 kB after minification.",
        "✓ built in 6.84s",
        "==> syncing dist/ to workbench:~/Projects/plcplayground",
        "sending incremental file list",
        "assets/index-C1x7Lw.js",
        "sent 404,113 bytes  received 1,284 bytes  162,158.80 bytes/sec",
        "==> docker compose up -d (workbench)",
        " Container plcplayground-web-1  Recreate",
        " Container plcplayground-web-1  Recreated",
        " Container plcplayground-web-1  Starting",
    ];
    let run = Run {
        id: 7,
        index: 0,
        project: "plcplayground".into(),
        command: "./deploy.sh".into(),
        started_ms: now - 42 * s,
        lines: lines.iter().map(|l| l.to_string()).collect(),
        total: lines.len(),
        ..Default::default()
    };
    let projects = vec![
        Project {
            name: "plcplayground".into(),
            path: "~/ArlintDev/plcplayground".into(),
            command: "./deploy.sh".into(),
            repo: "arlintdev/plcplayground".into(),
            branch: "main".into(),
            upstream: true,
            ahead: 2,
            sha: "3f9c2e1".into(),
            subject: "Timer instruction (TON) in the simulator and toolbox".into(),
            commit_ms: now - 6 * m,
            ci: Some(Ci {
                id: 1,
                status: "completed".into(),
                conclusion: "success".into(),
                workflow: "build".into(),
                branch: "main".into(),
                title: "Rung editor: drag to reorder".into(),
                url: "https://github.com/arlintdev/plcplayground/actions/runs/1".into(),
                created_ms: now - 2 * h,
                ..Default::default()
            }),
            last: Some(LastDeploy { ok: true, exit: Some(0), ended_ms: now - 26 * h, secs: 51, ..Default::default() }),
            ..Default::default()
        },
        Project {
            name: "digin".into(),
            path: "~/ArlintDev/digin-all".into(),
            command: "./deploy.sh".into(),
            repo: "arlintdev/digin".into(),
            branch: "dig-596-bot-kinds".into(),
            upstream: true,
            dirty: 3,
            sha: "a41b07d".into(),
            subject: "Bot kinds: migration 0256 and the runner handshake".into(),
            commit_ms: now - 14 * m,
            ci: Some(Ci {
                id: 2,
                status: "in_progress".into(),
                workflow: "build".into(),
                branch: "dig-596-bot-kinds".into(),
                title: "Bot kinds (DIG-596)".into(),
                url: "https://github.com/arlintdev/digin/actions/runs/2".into(),
                created_ms: now - (3 * m + 12 * s),
                job: "test / cargo".into(),
                step: "cargo test --release".into(),
                step_no: 5,
                steps: 9,
                jobs_done: 2,
                jobs: 4,
                ..Default::default()
            }),
            last: Some(LastDeploy { ok: false, exit: Some(1), ended_ms: now - 3 * h, secs: 97, ..Default::default() }),
            ..Default::default()
        },
        Project {
            name: "ibtg".into(),
            path: "~/ArlintDev/ibtg".into(),
            command: "./deploy.sh".into(),
            repo: "arlintdev/ibtg".into(),
            branch: "main".into(),
            upstream: true,
            sha: "9e0d4aa".into(),
            subject: "Free mode: wallet pickups respawn after 30s".into(),
            commit_ms: now - 2 * h - 20 * m,
            ci: Some(Ci {
                id: 3,
                status: "completed".into(),
                conclusion: "success".into(),
                workflow: "deploy-check".into(),
                branch: "main".into(),
                title: "Free mode: wallet pickups respawn after 30s".into(),
                url: "https://github.com/arlintdev/ibtg/actions/runs/3".into(),
                created_ms: now - 2 * h - 18 * m,
                ..Default::default()
            }),
            last: Some(LastDeploy { ok: true, exit: Some(0), ended_ms: now - 2 * h - 10 * m, secs: 38, ..Default::default() }),
            ..Default::default()
        },
    ];
    Status { projects, run: Some(run), loaded: true, scan: "~/ArlintDev".into(), ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("quaked-deploy-{tag}-{}-{}", std::process::id(), crate::status::now_ms()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Collects a run's output and exit from the runner's messages.
    fn collect(rx: &Receiver<Msg>, id: u64, timeout: Duration) -> (Vec<String>, Option<(Option<i32>, Option<i32>)>) {
        let deadline = Instant::now() + timeout;
        let mut lines = Vec::new();
        let mut exit = None;
        let mut settle = None;
        loop {
            let now = Instant::now();
            if now >= deadline || settle.is_some_and(|t| now >= t) {
                return (lines, exit);
            }
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Msg::Line(i, l)) if i == id => lines.push(l),
                Ok(Msg::Exit(i, c, s)) if i == id => {
                    exit = Some((c, s));
                    settle = Some(Instant::now() + Duration::from_millis(300));
                }
                _ => {}
            }
        }
    }

    #[test]
    fn runner_streams_both_streams_and_the_exit_code() {
        let dir = temp_dir("run");
        let (tx, rx) = std::sync::mpsc::channel();
        start_process("echo hi; echo err 1>&2; pwd; exit 3", &dir, 1, tx).unwrap();
        let (lines, exit) = collect(&rx, 1, Duration::from_secs(10));
        assert!(lines.contains(&"hi".to_string()), "{lines:?}");
        assert!(lines.contains(&"err".to_string()), "{lines:?}");
        let here = std::fs::canonicalize(&dir).unwrap().display().to_string();
        assert!(lines.iter().any(|l| *l == here || l.ends_with(dir.file_name().unwrap().to_str().unwrap())), "ran in {dir:?}: {lines:?}");
        assert_eq!(exit, Some((Some(3), None)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn runner_cancel_kills_the_whole_group() {
        let dir = temp_dir("cancel");
        let (tx, rx) = std::sync::mpsc::channel();
        // The sleep is a child of sh: killing only sh would leave it holding the pipe open.
        let pid = start_process("echo start; sleep 30; echo never", &dir, 2, tx).unwrap();
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Msg::Line(2, l)) => assert_eq!(l, "start"),
            _ => panic!("no first line"),
        }
        let t = Instant::now();
        signal_group(pid, libc::SIGTERM);
        let (lines, exit) = collect(&rx, 2, Duration::from_secs(10));
        assert!(t.elapsed() < Duration::from_secs(5), "took {:?}", t.elapsed());
        assert!(!lines.contains(&"never".to_string()));
        assert_eq!(exit, Some((None, Some(libc::SIGTERM))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole worker: a deploy of a harmless command, its streamed log, its result, and
    /// the name guard on Cmd::Deploy.
    #[test]
    fn worker_runs_a_confirmed_deploy_and_reports_it() {
        let dir = temp_dir("worker");
        let cfg = Cfg {
            projects: vec![DeployProject { name: "scratch".into(), path: dir.display().to_string(), command: "echo one; echo two 1>&2; exit 0".into(), repo: String::new() }],
            ..Default::default()
        };
        let (utx, urx) = std::sync::mpsc::channel();
        let tx = spawn(cfg, move |u| {
            let _ = utx.send(u);
        });
        tx.send(Cmd::Deploy(0, "someone-else".into())).unwrap();
        tx.send(Cmd::Deploy(0, "scratch".into())).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let (mut notice, mut finished, mut last_run) = (None, None, None);
        while Instant::now() < deadline && finished.is_none() {
            match urx.recv_timeout(Duration::from_millis(100)) {
                Ok(Update::Notice(n)) => notice = Some(n),
                Ok(Update::Finished(f)) => finished = Some(f),
                Ok(Update::Run(r)) => last_run = Some(r),
                _ => {}
            }
        }
        assert!(notice.is_some_and(|n| n.contains("someone-else")));
        let f = finished.expect("finished");
        assert!(f.ok && !f.ci && f.project == "scratch");
        let r = last_run.expect("run");
        assert!(!r.running() && r.ok());
        assert!(r.lines.contains(&"one".to_string()) && r.lines.contains(&"two".to_string()), "{:?}", r.lines);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn worker_cancel_ends_the_deploy_as_cancelled() {
        let dir = temp_dir("worker-cancel");
        let cfg = Cfg {
            projects: vec![DeployProject { name: "slow".into(), path: dir.display().to_string(), command: "echo going; sleep 30".into(), repo: String::new() }],
            ..Default::default()
        };
        let (utx, urx) = std::sync::mpsc::channel();
        let tx = spawn(cfg, move |u| {
            let _ = utx.send(u);
        });
        tx.send(Cmd::Deploy(0, "slow".into())).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut finished = None;
        let mut cancelled = false;
        while Instant::now() < deadline && finished.is_none() {
            match urx.recv_timeout(Duration::from_millis(100)) {
                Ok(Update::Run(r)) if !cancelled && r.lines.iter().any(|l| l == "going") => {
                    tx.send(Cmd::Cancel).unwrap();
                    cancelled = true;
                }
                Ok(Update::Finished(f)) => finished = Some(f),
                _ => {}
            }
        }
        let f = finished.expect("finished");
        assert!(f.cancelled && !f.ok);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovers_folders_with_an_executable_deploy_script() {
        let root = temp_dir("discover");
        for (name, mode) in [("site", 0o755), ("notes", 0o644)] {
            let d = root.join(name);
            std::fs::create_dir_all(&d).unwrap();
            let f = d.join("deploy.sh");
            std::fs::write(&f, "#!/bin/sh\necho never run\n").unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        std::fs::create_dir_all(root.join("empty")).unwrap();
        let found = discover(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "site");
        assert_eq!(found[0].command, "./deploy.sh");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parses_git_status_headers() {
        let g = parse_status("## main...origin/main [ahead 2, behind 1]\n M src/a.rs\n?? new.txt\n");
        assert_eq!((g.branch.as_str(), g.upstream, g.ahead, g.behind, g.dirty), ("main", true, 2, 1, 2));
        let g = parse_status("## feature/x...origin/feature/x [behind 4]\n");
        assert_eq!((g.branch.as_str(), g.ahead, g.behind, g.dirty), ("feature/x", 0, 4, 0));
        let g = parse_status("## main\n");
        assert_eq!((g.branch.as_str(), g.upstream), ("main", false));
        let g = parse_status("## main...origin/main [gone]\n");
        assert!(!g.upstream);
        assert_eq!(parse_status("## No commits yet on main\n").branch, "main");
        assert_eq!(parse_status("## HEAD (no branch)\n").branch, "detached");
    }

    #[test]
    fn github_repos_from_remotes() {
        assert_eq!(github_repo("https://github.com/arlintdev/plcplayground.git").as_deref(), Some("arlintdev/plcplayground"));
        assert_eq!(github_repo("git@github.com:arlintdev/ibtg.git\n").as_deref(), Some("arlintdev/ibtg"));
        assert_eq!(github_repo("ssh://git@github.com/arlintdev/digin").as_deref(), Some("arlintdev/digin"));
        assert_eq!(github_repo("https://gitlab.com/a/b.git"), None);
    }

    #[test]
    fn parses_runs_and_jobs() {
        let runs = r#"[{"conclusion":"","createdAt":"2026-11-13T03:14:38Z","databaseId":43357633249,"displayTitle":"v0.46.1 (#744)","headBranch":"v0.46.1","name":"build","status":"in_progress","url":"https://github.com/o/r/actions/runs/43357633249"}]"#;
        let mut ci = parse_runs(runs).unwrap().unwrap();
        assert_eq!(ci.id, 43357633249);
        assert!(ci.running() && !ci.passed() && !ci.failed());
        assert_eq!(ci.created_ms, crate::github::iso_ms("2026-11-13T03:14:38Z"));
        let jobs = r#"{"jobs":[
            {"name":"lint","status":"completed","conclusion":"success","steps":[]},
            {"name":"test","status":"in_progress","conclusion":"","steps":[
                {"name":"Set up job","number":1,"status":"completed","conclusion":"success"},
                {"name":"cargo test","number":2,"status":"in_progress","conclusion":""},
                {"name":"Post","number":3,"status":"queued","conclusion":""}]},
            {"name":"release","status":"queued","conclusion":"","steps":[]}]}"#;
        apply_jobs(&mut ci, jobs);
        assert_eq!((ci.job.as_str(), ci.step.as_str(), ci.step_no, ci.steps, ci.jobs_done, ci.jobs), ("test", "cargo test", 2, 3, 1, 3));
        assert_eq!(parse_runs("[]").unwrap(), None);
        let done = parse_runs(r#"[{"databaseId":1,"status":"completed","conclusion":"failure"}]"#).unwrap().unwrap();
        assert!(done.failed());
    }

    #[test]
    fn cleans_terminal_output() {
        assert_eq!(clean_line("\u{1b}[32m✓\u{1b}[0m built\n"), "✓ built");
        assert_eq!(clean_line("10%\r50%\r100%\r\n"), "100%");
        assert_eq!(clean_line("a\tb"), "a    b");
        assert_eq!(clean_line("\u{1b}]0;title\u{7}text"), "text");
    }

    #[test]
    fn targets_round_trip() {
        for t in [Target::Card(0), Target::Card(7), Target::Ci(0), Target::Ci(12), Target::Confirm, Target::Cancel, Target::Close] {
            assert_eq!(Target::from_code(t.code()), t);
        }
    }
}
