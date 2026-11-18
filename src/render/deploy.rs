//! SHIP page: project cards (git, CI, last deploy) and, while a deploy runs, its live log.
//!
//! Two layouts share one selection (`v.tile`):
//!   cards   a strip, then up to four project cards (more scroll with the knob)
//!   log     a compact project list on the left, the running / last deploy's log on the right
//! The armed state (first press of the two-step deploy) turns the strip and the card red.
use super::*;
use crate::deploy::{Ci, Project, Run, Status, Target};

const ARMED: u32 = 0xFF3B30;
const ARMED_BG: u32 = 0x2A1012;
const BLUE: u32 = 0x8AB4F8;
const BOX: u32 = 0x1C1C22;
const STRIP_H: f32 = 40.0;
const TOP: f32 = CY + STRIP_H + 6.0;
const LIST_W: f32 = 560.0;
const ROW_H: f32 = 80.0;
const ROWS: usize = 5;
const LOG_ROW_H: f32 = 20.0;
const LOG_TOP: f32 = 76.0;

// ---------------------------------------------------------------- layout

/// Three columns for up to three projects, four beyond that (the rest scroll).
fn cols(n: usize) -> usize {
    n.clamp(3, 4)
}

fn card_rect(slot: usize, cols: usize) -> R {
    let w = (W - 2.0 * M - (cols as f32 - 1.0) * G) / cols as f32;
    (M + slot as f32 * (w + G), TOP, w, CY + CH - TOP)
}

/// (first card shown, columns)
fn card_window(v: &View) -> (usize, usize) {
    let n = v.deploy.projects.len();
    let c = cols(n);
    (list_window(n, v.tile, c), c)
}

fn ci_box(r: R) -> R {
    (r.0 + 16.0, r.1 + 148.0, r.2 - 32.0, 70.0)
}

fn action_box(r: R) -> R {
    (r.0 + 16.0, r.1 + r.3 - 76.0, r.2 - 32.0, 60.0)
}

fn list_panel() -> R {
    (M, CY, LIST_W, CH)
}

fn row_rect(slot: usize) -> R {
    (M + 8.0, CY + 8.0 + slot as f32 * ROW_H, LIST_W - 16.0, ROW_H - 6.0)
}

fn row_ci(r: R) -> R {
    (r.0 + r.2 - 182.0, r.1 + 12.0, 170.0, r.3 - 24.0)
}

fn log_panel() -> R {
    (M + LIST_W + G, CY, W - 2.0 * M - LIST_W - G, CH)
}

fn log_button(p: R) -> R {
    (p.0 + p.2 - 20.0 - 168.0, p.1 + 12.0, 168.0, 44.0)
}

fn log_rows() -> usize {
    ((CH - LOG_TOP - 12.0) / LOG_ROW_H) as usize
}

fn is_running(d: &Status, p: &Project) -> bool {
    d.running().is_some_and(|r| r.project == p.name)
}

/// The card's edge color: what most needs a look.
fn tone(d: &Status, i: usize, p: &Project, now: u64) -> u32 {
    if is_running(d, p) {
        ORANGE
    } else if d.armed_deploy(now).is_some_and(|a| a.index == i) {
        ARMED
    } else if p.ci.as_ref().is_some_and(Ci::failed) || p.last.as_ref().is_some_and(|l| !l.ok && !l.cancelled) {
        RED
    } else if p.ci.as_ref().is_some_and(Ci::running) {
        AMBER
    } else if p.ci.as_ref().is_some_and(Ci::passed) || p.last.as_ref().is_some_and(|l| l.ok) {
        GREEN
    } else {
        0x3A3A42
    }
}

/// (symbol, color, label) for a CI run.
fn ci_look(ci: &Ci, tick: bool) -> (&'static str, u32, &'static str) {
    if ci.running() {
        let col = if tick { AMBER } else { 0xB08A10 };
        return match ci.status.as_str() {
            "queued" | "waiting" | "requested" | "pending" => ("clock.fill", col, "CI queued"),
            _ => ("arrow.triangle.2.circlepath", col, "CI running"),
        };
    }
    match ci.conclusion.as_str() {
        "success" => ("checkmark.circle.fill", GREEN, "CI passed"),
        "failure" | "timed_out" | "startup_failure" => ("xmark.circle.fill", RED, "CI failed"),
        "action_required" => ("exclamationmark.circle.fill", AMBER, "CI needs approval"),
        "cancelled" => ("minus.circle.fill", MUTED, "CI cancelled"),
        "skipped" | "neutral" => ("checkmark.circle", MUTED, "CI skipped"),
        _ => ("questionmark.circle", MUTED, "CI"),
    }
}

fn log_color(line: &str) -> u32 {
    let l = line.to_lowercase();
    if l.contains("error") || l.contains("fatal") || l.contains("failed") || l.contains("panic") || l.contains("denied") {
        0xFF9B9B
    } else if l.contains("warn") || l.starts_with("(!)") {
        0xE6B673
    } else if line.starts_with("==>") || line.starts_with("$ ") || line.starts_with("──") || line.starts_with("+ ") {
        BLUE
    } else if line.starts_with('✓') || line.starts_with("✔") || l.starts_with("done") {
        0x7FD1B9
    } else {
        0xC9C9D1
    }
}

