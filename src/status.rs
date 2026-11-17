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
    /// What Claude last said during this turn (its narration between tool calls).
    #[serde(default)]
    pub comment: String,
    /// Which Mac it runs on, for sessions synced from another one ("" = this Mac).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub machine: String,
    /// Claude's task list for this work (the desktop app's plan, or TodoWrite).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todos: Vec<Todo>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoState {
    #[default]
    Pending,
    InProgress,
    Completed,
}

/// One item of a session's task list.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Todo {
    pub text: String,
    pub state: TodoState,
}

/// Claude's task list rebuilt from the session's transcript: its last full plan, then the
/// changes after it. For a plan set before quaked started following it (an update, a
/// missed hook). Reads backwards from the end in 1 MB chunks, at most 48 MB, and only
/// parses lines that mention a plan tool. Also returns the tool-use ids it replayed.
pub fn todos_from_transcript(path: &str) -> (Vec<Todo>, std::collections::HashSet<String>) {
    use std::io::{Read, Seek, SeekFrom};
    const CHUNK: u64 = 1 << 20;
    const MAX: u64 = 48 << 20;
    let mut seen = std::collections::HashSet::new();
    let Ok(mut f) = fs::File::open(path) else { return (Vec::new(), seen) };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let is_plan = |name: &str| name.ends_with("__update_plan") || name == "TodoWrite" || name == NEXT_STEP_TOOL;
    // Newest first, up to and including the last full list.
    let mut calls: Vec<(String, Value)> = Vec::new();
    let mut carry: Vec<u8> = Vec::new();
    let mut end = len;
    let mut full = false;
    while end > 0 && len - end < MAX && !full {
        let start = end.saturating_sub(CHUNK);
        let mut buf = vec![0; (end - start) as usize];
        if f.seek(SeekFrom::Start(start)).is_err() || f.read_exact(&mut buf).is_err() {
            break;
        }
        buf.extend_from_slice(&carry);
        // A line cut by the chunk's start waits for the next (earlier) chunk.
        let split = if start == 0 { 0 } else { buf.iter().position(|&b| b == b'\n').map_or(buf.len(), |i| i + 1) };
        carry = buf[..split].to_vec();
        let text = String::from_utf8_lossy(&buf[split..]).into_owned();
        'lines: for line in text.lines().rev() {
            if !line.contains("\"assistant\"") || !(line.contains("update_plan") || line.contains("TodoWrite") || line.contains(NEXT_STEP_TOOL)) {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if v["type"] != "assistant" {
                continue;
            }
            for b in v["message"]["content"].as_array().into_iter().flatten().rev() {
                let name = b["name"].as_str().unwrap_or("");
                if b["type"] != "tool_use" || !is_plan(name) {
                    continue;
                }
                let id = b["id"].as_str().unwrap_or("").to_string();
                if !seen.insert(id) {
                    continue;
                }
                let whole = name == "TodoWrite" || (name.ends_with("__update_plan") && b["input"].get("items").is_some());
                calls.push((name.to_string(), b["input"].clone()));
                if whole {
                    full = true;
                    break 'lines;
                }
            }
        }
        end = start;
    }
    let mut todos = Vec::new();
    if full {
        for (name, input) in calls.iter().rev() {
            update_todos(&mut todos, name, input);
        }
    }
    (todos, seen)
}

