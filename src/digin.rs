//! DIGIN pane data: your open issues, unread inbox (mentions, DMs, assignments), chat
//! unread counts and recently changed pages, from one or more Digin servers.
//!
//! Sign-in is Digin's email code (POST /auth/email/start, then /auth/email/token), which
//! gives a `digin_` API token (90 days, no refresh) holding only the permits in [`permits`];
//! a token made in Digin's Settings → Access works too. Tokens live in this Mac's Keychain
//! (see secret.rs), never in config.json. The REST API is /api/v1 with `Authorization: Bearer`.
//!
//! The board can also open one item (an issue with its comments, an inbox line in full, a
//! page's text) and act on it: move an issue to another status, mark an inbox line read.
//! Those requests run here, on this thread, and come back to the app as a [`Reply`] carried
//! by the next [`Status`]; the app keeps the open item in `Status::detail`.

use crate::config::{Digin as DiginCfg, DiginAccount};
use crate::device::log;
use crate::secret;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Account {
    pub url: String,
    pub label: String,
    pub email: String,
    pub signed_in: bool,
    pub handle: String,
    pub name: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Issue {
    pub account: usize,
    /// The issue's id on its server (what /api/v1/issues/{id} takes).
    pub id: i64,
    /// "KES-12"
    pub key: String,
    pub title: String,
    /// backlog, todo, in_progress, waiting, done, canceled
    pub status: String,
    pub status_name: String,
    /// 0 none, 1 urgent, 2 high, 3 medium, 4 low
    pub priority: u8,
    pub url: String,
    pub updated: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Note {
    pub account: usize,
    /// A notification's id; a conversation's line (from chat) has minus its newest message's.
    pub id: i64,
    /// assigned, mentioned, commented, status, due, changed, message, call_missed...
    pub kind: String,
    pub title: String,
    pub body: String,
    pub url: String,
    pub created: u64,
    /// A conversation's line: the conversation and its newest unread message (else 0).
    pub channel: i64,
    pub message: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DocPage {
    pub account: usize,
    pub id: i64,
    pub title: String,
    pub project: String,
    pub updated: u64,
    /// Someone else edited it since you last read it.
    pub changed: bool,
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub accounts: Vec<Account>,
    pub issues: Vec<Issue>,
    pub inbox: Vec<Note>,
    pub pages: Vec<DocPage>,
    /// Unread chat messages and @-mentions across channels, and how many channels.
    pub chat_unread: u32,
    pub chat_mentions: u32,
    pub chat_channels: u32,
    /// The item open on the board. The app's own: this thread never sets it, and the app
    /// keeps it across updates.
    pub detail: Option<Detail>,
    /// What this thread has to tell the app about the open item (taken out on arrival).
    pub reply: Option<Reply>,
}

impl Status {
    pub fn signed_in(&self) -> bool {
        self.accounts.iter().any(|a| a.signed_in)
    }
}

#[derive(Debug)]
pub enum Cmd {
    Configure(DiginCfg),
    Refresh,
    /// Fetch an item for the board; answered with [`Reply::Opened`].
    OpenIssue(Issue),
    OpenNote(Note),
    OpenPage(DocPage),
    /// Move an issue to one of its project's statuses; answered with [`Reply::Saved`].
    SetStatus { key: String, account: usize, issue: i64, status: StatusOpt },
    /// Mark an inbox line read; answered with [`Reply::Read`].
    MarkRead { key: String, account: usize, id: i64, channel: i64, message: i64 },
    /// Send the status again after this long, with [`Reply::Tick`] (the app's timer).
    Nudge(Duration),
}

// ---------------------------------------------------------------- the open item

/// Said instead of failing when the sign-in lacks a permit (Digin's `token_not_allowed`).
pub const NEEDS_PERMIT: &str = "This sign-in isn't allowed to do that yet. Sign in again on the Digin tab of Quaked Settings to allow it.";

pub fn needs_permit(e: &str) -> bool {
    e == NEEDS_PERMIT
}

/// How many of an issue's comments the board shows.
pub const COMMENTS_SHOWN: usize = 12;

/// One block of Markdown, flattened to plain text for the board.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// (level 1-6, text)
    Heading(u8, String),
    Para(String),
    /// A list item: (marker such as "•", "3." or "✓", nesting depth, text).
    Item(String, u8, String),
    /// One line of a code block, indentation kept.
    Code(String),
    Quote(String),
    /// A table row's cells, and whether it's the heading row.
    Row(Vec<String>, bool),
    Rule,
    /// Something the board can't draw (a diagram), said in words.
    Aside(String),
}

/// One of a project's statuses (`issueStatuses` in /api/v1/bootstrap). `id` 0 is a bare
/// category, for a server without statuses of its own.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatusOpt {
    pub id: i64,
    pub name: String,
    /// backlog, todo, in_progress, waiting, done, canceled
    pub category: String,
}

impl StatusOpt {
    /// Done and canceled close the issue: the board asks for a second press.
    pub fn closes(&self) -> bool {
        matches!(self.category.as_str(), "done" | "canceled")
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Comment {
    pub author: String,
    pub created: u64,
    pub text: Vec<Block>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct IssueView {
    pub id: i64,
    pub key: String,
    pub title: String,
    pub status: String,
    pub status_id: i64,
    pub status_name: String,
    pub priority: u8,
    /// The project's name ("" until fetched).
    pub project: String,
    pub assignee: String,
    pub updated: u64,
    pub url: String,
    /// The description, with the checklist where its `[[steps]]` marker is (or after it).
    pub description: Vec<Block>,
    /// Newest first (at most [`COMMENTS_SHOWN`]), and how many there are in all.
    pub comments: Vec<Comment>,
    pub comment_count: usize,
    /// The project's statuses in workflow order (empty until fetched).
    pub statuses: Vec<StatusOpt>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NoteView {
    pub id: i64,
    pub kind: String,
    /// Who and where ("Sam in #release", "DIG-612 Runner pool…").
    pub title: String,
    /// Who wrote it, when the line names a message or a comment.
    pub author: String,
    pub created: u64,
    pub url: String,
    /// The whole message where it could be found, else the line's own words.
    pub text: Vec<Block>,
    pub channel: i64,
    pub message: i64,
    /// Marked read from the board.
    pub read: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageView {
    pub id: i64,
    pub title: String,
    pub project: String,
    pub updated_by: String,
    pub updated: u64,
    pub changed: bool,
    pub url: String,
    pub text: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Issue(IssueView),
    Note(NoteView),
    Page(PageView),
}

/// What the knob can do on an open item, left to right on its action bar.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Status(StatusOpt),
    MarkRead,
    Open,
}

impl Body {
    pub fn url(&self) -> &str {
        match self {
            Body::Issue(x) => &x.url,
            Body::Note(n) => &n.url,
            Body::Page(p) => &p.url,
        }
    }

    pub fn actions(&self) -> Vec<Action> {
        match self {
            Body::Issue(x) => x.statuses.iter().cloned().map(Action::Status).chain(std::iter::once(Action::Open)).collect(),
            Body::Note(n) if !n.read => vec![Action::MarkRead, Action::Open],
            _ => vec![Action::Open],
        }
    }

    /// The action that is the issue's current status, if it has one on the bar.
    pub fn current(&self) -> Option<usize> {
        match self {
            Body::Issue(x) => x.statuses.iter().position(|s| if s.id != 0 { s.id == x.status_id } else { s.category == x.status }),
            _ => None,
        }
    }

    /// Where turning past the end of the text lands: the issue's current status (so the
    /// next turn moves it along), else the first action.
    pub fn entry_action(&self) -> usize {
        self.current().unwrap_or(0)
    }

    /// Where a press on the text lands: the issue's next status (so press, press moves it
    /// along; never canceled), "Mark read" on an unread line, else "Open in Digin".
    pub fn press_action(&self) -> usize {
        match self {
            Body::Issue(x) => match self.current() {
                Some(cur) => x.statuses.iter().enumerate().skip(cur + 1).find(|(_, s)| s.category != "canceled").map(|(i, _)| i).unwrap_or(cur),
                None => x.statuses.len(), // statuses not known yet: "Open in Digin"
            },
            _ => 0,
        }
    }
}

/// The item open on the board, and where the knob is on it. Owned by the app.
#[derive(Debug, Clone, PartialEq)]
pub struct Detail {
    /// The same string as `Level::Detail`: "issue:0:4512", "note:0:-881", "page:1:77".
    pub key: String,
    pub account: usize,
    /// What the list knew, until the fetch answers; then the whole thing.
    pub body: Body,
    pub loading: bool,
    /// Why it couldn't be fetched.
    pub error: Option<String>,
    /// None: the knob scrolls the text. Some(i): it's on action i.
    pub focus: Option<usize>,
    /// A closing status waiting for its second press: (action, when it was armed).
    pub armed: Option<(usize, Instant)>,
    /// A change on its way ("Saving…").
    pub busy: Option<String>,
    /// How the last change went: (words, worked).
    pub said: Option<(String, bool)>,
}

impl Detail {
    fn new(key: String, account: usize, body: Body) -> Detail {
        Detail { key, account, body, loading: true, error: None, focus: None, armed: None, busy: None, said: None }
    }

    /// An issue from the list, and the command that fetches the rest.
    pub fn issue(x: &Issue) -> (Detail, Cmd) {
        let body = IssueView { id: x.id, key: x.key.clone(), title: x.title.clone(), status: x.status.clone(), status_name: x.status_name.clone(), priority: x.priority, updated: x.updated, url: x.url.clone(), ..Default::default() };
        (Detail::new(format!("issue:{}:{}", x.account, x.id), x.account, Body::Issue(body)), Cmd::OpenIssue(x.clone()))
    }

    pub fn note(n: &Note) -> (Detail, Cmd) {
        let body = NoteView { id: n.id, kind: n.kind.clone(), title: n.title.clone(), created: n.created, url: n.url.clone(), text: markdown(&n.body), channel: n.channel, message: n.message, ..Default::default() };
        (Detail::new(format!("note:{}:{}", n.account, n.id), n.account, Body::Note(body)), Cmd::OpenNote(n.clone()))
    }

    pub fn page(p: &DocPage) -> (Detail, Cmd) {
        let body = PageView { id: p.id, title: p.title.clone(), project: p.project.clone(), updated: p.updated, changed: p.changed, url: p.url.clone(), ..Default::default() };
        (Detail::new(format!("page:{}:{}", p.account, p.id), p.account, Body::Page(body)), Cmd::OpenPage(p.clone()))
    }
}

/// From this thread to the app, about the open item (by its key).
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Opened(String, Result<Box<Body>, String>),
    /// The status the issue is in now.
    Saved(String, Result<StatusOpt, String>),
    Read(String, Result<(), String>),
    /// A [`Cmd::Nudge`] ran out.
    Tick,
}

/// A status category as Digin says it, for a server without statuses of its own.
fn category_name(c: &str) -> &'static str {
    match c {
        "backlog" => "Backlog",
        "todo" => "Todo",
        "in_progress" => "In progress",
        "waiting" => "Waiting",
        "done" => "Done",
        "canceled" => "Canceled",
        _ => "",
    }
}

// ---------------------------------------------------------------- Markdown, flattened

/// Markdown as blocks of plain text: headings, paragraphs, list items (with their bullets),
/// code lines, quotes and rules. Links keep their words, emphasis and inline code marks go.
pub fn markdown(text: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut para: Vec<String> = Vec::new();
    // Inside a fenced code block: its fence and whether it's a diagram.
    let mut fence: Option<(String, bool)> = None;
    // What the previous line was, for lines that continue it.
    let (mut in_item, mut in_quote) = (false, false);
    fn flush(para: &mut Vec<String>, out: &mut Vec<Block>) {
        if !para.is_empty() {
            let t = inline(&para.join(" "));
            if !t.is_empty() {
                out.push(Block::Para(t));
            }
            para.clear();
        }
    }
    for raw in text.lines() {
        let line = raw.trim_end().replace('\t', "    ");
        let t = line.trim_start();
        if let Some((mark, diagram)) = &fence {
            if t.starts_with(mark.as_str()) && t.trim_start_matches(mark.chars().next().unwrap_or('`')).trim().is_empty() {
                fence = None;
            } else if !diagram {
                out.push(Block::Code(line.clone()));
            }
            continue;
        }
        if t.starts_with("```") || t.starts_with("~~~") {
            flush(&mut para, &mut out);
            let ch = t.chars().next().unwrap_or('`');
            let n = t.chars().take_while(|c| *c == ch).count();
            let info = t[n..].trim().to_lowercase();
            let diagram = info.starts_with("mermaid");
            if diagram {
                out.push(Block::Aside("A diagram: open it in Digin to see it.".into()));
            }
            fence = Some((ch.to_string().repeat(n), diagram));
            (in_item, in_quote) = (false, false);
            continue;
        }
        if t.is_empty() {
            flush(&mut para, &mut out);
            (in_item, in_quote) = (false, false);
            continue;
        }
        if t.starts_with("<!--") || t == "[[steps]]" {
            continue;
        }
        // Setext headings: a line of = or - under a paragraph.
        if !para.is_empty() && t.len() >= 2 && (t.chars().all(|c| c == '=') || t.chars().all(|c| c == '-')) {
            let level = if t.starts_with('=') { 1 } else { 2 };
            let words = inline(&para.join(" "));
            para.clear();
            out.push(Block::Heading(level, words));
            continue;
        }
        let hashes = t.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && t[hashes..].chars().next().is_none_or(|c| c == ' ') {
            flush(&mut para, &mut out);
            let words = t[hashes..].trim().trim_end_matches('#').trim();
            out.push(Block::Heading(hashes as u8, inline(words)));
            (in_item, in_quote) = (false, false);
            continue;
        }
        if is_rule(t) {
            flush(&mut para, &mut out);
            out.push(Block::Rule);
            (in_item, in_quote) = (false, false);
            continue;
        }
        if let Some(q) = t.strip_prefix('>') {
            flush(&mut para, &mut out);
            let q = inline(q.trim_start_matches('>').trim());
            match out.last_mut() {
                Some(Block::Quote(prev)) if in_quote && !q.is_empty() => {
                    prev.push(' ');
                    prev.push_str(&q);
                }
                _ if q.is_empty() => {}
                _ => out.push(Block::Quote(q)),
            }
            (in_item, in_quote) = (false, true);
            continue;
        }
        if let Some((marker, rest)) = list_item(t) {
            flush(&mut para, &mut out);
            let indent = line.len() - t.len();
            out.push(Block::Item(marker, (indent / 2).min(4) as u8, inline(rest)));
            (in_item, in_quote) = (true, false);
            continue;
        }
        if t.starts_with('|') {
            flush(&mut para, &mut out);
            let cells: Vec<String> = t.trim_matches('|').split('|').map(|c| inline(c.trim())).collect();
            if t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
                // |---|---| under the first row makes it the heading.
                if let Some(Block::Row(_, head)) = out.last_mut() {
                    *head = true;
                }
            } else if cells.iter().any(|c| !c.is_empty()) {
                out.push(Block::Row(cells, false));
            }
            (in_item, in_quote) = (false, false);
            continue;
        }
        // A line that continues the list item or quote above it.
        if in_item || in_quote {
            let more = inline(t);
            match out.last_mut() {
                Some(Block::Item(_, _, prev)) | Some(Block::Quote(prev)) if !more.is_empty() => {
                    prev.push(' ');
                    prev.push_str(&more);
                }
                _ => {}
            }
            continue;
        }
        para.push(t.to_string());
    }
    flush(&mut para, &mut out);
    out
}

fn is_rule(t: &str) -> bool {
    let s: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    s.len() >= 3 && (s.chars().all(|c| c == '-') || s.chars().all(|c| c == '*') || s.chars().all(|c| c == '_'))
}

/// "- x", "* x", "+ x", "1. x", "2) x", and task items "- [ ] x" / "- [x] x".
fn list_item(t: &str) -> Option<(String, &str)> {
    let mut chars = t.char_indices();
    let (_, first) = chars.next()?;
    if matches!(first, '-' | '*' | '+') {
        let rest = t[1..].strip_prefix(' ')?.trim_start();
        if let Some(r) = rest.strip_prefix("[ ] ").or_else(|| (rest == "[ ]").then_some("")) {
            return Some(("○".into(), r));
        }
        if let Some(r) = rest.strip_prefix("[x] ").or_else(|| rest.strip_prefix("[X] ")) {
            return Some(("✓".into(), r));
        }
        return Some(("•".into(), rest));
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if (1..=3).contains(&digits) {
        let after = &t[digits..];
        let punct = after.chars().next()?;
        if matches!(punct, '.' | ')') {
            let rest = after[1..].strip_prefix(' ')?;
            return Some((format!("{}.", &t[..digits]), rest.trim_start()));
        }
    }
    None
}

/// The text of one line of Markdown: links as their words, images as their description,
/// `[[wiki links]]` as their names, emphasis and code marks dropped, HTML tags dropped.
pub fn inline(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let n = c.len();
    let mut out = String::with_capacity(s.len());
    let run = |i: usize, ch: char| c[i..].iter().take_while(|x| **x == ch).count();
    let mut i = 0;
    while i < n {
        let ch = c[i];
        match ch {
            '\\' if i + 1 < n && c[i + 1].is_ascii_punctuation() => {
                out.push(c[i + 1]);
                i += 2;
            }
            '`' => {
                let k = run(i, '`');
                let close = (i + k..n).find(|&j| c[j] == '`' && run(j, '`') == k && (j == 0 || c[j - 1] != '`'));
                match close {
                    Some(j) => {
                        out.extend(c[i + k..j].iter());
                        i = j + k;
                    }
                    None => {
                        out.extend(c[i..i + k].iter());
                        i += k;
                    }
                }
            }
            '!' if c.get(i + 1) == Some(&'[') => match bracket_link(&c, i + 1) {
                Some((label, after)) => {
                    let label = inline(&label);
                    out.push_str(if label.is_empty() { "(image)" } else { &label });
                    i = after;
                }
                None => {
                    out.push('!');
                    i += 1;
                }
            },
            '[' if c.get(i + 1) == Some(&'[') => {
                let close = (i + 2..n.saturating_sub(1)).find(|&j| c[j] == ']' && c[j + 1] == ']');
                match close {
                    Some(j) => {
                        let inner: String = c[i + 2..j].iter().collect();
                        out.push_str(inner.rsplit('|').next().unwrap_or(&inner).trim());
                        i = j + 2;
                    }
                    None => {
                        out.push('[');
                        i += 1;
                    }
                }
            }
            '[' => match bracket_link(&c, i) {
                Some((label, after)) => {
                    out.push_str(&inline(&label));
                    i = after;
                }
                None => {
                    out.push('[');
                    i += 1;
                }
            },
            '<' => {
                let close = (i + 1..n).find(|&j| c[j] == '>');
                let inner: String = close.map(|j| c[i + 1..j].iter().collect()).unwrap_or_default();
                let tag = inner.chars().next().is_some_and(|f| f.is_ascii_alphabetic() || f == '/' || f == '!') && !inner.contains(' ') || inner.starts_with("a ") || inner.starts_with("img ") || inner.starts_with("span ");
                match close {
                    Some(j) if inner.starts_with("http://") || inner.starts_with("https://") || inner.starts_with("mailto:") => {
                        out.push_str(inner.trim_start_matches("mailto:"));
                        i = j + 1;
                    }
                    Some(j) if tag => {
                        if inner.trim_end_matches('/').trim() == "br" {
                            out.push(' ');
                        }
                        i = j + 1;
                    }
                    _ => {
                        out.push('<');
                        i += 1;
                    }
                }
            }
            '~' => {
                let k = run(i, '~');
                if k < 2 {
                    out.push('~');
                }
                i += k;
            }
            '*' | '_' => {
                let k = run(i, ch);
                let prev = i.checked_sub(1).map(|p| c[p]);
                let next = c.get(i + k).copied();
                let opens = next.is_some_and(|x| !x.is_whitespace());
                let closes = prev.is_some_and(|x| !x.is_whitespace());
                let alnum = |x: Option<char>| x.is_some_and(|x| x.is_alphanumeric());
                let emphasis = if ch == '_' {
                    (opens && !alnum(prev)) || (closes && !alnum(next))
                } else {
                    (opens || closes) && !(alnum(prev) && alnum(next))
                };
                if !emphasis {
                    out.extend(c[i..i + k].iter());
                }
                i += k;
            }
            _ => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `[label](target)`, `[label][ref]` or `[label]` followed by nothing link-like (None) at
/// `c[i] == '['`: the label and the index just past the link.
fn bracket_link(c: &[char], i: usize) -> Option<(String, usize)> {
    let mut depth = 0;
    let mut j = i;
    while j < c.len() {
        match c[j] {
            '\\' => j += 1,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        j += 1;
    }
    if j >= c.len() {
        return None;
    }
    let label: String = c[i + 1..j].iter().collect();
    let (open, close) = match c.get(j + 1) {
        Some('(') => ('(', ')'),
        Some('[') => ('[', ']'),
        _ => return None,
    };
    let mut depth = 0;
    let mut k = j + 1;
    while k < c.len() {
        if c[k] == open {
            depth += 1;
        } else if c[k] == close {
            depth -= 1;
            if depth == 0 {
                return Some((label, k + 1));
            }
        }
        k += 1;
    }
    None
}

pub fn normalize(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    if u.starts_with("http://") || u.starts_with("https://") {
        u.to_string()
    } else {
        format!("https://{u}")
    }
}

fn keychain_account(url: &str) -> String {
    format!("digin:{}", normalize(url))
}

pub fn token_for(url: &str) -> Option<String> {
    secret::get(&keychain_account(url))
}

pub fn host_of(url: &str) -> String {
    normalize(url).trim_start_matches("https://").trim_start_matches("http://").to_string()
}

/// RFC 3339 -> unix ms.
fn time_ms(s: &str) -> u64 {
    crate::github::iso_ms(s)
}

/// One HTTP request through curl. The bearer token goes in on stdin (`-K -`), so it never
/// shows up in the process list. Returns (status, body).
fn curl(method: &str, url: &str, token: Option<&str>, body: Option<&Value>) -> Result<(u16, Vec<u8>), String> {
    let mut args: Vec<String> = vec!["-s".into(), "--max-time".into(), "12".into(), "-X".into(), method.into(), "-K".into(), "-".into(), "-w".into(), "\n%{http_code}".into()];
    let mut config = String::from("header = \"Accept: application/json\"\nheader = \"User-Agent: quaked\"\n");
    if let Some(t) = token {
        config.push_str(&format!("header = \"Authorization: Bearer {}\"\n", t.replace('"', "")));
    }
    if let Some(b) = body {
        config.push_str("header = \"Content-Type: application/json\"\n");
        config.push_str(&format!("data-binary = \"{}\"\n", b.to_string().replace('\\', "\\\\").replace('"', "\\\"")));
    }
    args.push(url.into());
    let mut child = Command::new("/usr/bin/curl").args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|e| e.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(config.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let mut text = out.stdout;
    let nl = text.iter().rposition(|b| *b == b'\n').ok_or("no answer")?;
    let code: u16 = String::from_utf8_lossy(&text[nl + 1..]).trim().parse().map_err(|_| "no answer".to_string())?;
    text.truncate(nl);
    if code == 0 {
        return Err("can't reach the server".into());
    }
    Ok((code, text))
}

fn error_of(code: u16, body: &[u8]) -> String {
    let v: Value = serde_json::from_slice(body).unwrap_or_default();
    let msg = v.pointer("/error/message").and_then(Value::as_str).or_else(|| v.pointer("/error/code").and_then(Value::as_str)).unwrap_or("");
    match code {
        401 => "signed out (sign in again)".into(),
        // The token doesn't hold the permit (signed in before quaked asked for it).
        403 if v.pointer("/error/code").and_then(Value::as_str) == Some("token_not_allowed") => NEEDS_PERMIT.into(),
        403 => format!("not allowed{}", if msg.is_empty() { String::new() } else { format!(": {msg}") }),
        _ if msg.is_empty() => format!("HTTP {code}"),
        _ => msg.to_string(),
    }
}

fn get(base: &str, token: &str, path: &str) -> Result<Value, String> {
    let (code, body) = curl("GET", &format!("{base}{path}"), Some(token), None)?;
    if !(200..300).contains(&code) {
        return Err(error_of(code, &body));
    }
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}

/// A change: `body` sent as JSON (never with an Origin header, which curl doesn't add).
/// Returns the answer's JSON, or null for 204.
fn send(base: &str, token: &str, req: &Request) -> Result<Value, String> {
    let (code, body) = curl(req.method, &format!("{base}{}", req.path), Some(token), Some(&req.body))?;
    if !(200..300).contains(&code) {
        return Err(error_of(code, &body));
    }
    if body.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}

/// One write to Digin, built apart from sending it (so it can be tested).
#[derive(Debug, PartialEq)]
struct Request {
    method: &'static str,
    path: String,
    body: Value,
}

/// Moves an issue to a status: `PATCH /api/v1/issues/{id}` (permit issues: write) with the
/// project's own status id, or the bare category on a server without statuses.
fn set_status_request(issue: i64, status: &StatusOpt) -> Request {
    let body = if status.id > 0 { serde_json::json!({"statusId": status.id}) } else { serde_json::json!({"status": status.category}) };
    Request { method: "PATCH", path: format!("/api/v1/issues/{issue}"), body }
}

/// Marks an inbox line read. A notification: `POST /api/v1/notifications/read {id}` (profile:
/// write). A conversation's line (negative id, from chat): the conversation read up to its
/// newest message, `POST /api/v1/channels/{id}/read {messageId}` (chat: write).
fn read_request(id: i64, channel: i64, message: i64) -> Option<Request> {
    if id > 0 {
        return Some(Request { method: "POST", path: "/api/v1/notifications/read".into(), body: serde_json::json!({"id": id}) });
    }
    let message = if message > 0 { message } else { -id };
    (channel > 0 && message > 0).then(|| Request { method: "POST", path: format!("/api/v1/channels/{channel}/read"), body: serde_json::json!({"messageId": message}) })
}

// ---------------------------------------------------------------- sign-in

/// What a server offers: `{providers:[{id,kind,name}], gate?, ...}`.
pub fn providers(url: &str) -> Result<Value, String> {
    let (code, body) = curl("GET", &format!("{}/auth/providers", normalize(url)), None, None)?;
    if code != 200 {
        return Err(format!("{} doesn't look like a Digin server ({})", host_of(url), error_of(code, &body)));
    }
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}

/// What the sign-in asks Digin for. Write only where the board acts: issues (moving an issue
/// to another status), profile (marking a notification read; it also holds /bootstrap and the
/// inbox) and chat (marking a conversation read; quaked never sends messages). Pages stay read.
fn permits() -> Value {
    serde_json::json!({"issues": "write", "pages": "read", "chat": "write", "profile": "write"})
}

/// The `quaked ctl digin-auth <json>` commands, run off the main thread by the control
/// socket (on the Mac with the Quake, so the token is made and kept there). Returns the
/// reply and, after a sign-in, the account to add to the settings.
pub fn auth(json: &str) -> (Value, Option<DiginAccount>) {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return (serde_json::json!({"error": "bad request"}), None) };
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let url = normalize(&s("url"));
    let fail = |e: String| (serde_json::json!({"error": e}), None);
    match s("step").as_str() {
        "providers" => match providers(&url) {
            Ok(p) => (p, None),
            Err(e) => fail(e),
        },
        "email-start" => match curl("POST", &format!("{url}/auth/email/start"), None, Some(&serde_json::json!({"email": s("email")}))) {
            Ok((204 | 200, _)) => (serde_json::json!({"ok": true}), None),
            Ok((code, body)) => fail(error_of(code, &body)),
            Err(e) => fail(e),
        },
        "email-token" | "token" => {
            let token = if s("step") == "token" {
                s("token")
            } else {
                let mut body = serde_json::json!({"email": s("email"), "code": s("code"), "name": format!("quaked on {}", crate::sync::computer_name()), "permits": permits()});
                if let Some(n) = v.get("notice").filter(|n| !n.is_null()) {
                    body["notice"] = n.clone();
                }
                match curl("POST", &format!("{url}/auth/email/token"), None, Some(&body)) {
                    Ok((code, body)) if (200..300).contains(&code) => {
                        let r: Value = serde_json::from_slice(&body).unwrap_or_default();
                        r.get("token").and_then(Value::as_str).unwrap_or("").to_string()
                    }
                    Ok((409, body)) => {
                        // The organisation has a notice to accept first.
                        let r: Value = serde_json::from_slice(&body).unwrap_or_default();
                        return (serde_json::json!({"notice": r.get("notice")}), None);
                    }
                    Ok((400, _)) => return fail("That code didn't work (wrong, used or expired). Send a new one.".into()),
                    Ok((code, body)) => return fail(error_of(code, &body)),
                    Err(e) => return fail(e),
                }
            };
            if !token.starts_with("digin_") {
                return fail("That isn't a Digin token (they start with digin_).".into());
            }
            // Make sure it works before keeping it.
            let me = match get(&url, &token, "/api/v1/bootstrap") {
                Ok(b) => b.get("me").cloned().unwrap_or_default(),
                Err(e) => return fail(e),
            };
            if let Err(e) = secret::set(&keychain_account(&url), &token) {
                return fail(format!("couldn't save it in the Keychain: {e}"));
            }
            let name = me.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let email = me.get("email").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| s("email"));
            log(format!("digin: signed in to {} as {email}", host_of(&url)));
            (serde_json::json!({"ok": true, "name": name}), Some(DiginAccount { url: url.clone(), label: host_of(&url), email }))
        }
        "sign-out" => {
            secret::delete(&keychain_account(&url));
            log(format!("digin: signed out of {}", host_of(&url)));
            (serde_json::json!({"ok": true}), None)
        }
        _ => fail("unknown step".into()),
    }
}


// ---------------------------------------------------------------- polling

/// Names and links that rarely change, per account (from /api/v1/bootstrap).
#[derive(Default, Clone)]
struct Directory {
    /// project id -> (KEY, workspace uuid)
    projects: HashMap<i64, (String, String)>,
    project_names: HashMap<i64, String>,
    /// project id -> its statuses in workflow order
    statuses: HashMap<i64, Vec<StatusOpt>>,
    /// user id -> name
    users: HashMap<i64, String>,
    me: i64,
    handle: String,
    name: String,
    at: Option<Instant>,
}

impl Directory {
    fn user(&self, id: Option<i64>) -> String {
        id.and_then(|i| self.users.get(&i).cloned()).unwrap_or_default()
    }
}

fn directory(base: &str, token: &str) -> Result<Directory, String> {
    get(base, token, "/api/v1/bootstrap").map(|b| directory_from(&b))
}

fn directory_from(b: &Value) -> Directory {
    let int = |v: &Value, k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let list = |k: &str| b.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
    let workspaces: HashMap<i64, String> = list("workspaces").iter().filter_map(|w| Some((w.get("id")?.as_i64()?, w.get("uuid")?.as_str()?.to_string()))).collect();
    let mut d = Directory::default();
    for p in list("projects") {
        let (id, key) = (int(&p, "id"), text(&p, "key"));
        if id == 0 || key.is_empty() {
            continue;
        }
        let ws = workspaces.get(&int(&p, "workspaceId")).cloned().unwrap_or_default();
        d.projects.insert(id, (key, ws));
        d.project_names.insert(id, text(&p, "name"));
    }
    let mut statuses: Vec<(i64, i64, i64, StatusOpt)> = list("issueStatuses")
        .iter()
        .map(|s| (int(s, "projectId"), int(s, "position"), int(s, "id"), StatusOpt { id: int(s, "id"), name: text(s, "name"), category: text(s, "category") }))
        .filter(|(p, _, id, _)| *p > 0 && *id > 0)
        .collect();
    statuses.sort_by_key(|(p, pos, id, _)| (*p, *pos, *id));
    for (p, _, _, s) in statuses {
        d.statuses.entry(p).or_default().push(s);
    }
    for u in list("users").iter().chain(b.get("me")) {
        let id = int(u, "id");
        if id > 0 {
            d.users.insert(id, text(u, "name"));
        }
    }
    d.me = b.pointer("/me/id").and_then(Value::as_i64).unwrap_or(0);
    d.handle = b.pointer("/me/handle").and_then(Value::as_str).unwrap_or("").to_string();
    d.name = b.pointer("/me/name").and_then(Value::as_str).unwrap_or("").to_string();
    d.at = Some(Instant::now());
    d
}

fn link(base: &str, dir: &Directory, project: i64, rest: &str) -> String {
    match dir.projects.get(&project) {
        Some((key, ws)) if !ws.is_empty() => format!("{base}/w/{ws}/{key}/{rest}"),
        Some((key, _)) => format!("{base}/{key}/{rest}"),
        None => base.to_string(),
    }
}

fn text_of(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn int_of(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}

/// An issue as /api/v1/my and /api/v1/issues/{id} carry it.
fn issue_of(i: usize, base: &str, dir: &Directory, x: &Value) -> Issue {
    let number = int_of(x, "number");
    Issue {
        account: i,
        id: int_of(x, "id"),
        key: format!("{}-{number}", text_of(x, "key")),
        title: text_of(x, "title"),
        status: text_of(x, "status"),
        status_name: text_of(x, "statusName"),
        priority: x.get("priority").and_then(Value::as_u64).unwrap_or(0) as u8,
        url: link(base, dir, int_of(x, "projectId"), &format!("issues/{number}")),
        updated: time_ms(&text_of(x, "updatedAt")),
    }
}

/// An inbox line from /api/v1/notifications.
fn note_of(i: usize, base: &str, n: &Value) -> Note {
    let url = text_of(n, "url");
    Note {
        account: i,
        id: int_of(n, "id"),
        kind: text_of(n, "kind"),
        title: text_of(n, "title"),
        body: text_of(n, "body"),
        url: if url.starts_with('/') { format!("{base}{url}") } else if url.is_empty() { format!("{base}/inbox") } else { url },
        created: time_ms(&text_of(n, "createdAt")),
        channel: int_of(n, "channelId"),
        message: int_of(n, "messageId"),
    }
}

fn page_of(i: usize, base: &str, dir: &Directory, p: &Value) -> DocPage {
    let project = int_of(p, "projectId");
    let id = int_of(p, "id");
    DocPage {
        account: i,
        id,
        title: text_of(p, "title"),
        project: dir.projects.get(&project).map(|(k, _)| k.clone()).unwrap_or_default(),
        updated: time_ms(&text_of(p, "updatedAt")),
        changed: p.get("changed").and_then(Value::as_bool).unwrap_or(false),
        url: link(base, dir, project, &format!("pages/{id}")),
    }
}

struct Fetched {
    issues: Vec<Issue>,
    inbox: Vec<Note>,
    pages: Option<Vec<DocPage>>,
    chat: (u32, u32, u32),
}

fn fetch(i: usize, base: &str, token: &str, dir: &Directory, with_pages: bool) -> Result<Fetched, String> {
    let my = get(base, token, "/api/v1/my")?;
    let issues = my.get("open").and_then(Value::as_array).into_iter().flatten().map(|x| issue_of(i, base, dir, x)).collect();
    let notes = get(base, token, "/api/v1/notifications?unread=1&limit=30")?;
    let inbox = notes.get("notifications").and_then(Value::as_array).into_iter().flatten().map(|n| note_of(i, base, n)).collect();
    let mut chat = (0, 0, 0);
    if let Ok(channels) = get(base, token, "/api/v1/channels") {
        for c in channels.as_array().into_iter().flatten() {
            let unread = c.get("unread").and_then(Value::as_u64).unwrap_or(0) as u32;
            let mentions = c.get("mentions").and_then(Value::as_u64).unwrap_or(0) as u32;
            if unread > 0 || mentions > 0 {
                chat.0 += unread;
                chat.1 += mentions;
                chat.2 += 1;
            }
        }
    }
    let pages = if with_pages {
        let all = get(base, token, "/api/v1/pages")?;
        let mut list: Vec<DocPage> = all.as_array().into_iter().flatten().map(|p| page_of(i, base, dir, p)).collect();
        list.sort_by(|a, b| b.updated.cmp(&a.updated));
        list.truncate(30);
        Some(list)
    } else {
        None
    };
    Ok(Fetched { issues, inbox, pages, chat })
}

// ---------------------------------------------------------------- one item, for the board

/// An issue's description with its checklist put where its `[[steps]]` marker is (or after
/// it), as Markdown.
fn with_checklist(description: &str, checklist: &[Value]) -> String {
    let mut steps: Vec<(i64, String)> = checklist.iter().map(|s| (int_of(s, "position"), format!("- [{}] {}", if s.get("done").and_then(Value::as_bool).unwrap_or(false) { "x" } else { " " }, text_of(s, "text")))).collect();
    steps.sort_by_key(|(p, _)| *p);
    let steps = steps.into_iter().map(|(_, s)| s).collect::<Vec<_>>().join("\n");
    if steps.is_empty() {
        return description.to_string();
    }
    if description.lines().any(|l| l.trim() == "[[steps]]") {
        return description.lines().map(|l| if l.trim() == "[[steps]]" { format!("\n{steps}\n") } else { l.to_string() }).collect::<Vec<_>>().join("\n");
    }
    format!("{description}\n\n{steps}")
}

/// `GET /api/v1/issues/{id}` (issues: read): `{issue, comments, checklist, ...}`.
fn issue_view(i: usize, base: &str, dir: &Directory, v: &Value) -> IssueView {
    let x = v.get("issue").cloned().unwrap_or_default();
    let listed = issue_of(i, base, dir, &x);
    let project = int_of(&x, "projectId");
    let assignee = x.get("assigneeId").and_then(Value::as_i64);
    let mut statuses = dir.statuses.get(&project).cloned().unwrap_or_default();
    if statuses.is_empty() {
        statuses = ["backlog", "todo", "in_progress", "waiting", "done", "canceled"].iter().map(|c| StatusOpt { id: 0, name: category_name(c).into(), category: c.to_string() }).collect();
    }
    let all: Vec<&Value> = v.get("comments").and_then(Value::as_array).into_iter().flatten().collect();
    let comments = all
        .iter()
        .rev()
        .take(COMMENTS_SHOWN)
        .map(|c| Comment { author: dir.user(c.get("authorId").and_then(Value::as_i64)), created: time_ms(&text_of(c, "createdAt")), text: markdown(&text_of(c, "body")) })
        .collect();
    let checklist = v.get("checklist").and_then(Value::as_array).cloned().unwrap_or_default();
    IssueView {
        id: listed.id,
        key: listed.key,
        title: listed.title,
        status: listed.status,
        status_id: int_of(&x, "statusId"),
        status_name: listed.status_name,
        priority: listed.priority,
        project: dir.project_names.get(&project).cloned().unwrap_or_default(),
        assignee: match assignee {
            Some(a) if a == dir.me && dir.me != 0 => "you".into(),
            a => dir.user(a),
        },
        updated: listed.updated,
        url: listed.url,
        description: markdown(&with_checklist(&text_of(&x, "description"), &checklist)),
        comments,
        comment_count: all.len(),
        statuses,
    }
}

/// `GET /api/v1/pages/{id}` (pages: read): `{page: {title, body, updatedBy, ...}, ...}`.
fn page_view(base: &str, dir: &Directory, listed: &DocPage, v: &Value) -> PageView {
    let p = v.get("page").cloned().unwrap_or_default();
    let project = int_of(&p, "projectId");
    PageView {
        id: listed.id,
        title: Some(text_of(&p, "title")).filter(|t| !t.is_empty()).unwrap_or_else(|| listed.title.clone()),
        project: dir.project_names.get(&project).cloned().filter(|n| !n.is_empty()).unwrap_or_else(|| listed.project.clone()),
        updated_by: dir.user(p.get("updatedBy").and_then(Value::as_i64)),
        updated: Some(time_ms(&text_of(&p, "updatedAt"))).filter(|t| *t > 0).unwrap_or(listed.updated),
        changed: listed.changed,
        url: if project > 0 { link(base, dir, project, &format!("pages/{}", listed.id)) } else { listed.url.clone() },
        text: markdown(&text_of(&p, "body")),
    }
}

/// The chat message an inbox line is about: its own (a conversation's line), or the one a
/// mention's link names (`…/chat/{channel}?m={message}`).
fn message_of(n: &Note) -> Option<i64> {
    if n.message > 0 {
        return Some(n.message);
    }
    let q = n.url.split_once('?')?.1;
    q.split('&').find_map(|kv| kv.strip_prefix("m=")).and_then(|m| m.parse().ok()).filter(|m: &i64| *m > 0)
}

/// The issue an inbox line's link names: (workspace uuid or "", KEY, number), from
/// `…/w/{uuid}/{KEY}/issues/{n}` or `…/{KEY}/issues/{n}`.
fn issue_ref(url: &str) -> Option<(String, String, i64)> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let parts: Vec<&str> = path.split('/').collect();
    let at = parts.iter().rposition(|p| *p == "issues")?;
    let number = parts.get(at + 1)?.parse().ok()?;
    let key = parts.get(at.checked_sub(1)?)?.to_string();
    let ws = if at >= 3 && parts[at - 3] == "w" { parts[at - 2].to_string() } else { String::new() };
    (!key.is_empty()).then_some((ws, key, number))
}

/// The whole comment an inbox line clipped (Digin keeps 240 characters, flattened): the
/// newest comment whose words start the same way.
fn full_comment(issue: &Value, clipped: &str) -> Option<(String, Option<i64>)> {
    let head: String = clipped.trim_end_matches('…').split_whitespace().collect::<Vec<_>>().join(" ");
    if head.is_empty() {
        return None;
    }
    issue.get("comments").and_then(Value::as_array)?.iter().rev().find_map(|c| {
        let body = text_of(c, "body");
        let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
        flat.starts_with(&head).then(|| (body, c.get("authorId").and_then(Value::as_i64)))
    })
}

/// An inbox line in full. Never fails: whatever can't be fetched, the line's own words stand.
fn note_view(base: &str, token: &str, dir: &Directory, n: &Note) -> NoteView {
    let mut view = NoteView { id: n.id, kind: n.kind.clone(), title: n.title.clone(), created: n.created, url: n.url.clone(), text: markdown(&n.body), channel: n.channel, message: n.message, ..Default::default() };
    if let Some(m) = message_of(n) {
        // GET /api/v1/messages/{id} (chat: read)
        if let Ok(msg) = get(base, token, &format!("/api/v1/messages/{m}")) {
            let body = text_of(&msg, "body");
            if !body.trim().is_empty() {
                view.text = markdown(&body);
            }
            view.author = dir.user(msg.get("authorId").and_then(Value::as_i64));
        }
        return view;
    }
    if n.body.ends_with('…') && matches!(n.kind.as_str(), "commented" | "mentioned") {
        if let Some((ws, key, number)) = issue_ref(&n.url) {
            let q = if ws.is_empty() { String::new() } else { format!("?workspace={ws}") };
            // GET /api/v1/issues/by-key/{KEY}/{number} (issues: read)
            if let Ok(issue) = get(base, token, &format!("/api/v1/issues/by-key/{key}/{number}{q}")) {
                if let Some((body, author)) = full_comment(&issue, &n.body) {
                    view.text = markdown(&body);
                    view.author = dir.user(author);
                }
            }
        }
    }
    view
}

pub fn spawn(cfg: DiginCfg, fast: Arc<AtomicBool>, emit: impl Fn(Status) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("digin".into()).spawn(move || run(cfg, fast, rx, emit)).unwrap();
    tx
}

/// An account's address, token and directory, for one request about an item.
fn account_of(cfg: &DiginCfg, dirs: &mut HashMap<String, Directory>, account: usize) -> Result<(String, String, Directory), String> {
    let acct = cfg.accounts.get(account).ok_or("that Digin server isn't in the settings any more")?;
    let base = normalize(&acct.url);
    let token = token_for(&base).ok_or("not signed in on this Mac")?;
    if !dirs.contains_key(&base) {
        dirs.insert(base.clone(), directory(&base, &token)?);
    }
    let dir = dirs.get(&base).cloned().unwrap_or_default();
    Ok((base, token, dir))
}

fn run(mut cfg: DiginCfg, fast: Arc<AtomicBool>, rx: Receiver<Cmd>, emit: impl Fn(Status)) {
    let mut last = Status::default();
    let mut dirs: HashMap<String, Directory> = HashMap::new();
    let mut pages: HashMap<String, Vec<DocPage>> = HashMap::new();
    let mut next = Instant::now();
    let mut next_pages = Instant::now();
    let mut errors: HashMap<String, String> = HashMap::new();
    let mut nudge: Option<Instant> = None;
    let tell = |last: &Status, r: Reply| {
        let mut s = last.clone();
        s.reply = Some(r);
        emit(s);
    };
    loop {
        if Instant::now() >= next {
            let with_pages = Instant::now() >= next_pages;
            let mut st = Status::default();
            for (i, acct) in cfg.accounts.iter().enumerate() {
                let base = normalize(&acct.url);
                let mut a = Account { url: base.clone(), label: if acct.label.is_empty() { host_of(&base) } else { acct.label.clone() }, email: acct.email.clone(), ..Default::default() };
                let Some(token) = token_for(&base) else {
                    a.error = Some("not signed in on this Mac".into());
                    st.accounts.push(a);
                    continue;
                };
                a.signed_in = true;
                let stale = dirs.get(&base).and_then(|d| d.at).is_none_or(|t| t.elapsed() > Duration::from_secs(600));
                if stale {
                    match directory(&base, &token) {
                        Ok(d) => {
                            dirs.insert(base.clone(), d);
                        }
                        Err(e) => {
                            a.error = Some(e.clone());
                            if e.starts_with("signed out") {
                                a.signed_in = false;
                            }
                        }
                    }
                }
                let dir = dirs.get(&base).cloned().unwrap_or_default();
                a.handle = dir.handle.clone();
                a.name = dir.name.clone();
                if a.error.is_none() {
                    match fetch(i, &base, &token, &dir, with_pages) {
                        Ok(f) => {
                            st.issues.extend(f.issues);
                            st.inbox.extend(f.inbox);
                            if let Some(p) = f.pages {
                                pages.insert(base.clone(), p);
                            }
                            st.chat_unread += f.chat.0;
                            st.chat_mentions += f.chat.1;
                            st.chat_channels += f.chat.2;
                        }
                        Err(e) => a.error = Some(e),
                    }
                }
                if let Some(e) = &a.error {
                    if errors.get(&base) != Some(e) {
                        log(format!("digin {}: {e}", host_of(&base)));
                        errors.insert(base.clone(), e.clone());
                    }
                } else {
                    errors.remove(&base);
                }
                if let Some(p) = pages.get(&base) {
                    st.pages.extend(p.iter().cloned().map(|mut d| {
                        d.account = i;
                        d
                    }));
                }
                st.accounts.push(a);
            }
            // Most urgent first: in progress, then by priority (urgent = 1 ... none = 0 last).
            let rank = |s: &str| match s {
                "in_progress" => 0,
                "waiting" => 1,
                "todo" => 2,
                _ => 3,
            };
            st.issues.sort_by_key(|x| (rank(&x.status), if x.priority == 0 { 9 } else { x.priority }, std::cmp::Reverse(x.updated)));
            st.inbox.sort_by(|a, b| b.created.cmp(&a.created));
            st.pages.sort_by(|a, b| b.updated.cmp(&a.updated));
            if st != last {
                last = st.clone();
                emit(st);
            }
            if with_pages {
                next_pages = Instant::now() + Duration::from_secs(300);
            }
            let secs = cfg.refresh_seconds.max(15);
            next = Instant::now() + Duration::from_secs(if fast.load(Ordering::Relaxed) { (secs / 3).max(15) } else { secs });
        }
        let wait = nudge.map(|t| t.saturating_duration_since(Instant::now())).unwrap_or(Duration::MAX).min(Duration::from_secs(2));
        match rx.recv_timeout(wait) {
            Ok(Cmd::Configure(c)) => {
                cfg = c;
                next = Instant::now();
                next_pages = Instant::now();
            }
            Ok(Cmd::Refresh) => {
                next = Instant::now();
                next_pages = Instant::now();
                dirs.clear();
            }
            Ok(Cmd::OpenIssue(x)) => {
                let key = format!("issue:{}:{}", x.account, x.id);
                let r = account_of(&cfg, &mut dirs, x.account).and_then(|(base, token, dir)| get(&base, &token, &format!("/api/v1/issues/{}", x.id)).map(|v| Box::new(Body::Issue(issue_view(x.account, &base, &dir, &v)))));
                if let Err(e) = &r {
                    log(format!("digin: couldn't open {}: {e}", x.key));
                }
                tell(&last, Reply::Opened(key, r));
            }
            Ok(Cmd::OpenNote(n)) => {
                let key = format!("note:{}:{}", n.account, n.id);
                let r = account_of(&cfg, &mut dirs, n.account).map(|(base, token, dir)| Box::new(Body::Note(note_view(&base, &token, &dir, &n))));
                tell(&last, Reply::Opened(key, r));
            }
            Ok(Cmd::OpenPage(p)) => {
                let key = format!("page:{}:{}", p.account, p.id);
                let r = account_of(&cfg, &mut dirs, p.account).and_then(|(base, token, dir)| get(&base, &token, &format!("/api/v1/pages/{}", p.id)).map(|v| Box::new(Body::Page(page_view(&base, &dir, &p, &v)))));
                if let Err(e) = &r {
                    log(format!("digin: couldn't open page {}: {e}", p.title));
                }
                tell(&last, Reply::Opened(key, r));
            }
            Ok(Cmd::SetStatus { key, account, issue, status }) => {
                let req = set_status_request(issue, &status);
                let r = account_of(&cfg, &mut dirs, account).and_then(|(base, token, _)| send(&base, &token, &req)).map(|v| {
                    let x = v.get("issue").cloned().unwrap_or_default();
                    let now = StatusOpt { id: int_of(&x, "statusId"), name: text_of(&x, "statusName"), category: text_of(&x, "status") };
                    if now.category.is_empty() { status.clone() } else { now }
                });
                match &r {
                    Ok(now) => {
                        log(format!("digin: issue {issue} moved to {}", now.name));
                        // Show it in the list right away; a closed issue leaves "my open issues".
                        if now.closes() {
                            last.issues.retain(|x| !(x.account == account && x.id == issue));
                        } else if let Some(x) = last.issues.iter_mut().find(|x| x.account == account && x.id == issue) {
                            x.status = now.category.clone();
                            x.status_name = now.name.clone();
                        }
                        next = Instant::now();
                    }
                    Err(e) => log(format!("digin: couldn't move issue {issue}: {e}")),
                }
                tell(&last, Reply::Saved(key, r));
            }
            Ok(Cmd::MarkRead { key, account, id, channel, message }) => {
                let r = match read_request(id, channel, message) {
                    Some(req) => account_of(&cfg, &mut dirs, account).and_then(|(base, token, _)| send(&base, &token, &req)).map(|_| ()),
                    None => Err("this line can't be marked read from here".to_string()),
                };
                match &r {
                    Ok(()) => {
                        last.inbox.retain(|n| !(n.account == account && n.id == id));
                        next = Instant::now();
                    }
                    Err(e) => log(format!("digin: couldn't mark {id} read: {e}")),
                }
                tell(&last, Reply::Read(key, r));
            }
            Ok(Cmd::Nudge(after)) => nudge = Some(Instant::now() + after),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if nudge.is_some_and(|t| Instant::now() >= t) {
            nudge = None;
            tell(&last, Reply::Tick);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_server_addresses() {
        assert_eq!(normalize("digin.work/"), "https://digin.work");
        assert_eq!(normalize(" https://x.example.com "), "https://x.example.com");
        assert_eq!(host_of("https://digin.work"), "digin.work");
    }

    /// /api/v1/bootstrap, cut down to what quaked reads (field names from digin-rust's
    /// crates/api/src/bootstrap.rs, statuses/src/lib.rs, projects/src/pg.rs, accounts/src/pg.rs).
    fn bootstrap() -> Value {
        json!({
            "me": {"id": 7, "name": "Austin", "handle": "austin", "email": "a@example.com"},
            "users": [{"id": 7, "name": "Austin", "handle": "austin"}, {"id": 9, "name": "Priya", "handle": "priya"}],
            "workspaces": [{"id": 1, "uuid": "ws-uuid", "name": "Arlint"}],
            "projects": [{"id": 3, "workspaceId": 1, "key": "DIG", "name": "Digin"}],
            "issueStatuses": [
                {"id": 33, "projectId": 3, "name": "Done", "category": "done", "color": "", "position": 5, "issueCount": 0},
                {"id": 30, "projectId": 3, "name": "Todo", "category": "todo", "color": "", "position": 1, "issueCount": 0},
                {"id": 31, "projectId": 3, "name": "In progress", "category": "in_progress", "color": "", "position": 2, "issueCount": 0},
                {"id": 34, "projectId": 3, "name": "Canceled", "category": "canceled", "color": "", "position": 6, "issueCount": 0},
                {"id": 32, "projectId": 3, "name": "In review", "category": "waiting", "color": "", "position": 3, "issueCount": 0}
            ]
        })
    }

    #[test]
    fn reads_the_directory() {
        let d = directory_from(&bootstrap());
        assert_eq!(d.me, 7);
        assert_eq!(d.user(Some(9)), "Priya");
        assert_eq!(d.projects.get(&3), Some(&("DIG".to_string(), "ws-uuid".to_string())));
        let names: Vec<&str> = d.statuses[&3].iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Todo", "In progress", "In review", "Done", "Canceled"]);
    }

    /// GET /api/v1/issues/{id}: `IssueDetail` (crates/issues/src/http.rs) with `Issue`
    /// (issues/src/pg.rs), `CommentView` and `ChecklistItem`.
    fn issue_detail() -> Value {
        json!({
            "issue": {"id": 4512, "projectId": 3, "key": "DIG", "number": 612, "title": "Runner pool drains", "description": "Steps:\n\n[[steps]]\n\nSee **logs**.",
                      "status": "in_progress", "statusId": 31, "statusName": "In progress", "priority": 1, "assigneeId": 7, "updatedAt": "2026-11-16T10:00:00Z", "comments": 2},
            "descriptionHtml": "<p>…</p>",
            "comments": [
                {"id": 1, "issueId": 4512, "authorId": 9, "body": "First look", "createdAt": "2026-11-16T09:00:00Z", "updatedAt": "", "internal": false, "bodyHtml": ""},
                {"id": 2, "issueId": 4512, "authorId": 7, "body": "Fixed in `main`", "createdAt": "2026-11-16T09:30:00Z", "updatedAt": "", "internal": false, "bodyHtml": ""}
            ],
            "checklist": [{"id": 2, "issueId": 4512, "position": 2, "text": "ship", "done": false}, {"id": 1, "issueId": 4512, "position": 1, "text": "repro", "done": true}],
            "activity": [], "attachments": [], "children": [], "blocking": {}, "parent": null, "backlinks": [], "planned": [], "watching": true, "watchers": []
        })
    }

    #[test]
    fn reads_an_issue() {
        let d = directory_from(&bootstrap());
        let x = issue_view(0, "https://digin.work", &d, &issue_detail());
        assert_eq!((x.id, x.key.as_str(), x.status_id, x.priority), (4512, "DIG-612", 31, 1));
        assert_eq!(x.project, "Digin");
        assert_eq!(x.assignee, "you");
        assert_eq!(x.url, "https://digin.work/w/ws-uuid/DIG/issues/612");
        assert_eq!(x.comment_count, 2);
        // Newest first, authors by name, Markdown flattened.
        assert_eq!(x.comments[0].author, "Austin");
        assert_eq!(x.comments[0].text, vec![Block::Para("Fixed in main".into())]);
        assert_eq!(x.comments[1].author, "Priya");
        // The checklist goes where [[steps]] was, in order.
        assert_eq!(
            x.description,
            vec![Block::Para("Steps:".into()), Block::Item("✓".into(), 0, "repro".into()), Block::Item("○".into(), 0, "ship".into()), Block::Para("See logs.".into())]
        );
        let body = Body::Issue(x);
        // Statuses in order, then "Open in Digin"; turning lands on the current one, a press
        // on the next one.
        assert_eq!(body.actions().len(), 6);
        assert_eq!(body.entry_action(), 1);
        assert_eq!(body.press_action(), 2);
    }

    #[test]
    fn press_never_lands_on_canceled() {
        let d = directory_from(&bootstrap());
        let mut v = issue_detail();
        v["issue"]["statusId"] = json!(33);
        v["issue"]["status"] = json!("done");
        let body = Body::Issue(issue_view(0, "https://digin.work", &d, &v));
        assert_eq!(body.entry_action(), 3);
        assert_eq!(body.press_action(), 3); // stays on Done rather than offering Canceled
    }

    #[test]
    fn falls_back_to_categories_without_statuses() {
        let x = issue_view(0, "https://digin.work", &Directory::default(), &issue_detail());
        assert_eq!(x.statuses.len(), 6);
        assert_eq!(x.statuses[2], StatusOpt { id: 0, name: "In progress".into(), category: "in_progress".into() });
        assert_eq!(Body::Issue(x).current(), Some(2));
        let req = set_status_request(4512, &StatusOpt { id: 0, name: "Done".into(), category: "done".into() });
        assert_eq!(req.body, json!({"status": "done"}));
    }

    #[test]
    fn builds_the_writes() {
        let req = set_status_request(4512, &StatusOpt { id: 33, name: "Done".into(), category: "done".into() });
        assert_eq!(req, Request { method: "PATCH", path: "/api/v1/issues/4512".into(), body: json!({"statusId": 33}) });
        assert_eq!(read_request(88, 0, 0), Some(Request { method: "POST", path: "/api/v1/notifications/read".into(), body: json!({"id": 88}) }));
        // A conversation's line: read the conversation up to its newest message.
        assert_eq!(read_request(-501, 12, 501), Some(Request { method: "POST", path: "/api/v1/channels/12/read".into(), body: json!({"messageId": 501}) }));
        assert_eq!(read_request(-501, 12, 0).map(|r| r.body), Some(json!({"messageId": 501})));
        assert_eq!(read_request(0, 0, 0), None);
    }

    #[test]
    fn asks_for_write_only_where_the_board_acts() {
        assert_eq!(permits(), json!({"issues": "write", "pages": "read", "chat": "write", "profile": "write"}));
    }

    #[test]
    fn says_when_a_permit_is_missing() {
        // crates/api/src/guard.rs refuse_token: 403 token_not_allowed.
        let body = br#"{"error":{"code":"token_not_allowed","message":"This API token does not hold write on issues."}}"#;
        assert!(needs_permit(&error_of(403, body)));
        assert_eq!(error_of(403, br#"{"error":{"code":"forbidden","message":"Nope"}}"#), "not allowed: Nope");
    }

    #[test]
    fn reads_inbox_lines() {
        // A notification, and a conversation's line (crates/chat/src/lines.rs: id is minus the message).
        let n = note_of(0, "https://digin.work", &json!({"id": 41, "kind": "mentioned", "title": "#release in Arlint", "body": "@austin sign off?", "url": "/w/ws-uuid/chat/12?m=900", "createdAt": "2026-11-16T10:00:00Z", "readAt": null}));
        assert_eq!((n.id, n.channel, n.message), (41, 0, 0));
        assert_eq!(n.url, "https://digin.work/w/ws-uuid/chat/12?m=900");
        assert_eq!(message_of(&n), Some(900));
        let dm = note_of(0, "https://digin.work", &json!({"id": -501, "kind": "message", "title": "Priya", "body": "hi", "url": "/chat/12", "createdAt": "", "channelId": 12, "messageId": 501}));
        assert_eq!((dm.channel, dm.message), (12, 501));
        assert_eq!(message_of(&dm), Some(501));
    }

    #[test]
    fn finds_the_issue_and_comment_a_line_is_about() {
        assert_eq!(issue_ref("https://digin.work/w/ws-uuid/DIG/issues/612"), Some(("ws-uuid".into(), "DIG".into(), 612)));
        assert_eq!(issue_ref("https://digin.work/KES/issues/41#c"), Some((String::new(), "KES".into(), 41)));
        assert_eq!(issue_ref("https://digin.work/chat/12"), None);
        let issue = issue_detail();
        let long = "Fixed in `main`";
        assert_eq!(full_comment(&issue, "Fixed in `ma…").map(|c| c.0), Some(long.to_string()));
        assert_eq!(full_comment(&issue, "nothing like it…"), None);
    }

    #[test]
    fn reads_a_page() {
        let d = directory_from(&bootstrap());
        let listed = DocPage { account: 0, id: 77, title: "Old".into(), project: "DIG".into(), changed: true, ..Default::default() };
        let v = json!({"page": {"id": 77, "projectId": 3, "title": "Release checklist", "body": "# Steps\n\n1. Tag\n2. Ship", "updatedBy": 9, "updatedAt": "2026-11-16T10:00:00Z"}, "bodyHtml": "", "attachments": [], "backlinks": [], "refs": []});
        let p = page_view("https://digin.work", &d, &listed, &v);
        assert_eq!((p.title.as_str(), p.project.as_str(), p.updated_by.as_str(), p.changed), ("Release checklist", "Digin", "Priya", true));
        assert_eq!(p.url, "https://digin.work/w/ws-uuid/DIG/pages/77");
        assert_eq!(p.text, vec![Block::Heading(1, "Steps".into()), Block::Item("1.".into(), 0, "Tag".into()), Block::Item("2.".into(), 0, "Ship".into())]);
    }

    #[test]
    fn flattens_markdown() {
        let md = "Intro with a [link](https://x.y) and [[Runner notes|notes]].\nSame paragraph, `code`, **bold**, _it_, snake_case_name.\n\n## Heading ##\n\n- one\n  - nested\n* [x] done\n> quoted\n> more\n\n```rust\nfn main() {\n    go();\n}\n```\n\n```mermaid\ngraph TD\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n---\nTitle\n=====";
        assert_eq!(
            markdown(md),
            vec![
                Block::Para("Intro with a link and notes. Same paragraph, code, bold, it, snake_case_name.".into()),
                Block::Heading(2, "Heading".into()),
                Block::Item("•".into(), 0, "one".into()),
                Block::Item("•".into(), 1, "nested".into()),
                Block::Item("✓".into(), 0, "done".into()),
                Block::Quote("quoted more".into()),
                Block::Code("fn main() {".into()),
                Block::Code("    go();".into()),
                Block::Code("}".into()),
                Block::Aside("A diagram: open it in Digin to see it.".into()),
                Block::Row(vec!["a".into(), "b".into()], true),
                Block::Row(vec!["1".into(), "2".into()], false),
                Block::Rule,
                Block::Heading(1, "Title".into()),
            ]
        );
        assert_eq!(inline("![a chart](x.png) <br> <https://digin.work> 2 * 3 a<b"), "a chart https://digin.work 2 * 3 a<b");
        assert_eq!(inline("~~gone~~ \\*kept\\*"), "gone *kept*");
    }
}