/// Dark text on a light accent, white on a dark one.
fn on(bg: u32) -> u32 {
    let (r, g, b) = ((bg >> 16) & 255, (bg >> 8) & 255, bg & 255);
    if r * 299 + g * 587 + b * 114 > 150_000 { 0x0B1A10 } else { 0xFFFFFF }
}

/// "just now", "5m ago", "2h10 ago".
fn ago(ms: u64) -> String {
    if ms < 60_000 { "just now".into() } else { format!("{} ago", age(ms)) }
}

// ---------------------------------------------------------------- drawing

pub(super) fn draw(c: &mut Canvas, v: &View) {
    let d = v.deploy;
    if d.projects.is_empty() {
        return empty(c, v);
    }
    if d.log_shown() {
        project_list(c, v);
        if let Some(r) = &d.run {
            log_view(c, v, r);
        }
    } else {
        strip(c, v);
        cards(c, v);
    }
}

fn empty(c: &mut Canvas, v: &View) {
    let d = v.deploy;
    c.rrect((M, CY, W - 2.0 * M, CH), 16.0, PANEL_DEEP, 1.0);
    let mid = CY + CH / 2.0;
    c.symbol("paperplane.fill", 54.0, W / 2.0, mid - 70.0, FAINT);
    if !d.loaded {
        c.text(F::Medium, "Looking for projects…", 30.0, W / 2.0, mid + 10.0, FAINT, Align::Center);
        return;
    }
    c.text(F::Medium, "Nothing to deploy", 30.0, W / 2.0, mid + 10.0, MUTED, Align::Center);
    let how = if d.scan.is_empty() {
        "None of the configured projects' folders exist".to_string()
    } else {
        format!("Add projects under deploy.projects in the settings, or give a folder in {} an executable deploy.sh", d.scan)
    };
    c.fit(F::Medium, &how, 19.0, W - 200.0, W / 2.0, mid + 48.0, FAINT, Align::Center);
}

/// The line above the cards: a summary, or the armed warning.
fn strip(c: &mut Canvas, v: &View) {
    let d = v.deploy;
    let now = v.now_ms;
    if let Some(a) = d.armed_deploy(now) {
        let r = (M, CY + 1.0, W - 2.0 * M, STRIP_H);
        c.rrect(r, 10.0, ARMED, 1.0);
        let left = (a.until_ms.saturating_sub(now)) as f32 / 1000.0;
        let x = M + 18.0;
        let pw = c.pill(x, CY + 8.5, "ARMED", 14.0, 0xFFFFFF, ARMED);
        let w = c.text(F::Bold, "Press again to deploy", 20.0, x + pw + 16.0, CY + 28.0, 0xFFFFFF, Align::Left);
        c.text(F::Bold, &a.name, 20.0, x + pw + 16.0 + w + 8.0, CY + 28.0, 0xFFFFFF, Align::Left);
        let rx = W - M - 18.0;
        let tw = c.text(F::Bold, &format!("{left:.1}s"), 20.0, rx, CY + 28.0, 0xFFFFFF, Align::Right);
        c.text(F::Medium, "turn, double-click or hold to cancel", 16.0, rx - tw - 22.0, CY + 27.0, 0xFFD6D3, Align::Right);
        return;
    }
    let pw = c.pill(M, CY + 8.0, "SHIP", 14.0, v.accent, on(v.accent));
    let mut x = M + pw + 16.0;
    let n = d.projects.len();
    let ci_running = d.projects.iter().filter(|p| p.ci.as_ref().is_some_and(Ci::running)).count();
    let failing = d.projects.iter().filter(|p| p.ci.as_ref().is_some_and(Ci::failed)).count();
    let dirty = d.projects.iter().filter(|p| p.dirty > 0).count();
    let ahead = d.projects.iter().filter(|p| p.ahead > 0).count();
    let parts = [
        (plural(n, "project", "projects"), TEXT_2),
        (format!("{ci_running} CI running"), if ci_running > 0 { AMBER } else { MUTED }),
        (format!("{failing} failing"), if failing > 0 { RED } else { MUTED }),
        (format!("{dirty} with changes"), if dirty > 0 { AMBER } else { MUTED }),
        (format!("{ahead} unpushed"), if ahead > 0 { ORANGE } else { MUTED }),
    ];
    for (i, (t, col)) in parts.iter().enumerate() {
        if i > 0 {
            x += c.text(F::Medium, "·", 18.0, x + 4.0, CY + 27.0, FAINT, Align::Left) + 18.0;
        }
        x += c.text(F::Medium, t, 18.0, x, CY + 27.0, *col, Align::Left) + 10.0;
    }
    let mut rx = W - M;
    // More projects than fit: where the window is ("‹ 3   4–7 of 10   3 ›").
    let (first, cols) = card_window(v);
    if n > cols {
        let right = n.saturating_sub(first + cols);
        if right > 0 {
            rx -= c.pill(rx - c.measure(F::Bold, &format!("{right} ›"), 14.0) - 14.0 * 1.4, CY + 8.0, &format!("{right} ›"), 14.0, 0x2A2A31, TEXT) + 10.0;
        }
        rx -= c.text(F::Bold, &format!("{}–{} of {n}", first + 1, first + cols), 15.0, rx, CY + 27.0, MUTED, Align::Right) + 10.0;
        if first > 0 {
            rx -= c.pill(rx - c.measure(F::Bold, &format!("‹ {first}"), 14.0) - 14.0 * 1.4, CY + 8.0, &format!("‹ {first}"), 14.0, 0x2A2A31, TEXT) + 10.0;
        }
        rx -= 16.0;
    }
    let room = rx - x - 30.0;
    if let Some((t, _)) = d.toast.as_ref().filter(|(_, until)| now < *until) {
        c.fit(F::Bold, t, 17.0, room, rx, CY + 27.0, AMBER, Align::Right);
    } else if let Some(r) = &d.run {
        let what = if r.ok() { "deployed" } else if r.cancelled { "cancelled" } else { "failed" };
        let label = format!("last: {} {what} {}", r.project, ago(now.saturating_sub(r.ended_ms.unwrap_or(now))));
        c.fit(F::Medium, &label, 16.0, room, rx, CY + 27.0, if r.ok() || r.cancelled { MUTED } else { RED }, Align::Right);
    } else if !d.scan.is_empty() && n <= cols {
        c.fit(F::Mono, &format!("{}/*/deploy.sh", d.scan), 14.0, room, rx, CY + 26.0, FAINT, Align::Right);
    }
}