/// Follows Claude's task list through its tool calls: the desktop app's plan tool (a whole
/// list, or advance / done / start on it), the CLI's TodoWrite, and the next-step tool,
/// which checks everything off. Returns whether it changed.
pub fn update_todos(todos: &mut Vec<Todo>, tool: &str, input: &Value) -> bool {
    let state = |s: &str| match s {
        "completed" => TodoState::Completed,
        "in_progress" => TodoState::InProgress,
        _ => TodoState::Pending,
    };
    let list = |items: &[Value], text_key: &str| -> Vec<Todo> {
        items
            .iter()
            .filter_map(|i| {
                let text = i.get(text_key)?.as_str()?.trim();
                (!text.is_empty()).then(|| Todo { text: squash(text, 140), state: state(i.get("status").and_then(Value::as_str).unwrap_or("")) })
            })
            .take(16)
            .collect()
    };
    if tool.ends_with("__update_plan") {
        if let Some(items) = input.get("items").and_then(Value::as_array) {
            *todos = list(items, "content");
            return true;
        }
        let before = todos.clone();
        if input.get("advance").and_then(Value::as_bool) == Some(true) {
            if let Some(t) = todos.iter_mut().find(|t| t.state == TodoState::InProgress) {
                t.state = TodoState::Completed;
            }
            if let Some(t) = todos.iter_mut().find(|t| t.state == TodoState::Pending) {
                t.state = TodoState::InProgress;
            }
        }
        for n in input.get("done").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_u64) {
            if let Some(t) = (n as usize).checked_sub(1).and_then(|i| todos.get_mut(i)) {
                t.state = TodoState::Completed;
            }
        }
        if let Some(t) = input.get("start").and_then(Value::as_u64).and_then(|n| (n as usize).checked_sub(1)).and_then(|i| todos.get_mut(i)) {
            t.state = TodoState::InProgress;
        }
        return *todos != before;
    }
    if tool == "TodoWrite" {
        if let Some(items) = input.get("todos").and_then(Value::as_array) {
            *todos = list(items, "content");
            return true;
        }
    }
    if tool == NEXT_STEP_TOOL && todos.iter().any(|t| t.state != TodoState::Completed) {
        for t in todos.iter_mut() {
            t.state = TodoState::Completed;
        }
        return true;
    }
    false
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
/// A prompt without the `<system-reminder>…</system-reminder>` blocks apps inject into it
/// (cut-off ones included), so the board shows what you typed.
pub fn typed_prompt(s: &str) -> String {
    // Blocks Claude Code and the desktop app put into prompts themselves.
    let mut text = s.to_string();
    for tag in ["system-reminder", "task-notification", "side-sessions-event", "ci-monitor-event", "bash-stdout", "bash-stderr", "local-command-stdout", "command-message"] {
        text = strip_blocks(&text, tag);
    }
    // A shell command you ran in the app ("!ls") reads as "$ ls"; a slash command as itself.
    text = text.replace("<bash-input>", "$ ").replace("</bash-input>", "");
    for tag in ["command-name", "command-args"] {
        text = text.replace(&format!("<{tag}>"), " ").replace(&format!("</{tag}>"), " ");
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ").trim().to_string()
}

/// `s` without any <tag>...</tag> blocks (an unclosed one runs to the end).
fn strip_blocks(s: &str, tag: &str) -> String {
    let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find(&open) {
        out.push_str(&rest[..start]);
        match rest[start..].find(&close) {
            Some(end) => rest = &rest[start + end + close.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

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

/// What Claude has said most recently in the current turn ("Now let me check…"): the
/// newest assistant text after your latest prompt, from the end of the transcript.
fn turn_comment(path: &str) -> String {
    let Ok(mut f) = fs::File::open(path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if f.seek(SeekFrom::Start(len.saturating_sub(192 * 1024))).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buf);
    for line in text.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let content = v.pointer("/message/content");
        match v.get("type").and_then(Value::as_str) {
            // A prompt you typed (not a tool result) starts the turn: nothing said since.
            Some("user") => {
                let typed = match content {
                    Some(Value::String(_)) => true,
                    Some(Value::Array(a)) => a.iter().any(|c| c.get("type").and_then(Value::as_str) == Some("text")),
                    _ => false,
                };
                if typed {
                    return String::new();
                }
            }
            Some("assistant") => {
                let said: Vec<&str> = content
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter(|c| c.get("type").and_then(Value::as_str) == Some("text")).filter_map(|c| c.get("text").and_then(Value::as_str)).collect())
                    .unwrap_or_default();
                let said = said.join(" ");
                if !said.trim().is_empty() {
                    return squash(&said, 300);
                }
            }
            _ => {}
        }
    }
    String::new()
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
        comment: String::new(),
        machine: String::new(),
        todos: Vec::new(),
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
        "UserPromptSubmit" if typed_prompt(&input.prompt).is_empty() && !input.prompt.trim().is_empty() => {
            // Woken by Claude Code itself (a finished background task, a hook): not a
            // prompt from you, so the prompt and the turn's start stay as they were.
            new_state = State::Working;
        }
        "UserPromptSubmit" => {
            new_state = State::Working;
            s.prompt = squash(&typed_prompt(&input.prompt), 600);
            s.notice.clear();
            s.comment.clear();
            // A finished list belongs to the last piece of work.
            if s.todos.iter().all(|t| t.state == TodoState::Completed) {
                s.todos.clear();
            }
            push_step(&mut s, Step { ts: now, tag: "you".into(), text: squash(&typed_prompt(&input.prompt), 240), status: StepStatus::Info, tool_id: String::new() });
        }
        "PreToolUse" => {
            new_state = State::Working;
            let said = turn_comment(&s.transcript);
            if !said.is_empty() {
                s.comment = said;
            }
            let (tag, text) = summarize(&input.tool_name, &input.tool_input, &cwd);
            s.step_count += 1;
            push_step(&mut s, Step { ts: now, tag, text, status: StepStatus::Running, tool_id: input.tool_use_id.clone() });
        }
        "PostToolUse" | "PostToolUseFailure" => {
            new_state = State::Working;
            s.notice.clear();
            let status = if input.hook_event_name == "PostToolUse" { StepStatus::Ok } else { StepStatus::Error };
            if status == StepStatus::Ok {
                let partial = input.tool_name.ends_with("__update_plan") && input.tool_input.get("items").is_none();
                if partial && s.todos.is_empty() && !s.transcript.is_empty() {
                    // A change to a list we never saw: rebuild it from the transcript.
                    let (todos, replayed) = todos_from_transcript(&s.transcript);
                    s.todos = todos;
                    if !replayed.contains(&input.tool_use_id) {
                        update_todos(&mut s.todos, &input.tool_name, &input.tool_input);
                    }
                } else {
                    update_todos(&mut s.todos, &input.tool_name, &input.tool_input);
                }
            }
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
            s.comment.clear(); // the final reply shows instead
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

/// `quaked ask`: a PreToolUse hook for AskUserQuestion. Shows the question on the Quake
/// (on whichever Mac has it) and waits for you to spin to an option and press. Prints the
/// answer for Claude Code; prints nothing (so Claude asks as usual) if you choose "Answer
/// in Claude", don't answer in time, or no Quake is around.
pub fn ask_hook() {
    use std::io::Read;
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let Ok(input) = serde_json::from_str::<Value>(&raw) else { return };
    if input.get("tool_name").and_then(Value::as_str) != Some("AskUserQuestion") {
        return;
    }
    let tool_input = input.get("tool_input").cloned().unwrap_or(Value::Null);
    if tool_input.get("questions").and_then(Value::as_array).is_none_or(|q| q.is_empty()) {
        return;
    }
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or("");
    let payload = serde_json::json!({"session": project_name(cwd), "questions": tool_input["questions"]});
    let Some(answer) = ask_board(&payload) else { return };
    let Some(answers) = answer.get("answers").filter(|a| a.as_object().is_some_and(|o| !o.is_empty())) else { return };
    // updatedInput replaces the tool's input: the questions as they were, plus the answers.
    let mut updated = tool_input.clone();
    updated["answers"] = answers.clone();
    let out = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": "Answered on the Quake",
            "updatedInput": updated,
        }
    });
    println!("{out}");
}

/// Sends a question to the board (through the control socket; the daemon passes it to the
/// Mac with the Quake if it isn't here) and waits for the reply line.
fn ask_board(payload: &Value) -> Option<Value> {
    use std::io::{BufRead, BufReader, Write};
    let mut sock = std::os::unix::net::UnixStream::connect(crate::app::ctl_socket_path()).ok()?;
    let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(595)));
    writeln!(sock, "ask {payload}").ok()?;
    let _ = sock.shutdown(std::net::Shutdown::Write);
    let mut reply = String::new();
    BufReader::new(&sock).read_line(&mut reply).ok()?;
    serde_json::from_str(reply.trim()).ok()
}

