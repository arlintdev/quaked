//! CODE page data: your open pull requests (CI and review state) and the reviews waiting
//! on you, through the `gh` CLI's existing login (one GraphQL query per refresh), plus
//! GitLab merge requests through `glab` for every host it is signed in to.

use crate::config::Code as CodeCfg;
use crate::device::log;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

const GH: [&str; 2] = ["/opt/homebrew/bin/gh", "/usr/local/bin/gh"];
const GLAB: [&str; 2] = ["/opt/homebrew/bin/glab", "/usr/local/bin/glab"];
/// How long to wait before retrying a GitLab host whose sign-in failed.
const GITLAB_AUTH_BACKOFF: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pr {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    /// SUCCESS, FAILURE, ERROR, PENDING, EXPECTED, or "" (no checks)
    pub ci: String,
    /// APPROVED, CHANGES_REQUESTED, REVIEW_REQUIRED, or ""
    pub review: String,
    /// MERGEABLE, CONFLICTING, UNKNOWN
    pub mergeable: String,
    pub draft: bool,
    pub author: String,
    /// Unix ms
    pub updated: u64,
    /// A GitLab merge request (shown as "!12" instead of "#12").
    pub gitlab: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub login: String,
    pub mine: Vec<Pr>,
    pub reviews: Vec<Pr>,
    /// GitHub's last error (the lists keep their previous contents).
    pub error: Option<String>,
    /// GitHub answered at least once.
    pub ok: bool,
    /// GitLab hosts glab is configured for.
    pub gitlab_hosts: Vec<String>,
    /// At least one GitLab host answered.
    pub gitlab_ok: bool,
    pub gitlab_error: Option<String>,
}

impl Status {
    /// Mine first, then review requests: the order the knob walks through.
    pub fn all(&self) -> Vec<&Pr> {
        self.mine.iter().chain(self.reviews.iter()).collect()
    }
}

#[derive(Debug)]
pub enum Cmd {
    Configure(CodeCfg),
    Refresh,
}

const QUERY: &str = r#"query {
  viewer {
    login
    pullRequests(first: 30, states: OPEN, orderBy: {field: UPDATED_AT, direction: DESC}) {
      nodes {
        number title url isDraft updatedAt reviewDecision mergeable
        repository { nameWithOwner }
        commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
      }
    }
  }
  reviews: search(query: "is:pr is:open review-requested:@me archived:false", type: ISSUE, first: 15) {
    nodes {
      ... on PullRequest {
        number title url updatedAt isDraft
        author { login }
        repository { nameWithOwner }
        commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
      }
    }
  }
}"#;