fn cards(c: &mut Canvas, v: &View) {
    let d = v.deploy;
    let (first, cols) = card_window(v);
    for slot in 0..cols {
        let r = card_rect(slot, cols);
        match d.projects.get(first + slot) {
            Some(p) => card(c, v, first + slot, p, r),
            None => c.rrect(r, 14.0, PANEL_DEEP, 1.0),
        }
    }
}

fn card(c: &mut Canvas, v: &View, i: usize, p: &Project, r: R) {
    let d = v.deploy;
    let now = v.now_ms;
    let (x, y, w, h) = r;
    let armed = d.armed_deploy(now).filter(|a| a.index == i);
    let selected = v.level != &Level::Top && v.tile == i;
    let running = is_running(d, p);
    c.rrect(r, 14.0, if armed.is_some() { ARMED_BG } else { PANEL }, 1.0);
    c.rrect((x, y, 6.0, h), 3.0, tone(d, i, p, now), 1.0);
    let ix = x + 24.0;
    let iw = w - 44.0;

    // Name, then where it lives and what deploys it.
    let mut name_w = iw;
    if running {
        let label = "DEPLOYING";
        let pw = c.measure(F::Bold, label, 12.0) + 12.0 * 1.4;
        c.pill(x + w - 20.0 - pw, y + 20.0, label, 12.0, ORANGE, 0x1F0E07);
        name_w -= pw + 12.0;
    }
    c.fit(F::Bold, &p.name, 28.0, name_w, ix, y + 44.0, TEXT, Align::Left);
    c.clip(F::Mono, &format!("{}  ·  {}", p.path, p.command), 14.0, iw, ix, y + 68.0, FAINT);

    // Git: branch, ahead / behind, changes.
    let gy = y + 104.0;
    if let Some(e) = &p.git_error {
        c.symbol("exclamationmark.triangle.fill", 15.0, ix + 8.0, gy - 6.0, FAINT);
        c.clip(F::Medium, e, 16.0, iw - 24.0, ix + 24.0, gy, FAINT);
    } else {
        git_line(c, p, ix, gy, iw, 17.0);
    }

    // Last commit.
    if !p.subject.is_empty() {
        let when = age(now.saturating_sub(p.commit_ms));
        let aw = c.text(F::Medium, &when, 15.0, x + w - 20.0, y + 134.0, FAINT, Align::Right);
        c.clip(F::Medium, &p.subject, 17.0, iw - aw - 14.0, ix, y + 134.0, TEXT_2);
    }

    // CI badge (tap opens the run).
    ci_badge(c, v, p, ci_box(r));

    // Last deploy.
    let dy = y + 252.0;
    deploy_line(c, v, p, ix, dy, iw);

    // The action band: what a press does here.
    let ab = action_box(r);
    if let Some(a) = armed {
        let left = a.until_ms.saturating_sub(now);
        let total = a.until_ms.saturating_sub(a.since_ms).max(1);
        c.rrect(ab, 12.0, ARMED, 1.0);
        let (bx, by, bw, bh) = ab;
        c.symbol("paperplane.fill", 20.0, bx + 30.0, by + bh / 2.0 - 2.0, 0xFFFFFF);
        let secs = left.div_ceil(1000);
        let tw = c.text(F::Bold, &format!("{secs}s"), 22.0, bx + bw - 18.0, by + 37.0, 0xFFFFFF, Align::Right);
        c.fit(F::Bold, "PRESS AGAIN TO DEPLOY", 20.0, bw - 70.0 - tw - 16.0, bx + 52.0, by + 36.0, 0xFFFFFF, Align::Left);
        let frac = left as f32 / total as f32;
        c.rrect((bx + 12.0, by + bh - 9.0, (bw - 24.0) * frac, 4.0), 2.0, 0xFFFFFF, 0.55);
        c.outline(r, 14.0, ARMED, 4.0);
        return;
    }
    if selected {
        if let Some(rn) = d.running() {
            let msg = if rn.project == p.name { "hold  to cancel the deploy".to_string() } else { format!("{} is deploying", rn.project) };
            c.rrect(ab, 12.0, 0x1A1A1F, 1.0);
            c.text(F::Medium, &msg, 17.0, ab.0 + ab.2 / 2.0, ab.1 + 36.0, MUTED, Align::Center);
        } else {
            c.outline(ab, 12.0, 0x3A3A42, 2.0);
            let label = "press  to arm the deploy";
            let lw = c.measure(F::Medium, label, 18.0) + 30.0;
            let lx = ab.0 + (ab.2 - lw) / 2.0;
            c.symbol("paperplane", 17.0, lx + 9.0, ab.1 + ab.3 / 2.0 - 1.0, TEXT_2);
            c.text(F::Medium, label, 18.0, lx + 30.0, ab.1 + 36.0, TEXT_2, Align::Left);
        }
        c.outline(r, 14.0, TEXT, 3.0);
    }
}