/// A permission rule suggestion as one line ("Bash(npm run test:*)").
fn suggestion_label(suggestions: &Value) -> String {
    let rules: Vec<String> = suggestions
        .as_array()
        .map(|a| {
            a.iter()
                .flat_map(|s| s.get("rules").and_then(Value::as_array).cloned().unwrap_or_default())
                .map(|r| {
                    let tool = r.get("toolName").and_then(Value::as_str).unwrap_or("");
                    match r.get("ruleContent").and_then(Value::as_str) {
                        Some(c) if !c.is_empty() => format!("{tool}({c})"),
                        _ => tool.to_string(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    rules.join(", ")
}

/// `quaked permit`: a PermissionRequest hook. Shows what Claude wants to do on the Quake
/// with Allow / Always allow / Deny; prints the decision for Claude Code, or nothing (so
/// Claude Code asks as usual) if you hand it back, don't answer in time, or no Quake is
/// around.
pub fn permit_hook() {
    use std::io::Read;
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let Ok(input) = serde_json::from_str::<Value>(&raw) else { return };
    let tool = input.get("tool_name").and_then(Value::as_str).unwrap_or("").to_string();
    if tool.is_empty() {
        return;
    }
    let tool_input = input.get("tool_input").cloned().unwrap_or(Value::Null);
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or("");
    let session_id = input.get("session_id").and_then(Value::as_str).unwrap_or("");
    // Claude's multiple-choice question: answer it with the picked labels.
    if tool == "AskUserQuestion" {
        if tool_input.get("questions").and_then(Value::as_array).is_none_or(|q| q.is_empty()) {
            return;
        }
        let payload = serde_json::json!({"session": project_name(cwd), "sessionId": session_id, "questions": tool_input["questions"]});
        let Some(reply) = ask_board(&payload) else { return };
        let Some(answers) = reply.get("answers").filter(|a| a.as_object().is_some_and(|o| !o.is_empty())) else { return };
        let mut updated = tool_input.clone();
        updated["answers"] = answers.clone();
        println!("{}", serde_json::json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "allow", "updatedInput": updated}}}));
        return;
    }
    let (tag, detail) = summarize(&tool, &tool_input, cwd);
    let suggestions = input.get("permission_suggestions").cloned().unwrap_or(Value::Null);
    let rule = suggestion_label(&suggestions);
    let payload = serde_json::json!({
        "kind": "permission",
        "session": project_name(cwd),
        "sessionId": session_id,
        "tool": tool,
        "tag": tag,
        "detail": detail,
        "description": tool_input.get("description").and_then(Value::as_str).unwrap_or(""),
        "rule": rule,
    });
    let Some(reply) = ask_board(&payload) else { return };
    let Some(choice) = reply.get("answers").and_then(Value::as_object).and_then(|a| a.values().next()).and_then(Value::as_str) else { return };
    let decision = match choice {
        "Allow" => serde_json::json!({"behavior": "allow"}),
        "Always allow" => serde_json::json!({"behavior": "allow", "updatedPermissions": suggestions}),
        "Deny" => serde_json::json!({"behavior": "deny", "message": "Denied on the Quake."}),
        _ => return,
    };
    println!("{}", serde_json::json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": decision}}));
}

/// The buttons of a next-step box as labels: a reply's own words, or an app action's name.
pub fn next_step_options(actions: &Value) -> Vec<String> {
    let name = |a: &str| match a {
        "commit" => "Commit".to_string(),
        "create_pr" => "Create a PR".to_string(),
        "merge" => "Merge".to_string(),
        "fix_ci" => "Fix CI".to_string(),
        "address_comments" => "Address comments".to_string(),
        "resolve_conflicts" => "Resolve conflicts".to_string(),
        "verify" => "Verify".to_string(),
        "archive" => "Archive".to_string(),
        "new_session" => "New session".to_string(),
        other => other.replace('_', " "),
    };
    actions
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|b| match b.get("action").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    Some(action) => Some(name(action)),
                    None => b.get("label").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from),
                })
                .take(4)
                .collect()
        })
        .unwrap_or_default()
}

