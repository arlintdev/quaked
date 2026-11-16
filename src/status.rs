//! Claude Code session state. `quaked hook` is installed as a Claude Code hook and keeps
//! one small JSON file per session in ~/.claude/quaked/sessions; the board reads them.
//!
//! Events -> state:
//!   SessionStart      idle (unless already working)
//!   UserPromptSubmit  working
//!   PostToolUse       working (also clears "needs you" once a permission prompt is answered)
//!   Notification      waiting, except idle reminders, which change nothing
//!   Stop              done
//!   SessionEnd        file removed

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const STALE_MS: u64 = 12 * 60 * 60 * 1000; // forget sessions silent for 12 hours
pub const DONE_FADE_MS: u64 = 20 * 60 * 1000; // "done" turns grey after 20 minutes

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Waiting,
    Working,
    Done,
    Idle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub cwd: String,
    pub state: State,
    pub since: u64,
    pub started_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub detail: String,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn dir() -> PathBuf {
    if let Ok(d) = std::env::var("QUAKED_STATUS_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    Path::new(&home).join(".claude").join("quaked").join("sessions")
}

/// All live sessions, oldest first, with "done" faded to idle after a while and
/// duplicate project names numbered.
pub fn load(now: u64) -> Vec<Session> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir()) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&p) else { continue };
        let Ok(mut s) = serde_json::from_str::<Session>(&text) else { continue };
        if now.saturating_sub(s.updated_at) > STALE_MS {
            let _ = fs::remove_file(&p);
            continue;
        }
        if s.state == State::Done && now.saturating_sub(s.since) > DONE_FADE_MS {
            s.state = State::Idle;
        }
        out.push(s);
    }
    out.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)));
    let mut totals = std::collections::HashMap::<String, usize>::new();
    for s in &out {
        *totals.entry(s.name.clone()).or_default() += 1;
    }
    let mut seen = std::collections::HashMap::<String, usize>::new();
    for s in &mut out {
        if totals[&s.name] > 1 {
            let n = seen.entry(s.name.clone()).or_default();
            *n += 1;
            s.name = format!("{} {}", s.name, n);
        }
    }
    out
}

fn project_name(cwd: &str) -> String {
    if cwd.is_empty() {
        return "claude".into();
    }
    // .../repo/.claude/worktrees/<name> -> repo
    let base = match cwd.find("/.claude/worktrees/") {
        Some(i) => &cwd[..i],
        None => cwd,
    };
    let name = Path::new(base).file_name().and_then(|s| s.to_str()).unwrap_or("claude");
    if std::env::var("HOME").map(|h| Path::new(&h) == Path::new(base)).unwrap_or(false) {
        "~".into()
    } else {
        name.to_string()
    }
}

#[derive(Deserialize, Default)]
struct HookInput {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    hook_event_name: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    notification_type: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    tool_name: String,
}

fn next_state(input: &HookInput, prev: Option<&Session>) -> Option<State> {
    let keep = prev.map(|p| p.state);
    match input.hook_event_name.as_str() {
        "SessionStart" => Some(if keep == Some(State::Working) { State::Working } else { State::Idle }),
        "UserPromptSubmit" | "PostToolUse" => Some(State::Working),
        "Stop" => Some(State::Done),
        "Notification" => {
            let idle = input.notification_type == "idle_prompt"
                || input.notification_type == "auth_success"
                || input.message.to_lowercase().contains("waiting for your input");
            if idle { keep.or(Some(State::Idle)) } else { Some(State::Waiting) }
        }
        _ => keep,
    }
}

fn short_tool(name: &str) -> String {
    // mcp__server__tool -> tool
    match name.strip_prefix("mcp__").and_then(|r| r.split_once("__")) {
        Some((_, tool)) => tool.to_string(),
        None => name.to_string(),
    }
}

/// `quaked hook`: called by Claude Code with the hook payload on stdin. Prints nothing
/// and always exits 0, so it can never block or alter a session.
pub fn hook() {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let input: HookInput = serde_json::from_str(&raw).unwrap_or_default();
    if input.session_id.is_empty() || input.hook_event_name.is_empty() {
        return;
    }
    let dir = dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let id: String = input.session_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    let file = dir.join(format!("{id}.json"));
    if input.hook_event_name == "SessionEnd" {
        let _ = fs::remove_file(&file);
        return;
    }
    let prev: Option<Session> = fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok());
    let Some(state) = next_state(&input, prev.as_ref()) else { return };
    let now = now_ms();
    let changed = prev.as_ref().map(|p| p.state != state).unwrap_or(true);
    let detail = if input.hook_event_name == "PostToolUse" {
        short_tool(&input.tool_name)
    } else if state == State::Working {
        prev.as_ref().map(|p| p.detail.clone()).unwrap_or_default()
    } else {
        String::new()
    };
    // Tool calls while already working only rewrite the file now and then.
    if let Some(p) = &prev {
        if !changed && input.hook_event_name == "PostToolUse" && p.detail == detail && now.saturating_sub(p.updated_at) < 15_000 {
            return;
        }
    }
    let cwd = if input.cwd.is_empty() { prev.as_ref().map(|p| p.cwd.clone()).unwrap_or_default() } else { input.cwd.clone() };
    let s = Session {
        id: input.session_id.clone(),
        name: project_name(&cwd),
        cwd,
        state,
        since: if changed { now } else { prev.as_ref().map(|p| p.since).unwrap_or(now) },
        started_at: prev.as_ref().map(|p| p.started_at).unwrap_or(now),
        updated_at: now,
        detail,
    };
    let tmp = dir.join(format!(".{id}.{}.tmp", std::process::id()));
    if let Ok(text) = serde_json::to_string(&s) {
        if fs::write(&tmp, text).is_ok() {
            let _ = fs::rename(&tmp, &file);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(event: &str) -> HookInput {
        HookInput { session_id: "s".into(), hook_event_name: event.into(), ..Default::default() }
    }

    #[test]
    fn states() {
        assert_eq!(next_state(&input("UserPromptSubmit"), None), Some(State::Working));
        assert_eq!(next_state(&input("Stop"), None), Some(State::Done));
        let mut n = input("Notification");
        n.notification_type = "permission_prompt".into();
        assert_eq!(next_state(&n, None), Some(State::Waiting));
        n.notification_type = "idle_prompt".into();
        let done = Session {
            id: "s".into(), name: "x".into(), cwd: String::new(), state: State::Done,
            since: 0, started_at: 0, updated_at: 0, detail: String::new(),
        };
        assert_eq!(next_state(&n, Some(&done)), Some(State::Done));
    }

    #[test]
    fn names() {
        assert_eq!(project_name("/Users/a/ArlintDev/digin-all/digin/.claude/worktrees/epic-ellis-235fbb"), "digin");
        assert_eq!(project_name("/Users/a/ArlintDev/quaked"), "quaked");
        assert_eq!(short_tool("mcp__computer-use__app_click"), "app_click");
        assert_eq!(short_tool("Bash"), "Bash");
    }
}