/// "⑂ main  ↑2  ↓1  ● 3 changed" (or "clean").
fn git_line(c: &mut Canvas, p: &Project, x: f32, y: f32, w: f32, px: f32) {
    if p.branch.is_empty() {
        c.text(F::Medium, "…", px, x, y, FAINT, Align::Left);
        return;
    }
    let end = x + w;
    c.symbol("arrow.triangle.branch", px - 2.0, x + 8.0, y - px * 0.35, MUTED);
    // Leave room for the counts after the branch name.
    let counts_w = 64.0 * (u32::from(p.ahead > 0) + u32::from(p.behind > 0)) as f32 + if p.dirty > 0 { 130.0 } else { 70.0 };
    let mut cx = x + 22.0;
    cx += c.clip(F::Mono, &p.branch, px - 1.0, (w - 22.0 - counts_w).max(60.0), cx, y, TEXT_2) + 16.0;
    for (n, sym, col) in [(p.ahead, "arrow.up", ORANGE), (p.behind, "arrow.down", BLUE)] {
        if n > 0 && cx < end - 40.0 {
            c.symbol(sym, px - 4.0, cx + 6.0, y - px * 0.35, col);
            cx += 14.0 + c.text(F::Bold, &n.to_string(), px - 1.0, cx + 14.0, y, col, Align::Left) + 14.0;
        }
    }
    if cx < end - 40.0 {
        if p.dirty > 0 {
            c.circle(cx + 5.0, y - px * 0.33, 4.5, AMBER, 1.0);
            c.clip(F::Medium, &format!("{} changed", p.dirty), px - 1.0, end - cx - 16.0, cx + 16.0, y, AMBER);
        } else if !p.upstream {
            c.clip(F::Medium, "no upstream", px - 1.0, end - cx, cx, y, MUTED);
        } else if p.ahead == 0 && p.behind == 0 {
            c.clip(F::Medium, "clean", px - 1.0, end - cx, cx, y, MUTED);
        }
    }
}

fn ci_badge(c: &mut Canvas, v: &View, p: &Project, b: R) {
    let (bx, by, bw, bh) = b;
    c.rrect(b, 10.0, BOX, 1.0);
    let Some(ci) = &p.ci else {
        let msg = match (&p.ci_error, p.repo.is_empty()) {
            (Some(e), _) => e.clone(),
            (None, true) => "No GitHub repo, so no CI".into(),
            (None, false) if p.branch.is_empty() => "Checking CI…".into(),
            (None, false) => "No workflow runs yet".into(),
        };
        c.symbol("circle.dashed", 20.0, bx + 26.0, by + bh / 2.0, FAINT);
        c.clip(F::Medium, &msg, 16.0, bw - 60.0, bx + 48.0, by + bh / 2.0 + 6.0, if p.ci_error.is_some() { AMBER } else { FAINT });
        return;
    };
    let (sym, col, label) = ci_look(ci, v.tick);
    c.symbol(sym, 22.0, bx + 26.0, by + 24.0, col);
    let tx = bx + 48.0;
    // Right: how long it's been running, or how long ago it ran; and "opens" glyph.
    c.symbol("arrow.up.right", 12.0, bx + bw - 16.0, by + 16.0, FAINT);
    let when = if ci.running() { mmss(v.now_ms.saturating_sub(ci.created_ms) as f64 / 1000.0) } else { ago(v.now_ms.saturating_sub(ci.created_ms)) };
    let ww = c.text(F::Medium, &when, 15.0, bx + bw - 30.0, by + 30.0, if ci.running() { AMBER } else { FAINT }, Align::Right);
    let lw = c.text(F::Bold, label, 17.0, tx, by + 30.0, col, Align::Left);
    if !ci.workflow.is_empty() {
        c.clip(F::Medium, &format!("·  {}", ci.workflow), 15.0, (bw - 48.0 - lw - ww - 52.0).max(0.0), tx + lw + 8.0, by + 30.0, MUTED);
    }
    if ci.running() && !ci.job.is_empty() {
        let mut x = tx;
        let tail = if ci.steps > 0 { format!("{}/{}", ci.step_no, ci.steps) } else { String::new() };
        let tail_w = if tail.is_empty() { 0.0 } else { c.measure(F::Mono, &tail, 13.0) + 12.0 };
        let room = bw - 48.0 - 16.0 - tail_w;
        let job_w = c.clip(F::Mono, &ci.job, 14.0, room * 0.45, x, by + 56.0, TEXT_2);
        x += job_w;
        if !ci.step.is_empty() {
            x += c.text(F::Bold, "  ›  ", 16.0, x, by + 56.0, MUTED, Align::Left);
            x += c.clip(F::Mono, &ci.step, 14.0, (tx + room - x).max(0.0), x, by + 56.0, TEXT);
        }
        if !tail.is_empty() {
            c.text(F::Mono, &tail, 13.0, x + 12.0, by + 56.0, FAINT, Align::Left);
        }
    } else {
        let title = if ci.branch.is_empty() { ci.title.clone() } else { format!("{}  ·  {}", ci.title, ci.branch) };
        c.clip(F::Medium, &title, 15.0, bw - 64.0, tx, by + 56.0, MUTED);
    }
}

