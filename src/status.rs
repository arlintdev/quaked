//! Claude Code session state. `quaked hook` is installed as a Claude Code hook and keeps
//! one JSON file per session in ~/.claude/quaked/sessions; the board reads them.
//!
//! Events -> state and steps:
//!   SessionStart        idle (unless already working)
//!   UserPromptSubmit    working; records the prompt
//!   PreToolUse          working; adds a running step (command, file, query...)
//!   PostToolUse         marks that step done (and clears "needs you")
//!   PostToolUseFailure  marks that step failed
//!   Notification        waiting, with the reason; idle reminders change nothing
//!   Stop                done; records Claude's last message from the transcript
//!   SessionEnd          file removed

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const STALE_MS: u64 = 12 * 60 * 60 * 1000; // forget sessions silent for 12 hours
pub const DONE_FADE_MS: u64 = 20 * 60 * 1000; // "done" turns grey after 20 minutes
const MAX_STEPS: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Waiting,
    Working,
    Done,
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Running,
    Ok,
    Error,
    /// Not a tool call: a prompt you sent, or a "needs you" notice.
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Step {
    pub ts: u64,
    /// Short label: "$", "edit", "read", "you", "needs", an MCP tool name...
    pub tag: String,
    pub text: String,
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool_id: String,
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
    pub prompt: String,
    #[serde(default)]
    pub notice: String,
    #[serde(default)]
    pub last_message: String,
    #[serde(default)]
    pub step_count: u32,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub transcript: String,
    /// Which Mac it runs on, for sessions synced from another one ("" = this Mac).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub machine: String,
}

impl Session {
    /// The step in progress, or the latest tool step.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn current_step(&self) -> Option<&Step> {
        self.steps.iter().rev().find(|s| s.status != StepStatus::Info)
    }
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
            let _ = fs::remove_file(p.with_extension("lock"));
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

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
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
    if Path::new(&home()) == Path::new(base) {
        return "~".into();
    }
    Path::new(base).file_name().and_then(|s| s.to_str()).unwrap_or("claude").to_string()
}

/// Collapses whitespace and cuts to `max` characters.
pub fn squash(s: &str, max: usize) -> String {
    let joined = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() > max {
        joined.chars().take(max - 1).collect::<String>() + "…"
    } else {
        joined
    }
}

/// Path relative to the session's folder, or with ~ for home.
fn rel(path: &str, cwd: &str) -> String {
    if !cwd.is_empty() {
        if let Some(r) = path.strip_prefix(cwd).and_then(|r| r.strip_prefix('/')) {
            return r.to_string();
        }
    }
    let h = home();
    if !h.is_empty() {
        if let Some(r) = path.strip_prefix(&h) {
            return format!("~{r}");
        }
    }
    path.to_string()
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn first_string(input: &Value) -> String {
    const KEYS: [&str; 16] = [
        "action_summary", "description", "command", "query", "prompt", "url", "file_path", "path",
        "pattern", "title", "name", "text", "message", "recap", "summary", "content",
    ];
    if let Some(s) = KEYS.iter().map(|k| str_of(input, k)).find(|s| !s.trim().is_empty()) {
        return s.to_string();
    }
    // Anything else: the first non-empty string field.
    input
        .as_object()
        .and_then(|o| o.values().filter_map(Value::as_str).find(|s| !s.trim().is_empty()))
        .unwrap_or("")
        .to_string()
}

/// Shortens paths inside a command: $HOME becomes ~, and Claude Code's per-session
/// temp folders (/private/tmp/claude-<uid>/<project>/<session>/...) become $TMP/...
pub fn shorten_paths(text: &str) -> String {
    let mut out = text.to_string();
    for prefix in ["/private/tmp/claude-", "/tmp/claude-"] {
        while let Some(start) = out.find(prefix) {
            // skip "claude-<uid>/", "<project>/", "<session>/"
            let mut end = start + prefix.len();
            let mut ok = true;
            for _ in 0..3 {
                match out[end..].find('/') {
                    Some(i) => end += i + 1,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                break;
            }
            out.replace_range(start..end, "$TMP/");
        }
    }
    let home = home();
    if !home.is_empty() {
        out = out.replace(&home, "~");
    }
    out
}

/// Turns a tool call into a short tag and a one-line description.
pub fn summarize(tool: &str, input: &Value, cwd: &str) -> (String, String) {
    let file = |key: &str| rel(str_of(input, key), cwd);
    let (tag, text) = match tool {
        "Bash" => ("$".to_string(), str_of(input, "command").to_string()),
        "Edit" | "MultiEdit" => ("edit".into(), file("file_path")),
        "Write" => ("write".into(), file("file_path")),
        "NotebookEdit" => ("edit".into(), file("notebook_path")),
        "Read" => ("read".into(), file("file_path")),
        "Grep" => {
            let p = str_of(input, "path");
            let at = if p.is_empty() { String::new() } else { format!("  in {}", rel(p, cwd)) };
            ("grep".into(), format!("{}{at}", str_of(input, "pattern")))
        }
        "Glob" => ("glob".into(), str_of(input, "pattern").to_string()),
        "WebFetch" => ("web".into(), str_of(input, "url").to_string()),
        "WebSearch" => ("search".into(), str_of(input, "query").to_string()),
        "Task" | "Agent" => ("agent".into(), first_string(input)),
        "Skill" => ("skill".into(), str_of(input, "skill").to_string()),
        "TodoWrite" => ("todo".into(), "update todo list".into()),
        "AskUserQuestion" => (
            "ask".into(),
            input.pointer("/questions/0/question").and_then(Value::as_str).unwrap_or("question").to_string(),
        ),
        t if t.starts_with("mcp__") => {
            let tail = t.trim_start_matches("mcp__");
            let name = tail.split_once("__").map(|(_, n)| n).unwrap_or(tail);
            (name.to_string(), first_string(input))
        }
        t => (t.to_lowercase(), first_string(input)),
    };
    (squash(&tag, 18), squash(&shorten_paths(&text), 240))
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
    transcript_path: String,
    #[serde(default)]
    notification_type: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    tool_input: Value,
    #[serde(default)]
    tool_use_id: String,
    #[serde(default)]
    last_assistant_message: String,
}

/// Claude's most recent text reply, read from the end of the session transcript.
fn last_assistant_text(path: &str) -> String {
    let Ok(mut f) = fs::File::open(path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(512 * 1024);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buf);
    for line in text.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(content) = v.pointer("/message/content").and_then(Value::as_array) else { continue };
        let joined: Vec<&str> = content
            .iter()
            .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|c| c.get("text").and_then(Value::as_str))
            .collect();
        if !joined.is_empty() {
            return squash(&joined.join(" "), 600);
        }
    }
    String::new()
}