/// ISO-8601 "2026-11-16T17:23:45Z" -> unix ms.
pub fn iso_ms(s: &str) -> u64 {
    let b = s.as_bytes();
    if b.len() < 19 {
        return 0;
    }
    let n = |r: std::ops::Range<usize>| s.get(r).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
    let (year, month, day, h, mi, se) = (n(0..4), n(5..7), n(8..10), n(11..13), n(14..16), n(17..19));
    let (y, m) = if month <= 2 { (year - 1, month + 9) } else { (year, month - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    ((days * 86_400 + h * 3600 + mi * 60 + se) * 1000).max(0) as u64
}

fn pr_from(v: &Value) -> Option<Pr> {
    let s = |p: &str| v.pointer(p).and_then(Value::as_str).unwrap_or("").to_string();
    let number = v.get("number")?.as_u64()?;
    Some(Pr {
        repo: s("/repository/nameWithOwner"),
        number,
        title: s("/title"),
        url: s("/url"),
        ci: s("/commits/nodes/0/commit/statusCheckRollup/state"),
        review: s("/reviewDecision"),
        mergeable: s("/mergeable"),
        draft: v.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        author: s("/author/login"),
        updated: iso_ms(&s("/updatedAt")),
        gitlab: false,
    })
}

/// Hosts from glab's config (the `hosts:` map), unless the config names them.
fn glab_hosts(cfg: &CodeCfg) -> Vec<String> {
    if !cfg.gitlab_hosts.is_empty() || !cfg.gitlab {
        return if cfg.gitlab { cfg.gitlab_hosts.clone() } else { Vec::new() };
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [format!("{home}/Library/Application Support/glab-cli/config.yml"), format!("{home}/.config/glab-cli/config.yml")];
    let Some(text) = candidates.iter().find_map(|p| std::fs::read_to_string(p).ok()) else { return Vec::new() };
    let mut hosts = Vec::new();
    let mut in_hosts = false;
    for line in text.lines() {
        if line.starts_with("hosts:") {
            in_hosts = true;
        } else if in_hosts && !line.starts_with(' ') && !line.starts_with('#') && !line.trim().is_empty() {
            break;
        } else if in_hosts {
            // exactly one level of indentation: "    gitlab.com:"
            let indent = line.len() - line.trim_start().len();
            let t = line.trim();
            if (2..=4).contains(&indent) && t.ends_with(':') && !t.starts_with('#') {
                hosts.push(t.trim_end_matches(':').to_string());
            }
        }
    }
    hosts
}

fn glab_api(host: &str, path: &str) -> Result<Value, String> {
    let glab = GLAB.iter().find(|p| std::path::Path::new(p).exists()).ok_or("glab CLI not found")?;
    let out = std::process::Command::new(glab).args(["api", "--hostname", host, path]).stdin(std::process::Stdio::null()).output().map_err(|e| e.to_string())?;
    let body = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let all = format!("{body} {}", String::from_utf8_lossy(&out.stderr));
        if all.contains("401") || all.contains("invalid_token") || all.contains("revoked") {
            return Err("not signed in".into());
        }
        return Err(crate::status::squash(&all, 80));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())
}

fn mr_from(v: &Value) -> Option<Pr> {
    let s = |p: &str| v.pointer(p).and_then(Value::as_str).unwrap_or("").to_string();
    let status = s("/detailed_merge_status");
    let reference = s("/references/full");
    Some(Pr {
        repo: reference.split('!').next().unwrap_or("").to_string(),
        number: v.get("iid")?.as_u64()?,
        title: s("/title"),
        url: s("/web_url"),
        ci: match status.as_str() {
            "ci_still_running" => "PENDING",
            "ci_must_pass" => "FAILURE",
            "mergeable" => "SUCCESS",
            _ => "",
        }
        .into(),
        review: if status == "not_approved" { "REVIEW_REQUIRED".into() } else { String::new() },
        mergeable: if status == "conflict" || v.get("has_conflicts").and_then(Value::as_bool).unwrap_or(false) { "CONFLICTING".into() } else { "MERGEABLE".into() },
        draft: v.get("draft").and_then(Value::as_bool).unwrap_or(false),
        author: s("/author/username"),
        updated: iso_ms(&s("/updated_at")),
        gitlab: true,
    })
}

/// (your open MRs, MRs waiting on your review) on one GitLab host.
fn fetch_gitlab(host: &str) -> Result<(Vec<Pr>, Vec<Pr>), String> {
    let me = glab_api(host, "user")?;
    let user = me.get("username").and_then(Value::as_str).ok_or("no username")?.to_string();
    let list = |v: Value| -> Vec<Pr> { v.as_array().map(|a| a.iter().filter_map(mr_from).collect()).unwrap_or_default() };
    let mine = list(glab_api(host, "merge_requests?scope=created_by_me&state=opened&order_by=updated_at&per_page=30")?);
    let reviews = list(glab_api(host, &format!("merge_requests?scope=all&state=opened&reviewer_username={user}&order_by=updated_at&per_page=15"))?);
    Ok((mine, reviews))
}

fn fetch() -> Result<Status, String> {
    let gh = GH.iter().find(|p| std::path::Path::new(p).exists()).ok_or("gh CLI not found")?;
    let out = std::process::Command::new(gh)
        .args(["api", "graphql", "-f", &format!("query={QUERY}")])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(crate::status::squash(&String::from_utf8_lossy(&out.stderr), 120));
    }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let data = v.get("data").ok_or("no data")?;
    let list = |p: &str| -> Vec<Pr> { data.pointer(p).and_then(Value::as_array).map(|a| a.iter().filter_map(pr_from).collect()).unwrap_or_default() };
    Ok(Status {
        login: data.pointer("/viewer/login").and_then(Value::as_str).unwrap_or("").to_string(),
        mine: list("/viewer/pullRequests/nodes"),
        reviews: list("/reviews/nodes"),
        error: None,
        ok: true,
        ..Default::default()
    })
}

pub fn spawn(cfg: CodeCfg, fast: Arc<AtomicBool>, emit: impl Fn(Status) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("github".into()).spawn(move || run(cfg, fast, rx, emit)).unwrap();
    tx
}

/// Newest first, the way both services list them.
fn by_updated(mut v: Vec<Pr>) -> Vec<Pr> {
    v.sort_by(|a, b| b.updated.cmp(&a.updated));
    v
}

#[derive(Default)]
struct GitlabHost {
    mine: Vec<Pr>,
    reviews: Vec<Pr>,
    ok: bool,
    error: Option<String>,
    next: Option<Instant>,
}

fn run(mut cfg: CodeCfg, fast: Arc<AtomicBool>, rx: Receiver<Cmd>, emit: impl Fn(Status)) {
    let mut last = Status::default();
    let mut github = Status::default();
    let mut gitlab: Vec<(String, GitlabHost)> = Vec::new();
    let mut next = Instant::now();
    let mut last_error = String::new();
    let mut fetched = Instant::now().checked_sub(Duration::from_secs(3600)).unwrap_or_else(Instant::now);
    loop {
        if Instant::now() >= next {
            fetched = Instant::now();
            match fetch() {
                Ok(s) => {
                    last_error.clear();
                    github = s;
                }
                Err(e) => {
                    if e != last_error {
                        log(format!("github: {e}"));
                        last_error = e.clone();
                    }
                    github.error = Some(e);
                }
            }
            let hosts = glab_hosts(&cfg);
            if gitlab.iter().map(|(h, _)| h).ne(hosts.iter()) {
                gitlab = hosts.iter().map(|h| (h.clone(), GitlabHost::default())).collect();
            }
            for (host, g) in gitlab.iter_mut() {
                if g.next.is_some_and(|t| Instant::now() < t) {
                    continue;
                }
                g.next = None;
                match fetch_gitlab(host) {
                    Ok((mine, reviews)) => {
                        *g = GitlabHost { mine, reviews, ok: true, ..Default::default() };
                    }
                    Err(e) => {
                        if g.error.as_ref() != Some(&e) {
                            log(format!("gitlab {host}: {e}"));
                        }
                        if e == "not signed in" {
                            g.next = Some(Instant::now() + GITLAB_AUTH_BACKOFF);
                        }
                        g.error = Some(e);
                    }
                }
            }
            let failing: Vec<String> = gitlab.iter().filter(|(_, g)| g.error.is_some()).map(|(h, g)| format!("{h} {}", g.error.as_deref().unwrap_or(""))).collect();
            let st = Status {
                mine: by_updated(github.mine.iter().cloned().chain(gitlab.iter().flat_map(|(_, g)| g.mine.iter().cloned())).collect()),
                reviews: by_updated(github.reviews.iter().cloned().chain(gitlab.iter().flat_map(|(_, g)| g.reviews.iter().cloned())).collect()),
                gitlab_hosts: hosts,
                gitlab_ok: gitlab.iter().any(|(_, g)| g.ok),
                gitlab_error: (!failing.is_empty()).then(|| failing.join(", ")),
                ..github.clone()
            };
            if st != last {
                last = st.clone();
                emit(st);
            }
            let secs = cfg.refresh_seconds.max(15);
            next = Instant::now() + Duration::from_secs(if fast.load(Ordering::Relaxed) { secs / 2 } else { secs });
        }
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Cmd::Configure(c)) => cfg = c,
            // Opening the page asks for fresh data, but not more than every 20 seconds.
            Ok(Cmd::Refresh) => next = next.min(fetched + Duration::from_secs(20)),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iso_dates() {
        assert_eq!(iso_ms("1970-01-01T00:00:00Z"), 0);
        assert_eq!(iso_ms("2026-11-16T00:00:00Z"), 1_794_787_200_000);
    }
}