/// The project's deploy state: running, or how the last one went.
fn deploy_line(c: &mut Canvas, v: &View, p: &Project, x: f32, y: f32, w: f32) {
    let d = v.deploy;
    let now = v.now_ms;
    let (sym, col, text, text_col) = if let Some(r) = d.running().filter(|r| r.project == p.name) {
        let t = if r.cancelled { format!("Cancelling…  {}", mmss(r.secs(now))) } else { format!("Deploying…  {}", mmss(r.secs(now))) };
        ("paperplane.fill", if v.tick { ORANGE } else { 0x9A5640 }, t, ORANGE)
    } else {
        match &p.last {
            Some(l) if l.ok => ("checkmark.circle.fill", GREEN, format!("Deployed {}  ·  took {}", ago(now.saturating_sub(l.ended_ms)), mmss(l.secs as f64)), TEXT_2),
            Some(l) if l.cancelled => ("minus.circle.fill", MUTED, format!("Deploy cancelled {}", ago(now.saturating_sub(l.ended_ms))), MUTED),
            Some(l) => {
                let why = match l.exit {
                    Some(code) => format!("exit {code}"),
                    None => "killed".into(),
                };
                ("xmark.circle.fill", RED, format!("Deploy failed {}  ·  {why}", ago(now.saturating_sub(l.ended_ms))), 0xFF9B9B)
            }
            None => ("paperplane", FAINT, "Not deployed from the Quake yet".into(), FAINT),
        }
    };
    c.symbol(sym, 18.0, x + 9.0, y - 6.0, col);
    c.clip(F::Medium, &text, 17.0, w - 30.0, x + 28.0, y, text_col);
}

/// Log layout, left: every project as a compact row.
fn project_list(c: &mut Canvas, v: &View) {
    let d = v.deploy;
    let now = v.now_ms;
    c.rrect(list_panel(), 14.0, PANEL_DEEP, 1.0);
    let n = d.projects.len();
    let first = list_window(n, v.tile, ROWS);
    for slot in 0..ROWS {
        let i = first + slot;
        let Some(p) = d.projects.get(i) else { break };
        let r = row_rect(slot);
        let (x, y, _, h) = r;
        let selected = v.level != &Level::Top && v.tile == i;
        let running = is_running(d, p);
        c.rrect(r, 10.0, if selected { 0x22222A } else if running { 0x1E1712 } else { PANEL }, 1.0);
        c.rrect((x, y + 10.0, 5.0, h - 20.0), 2.5, tone(d, i, p, now), 1.0);
        let ci = row_ci(r);
        let name_w = ci.0 - x - 30.0;
        c.fit(F::Bold, &p.name, 21.0, name_w, x + 20.0, y + 31.0, if running { ORANGE } else { TEXT }, Align::Left);
        if let Some(e) = &p.git_error {
            c.clip(F::Medium, e, 14.0, name_w, x + 20.0, y + 57.0, FAINT);
        } else {
            git_line(c, p, x + 20.0, y + 57.0, name_w, 15.0);
        }
        // CI chip (tap opens the run).
        c.rrect(ci, 9.0, BOX, 1.0);
        match &p.ci {
            Some(run) => {
                let (sym, col, label) = ci_look(run, v.tick);
                c.symbol(sym, 17.0, ci.0 + 20.0, ci.1 + ci.3 / 2.0, col);
                let short = label.trim_start_matches("CI ");
                let tail = if run.running() { mmss(now.saturating_sub(run.created_ms) as f64 / 1000.0) } else { age(now.saturating_sub(run.created_ms)) };
                let tw = c.text(F::Medium, &tail, 14.0, ci.0 + ci.2 - 12.0, ci.1 + ci.3 / 2.0 + 5.0, FAINT, Align::Right);
                c.clip(F::Bold, short, 15.0, ci.2 - 50.0 - tw, ci.0 + 36.0, ci.1 + ci.3 / 2.0 + 5.0, col);
            }
            None => {
                c.symbol("circle.dashed", 16.0, ci.0 + 20.0, ci.1 + ci.3 / 2.0, FAINT);
                c.text(F::Medium, if p.repo.is_empty() { "no CI" } else { "no runs" }, 14.0, ci.0 + 36.0, ci.1 + ci.3 / 2.0 + 5.0, FAINT, Align::Left);
            }
        }
        if selected {
            c.outline(r, 10.0, TEXT, 2.0);
        }
    }
    let more = n.saturating_sub(first + ROWS);
    if more > 0 || first > 0 {
        let label = format!("{}–{} of {n}", first + 1, (first + ROWS).min(n));
        c.text(F::Bold, &label, 12.0, M + LIST_W - 18.0, CY + CH - 6.0, FAINT, Align::Right);
    }
    // A message for the board (the strip isn't shown in this layout).
    let mut bottom = CY + CH - 12.0;
    if let Some((t, _)) = d.toast.as_ref().filter(|(_, until)| now < *until) {
        let r = (M + 8.0, CY + CH - 50.0, LIST_W - 16.0, 42.0);
        c.rrect(r, 10.0, 0x2E2608, 1.0);
        c.fit(F::Bold, t, 16.0, r.2 - 28.0, r.0 + r.2 / 2.0, r.1 + 27.0, AMBER, Align::Center);
        bottom = r.1 - 8.0;
    }
    // Room left under the rows: the selected (or deploying) project's last commit and deploy.
    let top = CY + 8.0 + ROWS.min(n - first) as f32 * ROW_H + 4.0;
    let pick = if v.level != &Level::Top { Some(v.tile) } else { d.running().and_then(|r| d.projects.iter().position(|p| p.name == r.project)) };
    let Some(p) = pick.and_then(|i| d.projects.get(i)) else { return };
    if bottom - top < 104.0 {
        return;
    }
    let (x, w) = (M + 28.0, LIST_W - 56.0);
    c.text(F::Bold, "LAST COMMIT", 12.0, x, top + 24.0, FAINT, Align::Left);
    if p.subject.is_empty() {
        c.text(F::Medium, "—", 17.0, x, top + 50.0, FAINT, Align::Left);
    } else {
        let aw = c.text(F::Medium, &age(now.saturating_sub(p.commit_ms)), 15.0, x + w, top + 50.0, FAINT, Align::Right);
        c.clip(F::Medium, &p.subject, 17.0, w - aw - 12.0, x, top + 50.0, TEXT_2);
        c.clip(F::Mono, &format!("{}  ·  {}", p.sha, p.path), 13.0, w, x, top + 72.0, FAINT);
    }
    if bottom - top >= 140.0 {
        deploy_line(c, v, p, x, top + 112.0, w);
    }
}