/// What the session is told when you answer its next-step question on the Quake.
pub fn next_step_wake(question: &str, label: &str) -> String {
    format!("The user answered your next-step question on their Quake desk screen. Question: \"{question}\". Their answer: \"{label}\". Treat it as their reply and carry on.")
}

/// `quaked nextstep`: a background (asyncRewake) PostToolUse hook on the desktop app's
/// next-step tool. Shows its question on the Quake next to the app's own buttons; when you
/// answer on the Quake it exits 2 with your answer on stderr, which wakes the session with
/// it. Answered in the app first, dismissed or timed out: exits 0 and says nothing.
pub fn next_step_hook() {
    use std::io::Read;
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let Ok(input) = serde_json::from_str::<Value>(&raw) else { return };
    let tool_input = input.get("tool_input").cloned().unwrap_or(Value::Null);
    let question = tool_input.get("ask").and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
    let options = next_step_options(tool_input.get("actions").unwrap_or(&Value::Null));
    if question.is_empty() || options.is_empty() {
        return;
    }
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or("");
    let session_id = input.get("session_id").and_then(Value::as_str).unwrap_or("");
    let payload = serde_json::json!({
        "kind": "next",
        "session": project_name(cwd),
        "sessionId": session_id,
        "questions": [{"question": question, "header": "Next step", "options": options, "multiSelect": false}],
    });
    let Some(reply) = ask_board(&payload) else { return };
    let Some(label) = reply.get("answers").and_then(Value::as_object).and_then(|a| a.values().next()).and_then(Value::as_str) else { return };
    eprintln!("{}", next_step_wake(&question, label));
    std::process::exit(2);
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

/// The Claude Code events quaked listens to, and whether the entry takes a tool matcher.
/// The hooks quaked installs: (event, tool matcher, `quaked` subcommand, timeout seconds).
/// `quaked permit` mirrors Claude's questions and permission prompts on the Quake, next to
/// Claude's own prompt (whichever you answer first applies), so it may wait.
/// (event, matcher, quaked subcommand, timeout, runs in the background and may wake the
/// session: asyncRewake).
const HOOK_EVENTS: [(&str, Option<&str>, &str, u32, bool); 10] = [
    ("SessionStart", None, "hook", 5, false),
    ("UserPromptSubmit", None, "hook", 5, false),
    ("PreToolUse", Some("*"), "hook", 5, false),
    ("PermissionRequest", Some("*"), "permit", 600, false),
    ("PostToolUse", Some("*"), "hook", 5, false),
    // Claude's next-step question (the desktop app's box): shown on the Quake in the
    // background; a knob answer wakes the session with it.
    ("PostToolUse", Some(NEXT_STEP_TOOL), "nextstep", 600, true),
    ("PostToolUseFailure", Some("*"), "hook", 5, false),
    ("Notification", None, "hook", 5, false),
    ("Stop", None, "hook", 5, false),
    ("SessionEnd", None, "hook", 5, false),
];

/// The desktop app's "next step" tool: a recap, and maybe a question with buttons.
pub const NEXT_STEP_TOOL: &str = "mcp__ccd_turn__wrap_up";

/// Adds quaked's hooks to `settings` where they aren't yet (`binary` is the path to the
/// quaked binary). Returns what it added. Everything else in the file is left as it is.
fn merge_hooks(settings: &mut serde_json::Value, binary: &str) -> Result<Vec<String>, String> {
    use serde_json::{json, Value};
    let root = settings.as_object_mut().ok_or("settings.json isn't a JSON object")?;
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().ok_or("\"hooks\" in settings.json isn't an object")?;
    let mut added = Vec::new();
    // `quaked ask` ran as a PreToolUse hook, which holds Claude's own prompt back: remove it.
    for groups in hooks.values_mut().filter_map(Value::as_array_mut) {
        let before = groups.len();
        groups.retain(|g| !g.get("hooks").and_then(Value::as_array).is_some_and(|hs| hs.iter().any(|h| h.get("command").and_then(Value::as_str).is_some_and(|c| c.ends_with("quaked ask")))));
        if groups.len() != before {
            added.push("removed the old blocking question hook".to_string());
        }
    }
    hooks.retain(|_, g| g.as_array().is_none_or(|a| !a.is_empty()));
    for (event, matcher, sub, timeout, rewake) in HOOK_EVENTS {
        let groups = hooks.entry(event).or_insert_with(|| json!([]));
        let groups = groups.as_array_mut().ok_or(format!("hooks.{event} isn't a list"))?;
        let wanted = format!("quaked {sub}");
        let present = groups.iter().any(|g| {
            g.get("hooks").and_then(Value::as_array).is_some_and(|hs| {
                hs.iter().any(|h| h.get("command").and_then(Value::as_str).is_some_and(|c| c.ends_with(&wanted) || c.contains(&format!("{wanted} "))))
            })
        });
        if present {
            continue;
        }
        let mut entry = json!({"type": "command", "command": format!("{binary} {sub}"), "timeout": timeout});
        if rewake {
            entry["asyncRewake"] = json!(true);
        }
        groups.push(match matcher {
            Some(m) => json!({"matcher": m, "hooks": [entry]}),
            None => json!({"hooks": [entry]}),
        });
        added.push(if sub == "hook" { event.to_string() } else { format!("{event} ({sub})") });
    }
    Ok(added)
}

/// `quaked hooks`: makes Claude Code report sessions to quaked on this Mac.
pub fn install_hooks() -> Result<String, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME isn't set")?;
    let path = Path::new(&home).join(".claude").join("settings.json");
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "{}".into(),
        Err(e) => return Err(format!("can't read {}: {e}", path.display())),
    };
    // Never rewrite a file we can't parse.
    let mut settings: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{} isn't valid JSON ({e}); left it alone", path.display()))?;
    let binary = format!("{home}/.local/bin/quaked");
    let added = merge_hooks(&mut settings, &binary)?;
    if added.is_empty() {
        return Ok(format!("Claude Code hooks already in {}", path.display()));
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    if path.exists() {
        let backup = path.with_extension("json.before-quaked");
        if !backup.exists() {
            fs::copy(&path, &backup).map_err(|e| format!("can't back up settings.json: {e}"))?;
        }
    }
    let tmp = path.with_extension("json.quaked-tmp");
    let out = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())? + "\n";
    fs::write(&tmp, out).and_then(|_| fs::rename(&tmp, &path)).map_err(|e| format!("can't write {}: {e}", path.display()))?;
    Ok(format!("Added Claude Code hooks to {} ({})", path.display(), added.join(", ")))
}