fn push_step(s: &mut Session, step: Step) {
    s.steps.push(step);
    if s.steps.len() > MAX_STEPS {
        let extra = s.steps.len() - MAX_STEPS;
        s.steps.drain(..extra);
    }
}

/// Finds the running step this Post*ToolUse belongs to.
fn running_step<'a>(s: &'a mut Session, tool_id: &str, tag: &str) -> Option<&'a mut Step> {
    if !tool_id.is_empty() {
        if let Some(i) = s.steps.iter().rposition(|st| st.tool_id == tool_id) {
            return s.steps.get_mut(i);
        }
    }
    let i = s.steps.iter().rposition(|st| st.status == StepStatus::Running && st.tag == tag)?;
    s.steps.get_mut(i)
}

fn apply(input: &HookInput, prev: Option<Session>, now: u64) -> Option<Session> {
    let cwd = if input.cwd.is_empty() { prev.as_ref().map(|p| p.cwd.clone()).unwrap_or_default() } else { input.cwd.clone() };
    let mut s = prev.clone().unwrap_or(Session {
        id: input.session_id.clone(),
        name: project_name(&cwd),
        cwd: cwd.clone(),
        state: State::Idle,
        since: now,
        started_at: now,
        updated_at: now,
        prompt: String::new(),
        notice: String::new(),
        last_message: String::new(),
        step_count: 0,
        steps: Vec::new(),
        transcript: String::new(),
        machine: String::new(),
    });
    s.cwd = cwd.clone();
    s.name = project_name(&cwd);
    if !input.transcript_path.is_empty() {
        s.transcript = input.transcript_path.clone();
    }
    let old_state = prev.as_ref().map(|p| p.state);
    let new_state;
    match input.hook_event_name.as_str() {
        "SessionStart" => {
            new_state = if old_state == Some(State::Working) { State::Working } else { State::Idle };
        }
        "UserPromptSubmit" => {
            new_state = State::Working;
            s.prompt = squash(&input.prompt, 600);
            s.notice.clear();
            push_step(&mut s, Step { ts: now, tag: "you".into(), text: squash(&input.prompt, 240), status: StepStatus::Info, tool_id: String::new() });
        }
        "PreToolUse" => {
            new_state = State::Working;
            let (tag, text) = summarize(&input.tool_name, &input.tool_input, &cwd);
            s.step_count += 1;
            push_step(&mut s, Step { ts: now, tag, text, status: StepStatus::Running, tool_id: input.tool_use_id.clone() });
        }
        "PostToolUse" | "PostToolUseFailure" => {
            new_state = State::Working;
            s.notice.clear();
            let status = if input.hook_event_name == "PostToolUse" { StepStatus::Ok } else { StepStatus::Error };
            let (tag, text) = summarize(&input.tool_name, &input.tool_input, &cwd);
            match running_step(&mut s, &input.tool_use_id, &tag) {
                Some(step) => step.status = status,
                None => {
                    // PreToolUse didn't record it (older hook config): add it now.
                    s.step_count += 1;
                    push_step(&mut s, Step { ts: now, tag, text, status, tool_id: input.tool_use_id.clone() });
                }
            }
        }
        "Notification" => {
            let idle = input.notification_type == "idle_prompt"
                || input.notification_type == "auth_success"
                || input.message.to_lowercase().contains("waiting for your input");
            if idle {
                return None;
            }
            new_state = State::Waiting;
            s.notice = squash(&input.message, 300);
            let text = s.notice.clone();
            push_step(&mut s, Step { ts: now, tag: "needs".into(), text, status: StepStatus::Info, tool_id: String::new() });
        }
        "Stop" => {
            new_state = State::Done;
            s.notice.clear();
            let msg = if input.last_assistant_message.is_empty() { last_assistant_text(&s.transcript) } else { squash(&input.last_assistant_message, 600) };
            if !msg.is_empty() {
                s.last_message = msg;
            }
            // A step still "running" was interrupted or never reported back.
            for st in s.steps.iter_mut().filter(|st| st.status == StepStatus::Running) {
                st.status = StepStatus::Ok;
            }
        }
        _ => return None,
    }
    if old_state != Some(new_state) {
        s.since = now;
    }
    s.state = new_state;
    s.updated_at = now;
    Some(s)
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
    let lock_path = dir.join(format!("{id}.lock"));
    if input.hook_event_name == "SessionEnd" {
        let _ = fs::remove_file(&file);
        let _ = fs::remove_file(&lock_path);
        return;
    }
    // Parallel tool calls fire hooks concurrently; serialize read-modify-write.
    let Ok(lock) = fs::OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path) else { return };
    unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    let prev: Option<Session> = fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok());
    if let Some(s) = apply(&input, prev, now_ms()) {
        let tmp = dir.join(format!(".{id}.{}.tmp", std::process::id()));
        if let Ok(text) = serde_json::to_string(&s) {
            if fs::write(&tmp, text).is_ok() {
                let _ = fs::rename(&tmp, &file);
            }
        }
    }
    drop(lock);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn input(event: &str) -> HookInput {
        HookInput { session_id: "s".into(), hook_event_name: event.into(), cwd: "/w/proj".into(), ..Default::default() }
    }

    #[test]
    fn lifecycle() {
        let mut p = input("UserPromptSubmit");
        p.prompt = "fix   the build".into();
        let s = apply(&p, None, 1).unwrap();
        assert_eq!((s.state, s.prompt.as_str(), s.name.as_str()), (State::Working, "fix the build", "proj"));

        let mut pre = input("PreToolUse");
        pre.tool_name = "Bash".into();
        pre.tool_input = json!({"command": "cargo build --release"});
        pre.tool_use_id = "t1".into();
        let s = apply(&pre, Some(s), 2).unwrap();
        let cur = s.current_step().unwrap();
        assert_eq!((cur.tag.as_str(), cur.text.as_str(), cur.status), ("$", "cargo build --release", StepStatus::Running));

        let mut n = input("Notification");
        n.notification_type = "permission_prompt".into();
        n.message = "Claude needs your permission to use Bash".into();
        let s = apply(&n, Some(s), 3).unwrap();
        assert_eq!(s.state, State::Waiting);

        let mut post = pre;
        post.hook_event_name = "PostToolUse".into();
        let s = apply(&post, Some(s), 4).unwrap();
        assert_eq!((s.state, s.notice.as_str()), (State::Working, ""));
        assert_eq!(s.current_step().unwrap().status, StepStatus::Ok);
        assert_eq!(s.step_count, 1);

        let mut idle = input("Notification");
        idle.notification_type = "idle_prompt".into();
        assert!(apply(&idle, Some(s.clone()), 5).is_none());

        let mut stop = input("Stop");
        stop.last_assistant_message = "Build fixed.".into();
        let s = apply(&stop, Some(s), 6).unwrap();
        assert_eq!((s.state, s.last_message.as_str()), (State::Done, "Build fixed."));
    }

    #[test]
    fn summaries() {
        assert_eq!(summarize("Edit", &json!({"file_path": "/w/proj/src/app.rs"}), "/w/proj"), ("edit".into(), "src/app.rs".into()));
        assert_eq!(summarize("Grep", &json!({"pattern": "fn main"}), "/w/proj"), ("grep".into(), "fn main".into()));
        assert_eq!(
            summarize("mcp__computer-use__app_click", &json!({"action_summary": "Opens settings"}), ""),
            ("app_click".into(), "Opens settings".into())
        );
        assert_eq!(project_name("/u/a/ArlintDev/digin-all/digin/.claude/worktrees/epic-ellis-235fbb"), "digin");
        assert_eq!(
            shorten_paths("S=/private/tmp/claude-501/-Users-a-x/1234-abcd/scratchpad; ls $S"),
            "S=$TMP/scratchpad; ls $S"
        );
        assert_eq!(summarize("mcp__ccd_turn__wrap_up", &json!({"recap": "Done."}), ""), ("wrap_up".into(), "Done.".into()));
    }
}