/// Log layout, right: the running (or last) deploy, its tail, and Cancel / Close.
fn log_view(c: &mut Canvas, v: &View, r: &Run) {
    let now = v.now_ms;
    let d = v.deploy;
    let p = log_panel();
    let (px, py, pw, ph) = p;
    c.rrect(p, 14.0, PANEL_DEEP, 1.0);
    let running = r.running();
    let (label, bg, fg) = if running && r.cancelled {
        ("CANCELLING", 0x3A3A42, TEXT)
    } else if running {
        ("DEPLOYING", ORANGE, 0x1F0E07)
    } else if r.ok() {
        ("DEPLOYED", GREEN, 0xFFFFFF)
    } else if r.cancelled {
        ("CANCELLED", 0x3A3A42, TEXT)
    } else {
        ("FAILED", RED, 0x2A0808)
    };
    let mut x = px + 20.0;
    if running {
        c.circle(x + 7.0, py + 34.0, 6.0, ORANGE, if v.tick { 1.0 } else { 0.3 });
        x += 22.0;
    }
    x += c.pill(x, py + 21.0, label, 14.0, bg, fg) + 14.0;
    x += c.fit(F::Bold, &r.project, 26.0, 360.0, x, py + 43.0, TEXT, Align::Left) + 16.0;

    // Right: button, then time.
    let b = log_button(p);
    let cancel_armed = running && d.armed.as_ref().is_some_and(|a| a.cancel && now < a.until_ms);
    let (btn, bsym, bbg, bfg) = if running && r.cancelled {
        ("Stopping…", "hourglass", 0x26262D, MUTED)
    } else if cancel_armed {
        ("Tap to confirm", "xmark.octagon.fill", ARMED, 0xFFFFFF)
    } else if running {
        ("Cancel", "xmark", 0x4A1A1A, 0xFFD0D0)
    } else {
        ("Close", "xmark", 0x26262D, TEXT)
    };
    c.rrect(b, 10.0, bbg, 1.0);
    c.symbol(bsym, 16.0, b.0 + 24.0, b.1 + b.3 / 2.0, bfg);
    c.fit(F::Medium, btn, 17.0, b.2 - 50.0, b.0 + 42.0, b.1 + 28.0, bfg, Align::Left);
    let mut rx = b.0 - 20.0;
    if running {
        rx -= c.text(F::Bold, &mmss(r.secs(now)), 30.0, rx, py + 45.0, TEXT, Align::Right) + 18.0;
        if !r.cancelled {
            let how = if v.level == &Level::Top { "press, then hold to cancel" } else { "hold to cancel" };
            rx -= c.text(F::Medium, how, 15.0, rx, py + 42.0, FAINT, Align::Right) + 18.0;
        }
    } else {
        let mut parts = vec![format!("took {}", mmss(r.secs(now)))];
        if !r.ok() && !r.cancelled {
            parts.push(match (r.exit, r.signal) {
                (Some(code), _) => format!("exit {code}"),
                (None, Some(sig)) => format!("signal {sig}"),
                _ => "failed".into(),
            });
        }
        parts.push(ago(now.saturating_sub(r.ended_ms.unwrap_or(now))));
        rx -= c.text(F::Medium, &parts.join("  ·  "), 17.0, rx, py + 42.0, MUTED, Align::Right) + 18.0;
    }
    c.clip(F::Mono, &format!("$ {}", r.command), 15.0, (rx - x - 10.0).max(0.0), x, py + 42.0, FAINT);
    c.rect((px + 20.0, py + 66.0, pw - 40.0, 1.0), LINE, 1.0);

    // The tail, newest at the bottom.
    let rows = log_rows();
    if r.lines.is_empty() {
        let msg = if running { "Waiting for output…" } else { "No output" };
        c.text(F::Medium, msg, 20.0, px + pw / 2.0, py + LOG_TOP + (ph - LOG_TOP) / 2.0, FAINT, Align::Center);
        return;
    }
    let start = r.lines.len().saturating_sub(rows);
    let shown = &r.lines[start..];
    for (k, line) in shown.iter().enumerate() {
        let by = py + LOG_TOP + k as f32 * LOG_ROW_H + 15.0;
        let lw = c.clip(F::Mono, line, 14.0, pw - 60.0, px + 24.0, by, log_color(line));
        if running && k + 1 == shown.len() && v.tick {
            c.rect((px + 24.0 + lw + 4.0, by - 13.0, 8.0, 16.0), TEXT_2, 0.8);
        }
    }
    if r.total > rows {
        c.text(F::Bold, &format!("last {} of {} lines", shown.len(), r.total), 12.0, px + pw - 22.0, py + 82.0, FAINT, Align::Right);
    }
}