#[cfg(test)]
mod tests {
    #[test]
    fn prompts_lose_injected_reminders() {
        use super::*;
        assert_eq!(typed_prompt("<system-reminder>linked sessions</system-reminder> fix the build"), "fix the build");
        assert_eq!(typed_prompt("deploy it <system-reminder>cut off"), "deploy it");
        assert_eq!(typed_prompt("plain"), "plain");
    }

    #[test]
    fn narration_is_the_newest_text_in_this_turn() {
        use super::*;
        let dir = std::env::temp_dir().join(format!("quaked-narration-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let lines = [
            r#"{"type":"user","message":{"role":"user","content":"fix the build"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Let me look at the error first."},{"type":"tool_use","name":"Bash"}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"error: x"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read"}]}}"#,
        ];
        fs::write(&path, lines.join("\n")).unwrap();
        assert_eq!(turn_comment(path.to_str().unwrap()), "Let me look at the error first.");
        let next = format!("{}\n{}", lines.join("\n"), r#"{"type":"user","message":{"content":[{"type":"text","text":"now deploy"}]}}"#);
        fs::write(&path, next).unwrap();
        assert_eq!(turn_comment(path.to_str().unwrap()), "", "a new prompt starts a new turn");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn plans_rebuild_from_the_transcript() {
        let dir = std::env::temp_dir().join(format!("quaked-plan-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let tool = |id: &str, name: &str, input: Value| json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}}).to_string();
        let plan = "mcp__ccd_turn__update_plan";
        let mut lines = vec![
            tool("t0", plan, json!({"items": [{"content": "Old", "status": "in_progress"}]})),
            json!({"type": "user", "message": {"content": "hi"}}).to_string(),
            tool("t1", plan, json!({"items": [{"content": "A", "status": "in_progress"}, {"content": "B", "status": "pending"}, {"content": "C", "status": "pending"}]})),
            tool("t2", "Bash", json!({"command": "ls"})),
            tool("t3", plan, json!({"advance": true})),
        ];
        // A big line in between, so the reader crosses chunks.
        lines.insert(3, json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "x".repeat(1_500_000)}]}}).to_string());
        fs::write(&path, lines.join("\n") + "\n").unwrap();
        let (todos, ids) = todos_from_transcript(path.to_str().unwrap());
        use TodoState::*;
        assert_eq!(todos.iter().map(|t| (t.text.as_str(), t.state)).collect::<Vec<_>>(), [("A", Completed), ("B", InProgress), ("C", Pending)]);
        assert!(ids.contains("t3") && ids.contains("t1") && !ids.contains("t0"));
        assert!(todos_from_transcript("/nonexistent").0.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// QUAKED_TRANSCRIPT=<path> cargo test --release -- --ignored real_transcript --nocapture
    #[test]
    #[ignore]
    fn real_transcript() {
        let path = std::env::var("QUAKED_TRANSCRIPT").expect("QUAKED_TRANSCRIPT");
        let t = std::time::Instant::now();
        let (todos, ids) = todos_from_transcript(&path);
        eprintln!("{} todos from {} plan calls in {:?}", todos.len(), ids.len(), t.elapsed());
        for x in &todos {
            eprintln!("  {:?} {}", x.state, x.text);
        }
    }

    #[test]
    fn injected_blocks_are_not_your_prompt() {
        assert_eq!(typed_prompt("<task-notification>\n<task-id>b0ea</task-id>\n</task-notification>"), "");
        assert_eq!(typed_prompt("push it <system-reminder>x</system-reminder>"), "push it");
        assert_eq!(typed_prompt("<side-sessions-event kind=\"done\">y</side-sessions-event> ok"), "ok");
        assert_eq!(typed_prompt("<bash-input>docker ps</bash-input><bash-stdout>CONTAINER</bash-stdout><bash-stderr></bash-stderr>"), "$ docker ps");
        assert_eq!(typed_prompt("<command-message>compact</command-message><command-name>/compact</command-name><command-args>keep it short</command-args>"), "/compact keep it short");
    }

    #[test]
    fn task_lists_follow_the_plan_tools() {
        use TodoState::*;
        let mut t = Vec::new();
        let plan = "mcp__ccd_turn__update_plan";
        assert!(update_todos(&mut t, plan, &json!({"items": [{"content": "Read", "status": "in_progress"}, {"content": "Fix", "status": "pending"}, {"content": "Test", "status": "pending"}]})));
        assert_eq!(t.iter().map(|x| x.state).collect::<Vec<_>>(), [InProgress, Pending, Pending]);
        assert!(update_todos(&mut t, plan, &json!({"advance": true})));
        assert_eq!(t.iter().map(|x| x.state).collect::<Vec<_>>(), [Completed, InProgress, Pending]);
        assert!(update_todos(&mut t, plan, &json!({"done": [2], "start": 3})));
        assert_eq!(t.iter().map(|x| x.state).collect::<Vec<_>>(), [Completed, Completed, InProgress]);
        assert!(!update_todos(&mut t, plan, &json!({"done": [0, 9]})), "out of range does nothing");
        assert!(update_todos(&mut t, NEXT_STEP_TOOL, &json!({"recap": "done"})));
        assert!(t.iter().all(|x| x.state == Completed));
        assert!(update_todos(&mut t, "TodoWrite", &json!({"todos": [{"content": "One", "status": "completed", "activeForm": "Doing one"}]})));
        assert_eq!(t, [Todo { text: "One".into(), state: Completed }]);
        assert!(!update_todos(&mut t, "Bash", &json!({"command": "ls"})));
    }

    #[test]
    fn next_step_buttons_and_wake() {
        let actions = serde_json::json!([{"label": "Push it"}, {"action": "commit", "label": "ignored"}, {"label": "  "}, {"action": "create_pr"}]);
        assert_eq!(next_step_options(&actions), ["Push it", "Commit", "Create a PR"]);
        assert!(next_step_options(&serde_json::Value::Null).is_empty());
        let w = next_step_wake("Push to GitHub?", "Push it");
        assert!(w.contains("\"Push to GitHub?\"") && w.contains("\"Push it\""));
    }

    #[test]
    fn merging_hooks_keeps_everything_else_and_is_idempotent() {
        let mut v: serde_json::Value = serde_json::from_str(r#"{"model":"opus","hooks":{"Stop":[{"hooks":[{"type":"command","command":"say done"}]}]}}"#).unwrap();
        let added = merge_hooks(&mut v, "/h/.local/bin/quaked").unwrap();
        assert_eq!(added.len(), 10);
        assert_eq!(v["hooks"]["PermissionRequest"][0]["hooks"][0]["command"], "/h/.local/bin/quaked permit");
        let next = &v["hooks"]["PostToolUse"][1];
        assert_eq!(next["matcher"], NEXT_STEP_TOOL);
        assert_eq!(next["hooks"][0]["command"], "/h/.local/bin/quaked nextstep");
        assert_eq!(next["hooks"][0]["asyncRewake"], true);
        assert!(v["hooks"]["PostToolUse"][0]["hooks"][0].get("asyncRewake").is_none());
        assert_eq!(v["hooks"]["PreToolUse"].as_array().unwrap().len(), 1, "no blocking question hook");
        assert_eq!(v["model"], "opus");
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2, "kept the existing Stop hook");
        assert_eq!(v["hooks"]["PreToolUse"][0]["matcher"], "*");
        assert!(merge_hooks(&mut v, "/h/.local/bin/quaked").unwrap().is_empty());
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["model", "hooks"], "key order preserved");
    }

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