// ---------------------------------------------------------------- touch

/// Touch on this page: cards / rows arm, CI badges open the run, the armed card's red band
/// confirms, the log's button cancels (twice) or closes.
pub(super) fn hit(v: &View, x: f32, y: f32) -> Option<Hit> {
    let d = v.deploy;
    let n = d.projects.len();
    let t = |t: Target| Some(Hit::Feature(Page::Deploy, t.code()));
    if d.log_shown() {
        if inside(log_button(log_panel()), x, y) {
            return if d.running().is_some() { t(Target::Cancel) } else { t(Target::Close) };
        }
        let first = list_window(n, v.tile, ROWS);
        for slot in 0..ROWS.min(n.saturating_sub(first)) {
            let r = row_rect(slot);
            if inside(row_ci(r), x, y) {
                return t(Target::Ci(first + slot));
            }
            if inside(r, x, y) {
                return t(Target::Card(first + slot));
            }
        }
        return None;
    }
    let (first, cols) = card_window(v);
    for slot in 0..cols.min(n.saturating_sub(first)) {
        let i = first + slot;
        let r = card_rect(slot, cols);
        if d.armed_deploy(v.now_ms).is_some_and(|a| a.index == i) && inside(action_box(r), x, y) {
            return t(Target::Confirm);
        }
        if inside(ci_box(r), x, y) {
            return t(Target::Ci(i));
        }
        if inside(r, x, y) {
            return t(Target::Card(i));
        }
    }
    None
}

/// Where Hit::Feature(_, i) is, for the tap flash.
pub(super) fn rect(v: &View, i: usize) -> Option<R> {
    let d = v.deploy;
    let n = d.projects.len();
    if d.log_shown() {
        let first = list_window(n, v.tile, ROWS);
        return match Target::from_code(i) {
            Target::Cancel | Target::Close => Some(log_button(log_panel())),
            Target::Card(k) => Some(row_rect(k.checked_sub(first).filter(|s| *s < ROWS)?)),
            Target::Ci(k) => Some(row_ci(row_rect(k.checked_sub(first).filter(|s| *s < ROWS)?))),
            Target::Confirm => None,
        };
    }
    let (first, cols) = card_window(v);
    let card = |k: usize| k.checked_sub(first).filter(|s| *s < cols).map(|s| card_rect(s, cols));
    match Target::from_code(i) {
        Target::Card(k) => card(k),
        Target::Ci(k) => card(k).map(ci_box),
        Target::Confirm => d.armed_deploy(v.now_ms).and_then(|a| card(a.index)).map(action_box),
        Target::Cancel | Target::Close => None,
    }
}

pub(super) fn hints(level: &Level) -> &'static str {
    match level {
        Level::Top => "turn  switch page   ·   press  choose a project   ·   tap  arm a deploy",
        _ => "turn  choose   ·   press twice  deploy   ·   hold  cancel   ·   double-click  back",
    }
}

/// The breadcrumb next to the tabs.
pub(super) fn crumb(v: &View) -> Option<String> {
    let d = v.deploy;
    if let Some(a) = d.armed_deploy(v.now_ms) {
        return Some(format!("›  armed  ·  {}", a.name));
    }
    if v.level == &Level::Top {
        return d.running().map(|r| format!("›  deploying {}", r.project));
    }
    d.projects.get(v.tile).map(|p| format!("›  {}", p.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deploy::Armed;

    #[test]
    fn layouts_fit_the_board() {
        for n in [1, 3, 4, 9] {
            let c = cols(n);
            let last = card_rect(c - 1, c);
            assert!((last.0 + last.2 - (W - M)).abs() < 0.5, "{n}: {last:?}");
            assert!(last.1 + last.3 <= H - M + 0.5);
            let ab = action_box(last);
            assert!(ab.1 > ci_box(last).1 + ci_box(last).3 + 20.0);
        }
        let last = row_rect(ROWS - 1);
        assert!(last.1 + last.3 <= CY + CH);
        let lp = log_panel();
        assert!((lp.0 + lp.2 - (W - M)).abs() < 0.5);
        assert!(lp.1 + LOG_TOP + log_rows() as f32 * LOG_ROW_H <= lp.1 + lp.3);
    }

    /// Renders the page's states to PNGs for a look:
    /// QUAKED_DEPLOY_PREVIEW=/tmp/x cargo test --release deploy_states -- --ignored
    #[test]
    #[ignore]
    fn deploy_states() {
        let Ok(dir) = std::env::var("QUAKED_DEPLOY_PREVIEW") else { return };
        std::fs::create_dir_all(&dir).unwrap();
        let fonts = Fonts::load();
        let symbols = crate::symbols::Symbols::default();
        let cfg = crate::config::Config::default();
        let now = crate::status::now_ms();
        let sample = crate::deploy::sample();
        let mut cards_only = sample.clone();
        cards_only.run = None;
        let mut armed = cards_only.clone();
        armed.armed = Some(Armed { index: 1, name: "digin".into(), since_ms: now - 1300, until_ms: now + 2700, ..Default::default() });
        let mut failed = sample.clone();
        if let Some(r) = failed.run.as_mut() {
            r.ended_ms = Some(now - 20_000);
            r.exit = Some(1);
            r.lines.push("Error: ssh: connect to host workbench port 22: Operation timed out".into());
            r.lines.push("deploy.sh: line 14: rsync failed".into());
        }
        let mut cancel = sample.clone();
        cancel.armed = Some(Armed { index: 0, name: "plcplayground".into(), cancel: true, since_ms: now - 900, until_ms: now + 3100, ..Default::default() });
        let mut many = cards_only.clone();
        for k in 0..7 {
            let mut p = many.projects[k % 3].clone();
            p.name = format!("iboughtthisgun-{k}");
            p.git_error = (k == 2).then(|| "not a git repository".to_string());
            p.ci = if k == 3 { None } else { p.ci };
            p.repo = if k == 3 { String::new() } else { p.repo };
            many.projects.push(p);
        }
        let mut one = cards_only.clone();
        one.projects.truncate(1);
        one.toast = Some(("ibtg is still deploying".into(), now + 3000));
        let empty = Status { loaded: true, scan: "~/ArlintDev".into(), ..Default::default() };
        let mut toasted = sample.clone();
        toasted.toast = Some(("plcplayground is deploying  ·  one at a time".into(), now + 3000));
        let shots: Vec<(&str, &Status, Level, usize)> = vec![
            ("a-running", &sample, Level::Focus, 1),
            ("b-cards", &cards_only, Level::Focus, 0),
            ("c-armed", &armed, Level::Focus, 1),
            ("d-failed", &failed, Level::Focus, 0),
            ("e-cancel", &cancel, Level::Top, 0),
            ("f-many", &many, Level::Focus, 6),
            ("g-one", &one, Level::Top, 0),
            ("h-empty", &empty, Level::Top, 0),
            ("i-running-top", &toasted, Level::Top, 0),
        ];
        let (sounds, league, docker, ops, code, digin) = Default::default();
        let history = History::default();
        let (call, focus, paste, windows, calendar, health, night, viz) = Default::default();
        let stocks = crate::stocks::Status::default();
        for (name, st, level, tile) in shots {
            let mut c = Canvas::new(1920, 480, &fonts, &symbols);
            let v = View {
                pages: &cfg.pages,
                page: Page::Deploy,
                accent: crate::config::hex(cfg.ring.color_for(Page::Deploy), 0),
                shortcuts: &cfg.shortcuts,
                sounds: &sounds,
                tile,
                league: &league,
                league_cfg: &cfg.league,
                docker: &docker,
                ops: &ops,
                code: &code,
                digin: &digin,
                settings: SettingsView::default(),
                ring: &cfg.ring,
                history: &history,
                ask: None,
                call: &call,
                focus: &focus,
                deploy: st,
                paste: &paste,
                windows: &windows,
                calendar: &calendar,
                health: &health,
                stocks: &stocks,
                night: &night,
                viz: &viz,
                level: &level,
                sessions: &[],
                sel: 0,
                scroll: 0,
                track: None,
                position: 0.0,
                volume: None,
                problem: None,
                art: None,
                now_ms: now,
                clock: "4:21",
                connected: true,
                flash: None,
                notice: None,
                desk: super::super::Desk { here: "", sides: &Default::default(), macs: Vec::new(), join_on: Default::default() },
                tick: true,
            };
            super::super::draw(&mut c, &v);
            c.pixmap.save_png(format!("{dir}/{name}.png")).unwrap();
        }
    }
}
