//! DOCKER page across Macs. The Mac with the Quake pulls each paired Mac's containers
//! (`ctl docker`) and lists them with its own, marked with their Mac. Logs and the Start /
//! Stop / Restart / Pause buttons go to the Mac the container runs on (`ctl docker-logs`,
//! `ctl docker-action`); that Mac's own Docker thread does the work.
use super::*;

/// How often the board pulls paired Macs' containers: on the Docker page, and elsewhere
/// (the dashboard and notices still want them).
const PEER_FAST: Duration = Duration::from_secs(3);
const PEER_SLOW: Duration = Duration::from_secs(20);
/// How long a paired Mac keeps its engine on the fast poll after the board looks.
const WATCHED_FOR: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(super) struct DockerPeers {
    /// Each paired Mac's engine, as it last answered.
    got: Vec<(String, docker::Status)>,
    /// A paired Mac's container logs, while that container is open.
    logs: Option<(String, Vec<String>)>,
    /// When to pull again (None = now).
    next: Option<Instant>,
    busy: Arc<AtomicBool>,
    logs_busy: Arc<AtomicBool>,
    /// On a paired Mac: the board is looking at this Mac's containers until then.
    watched_until: Option<Instant>,
}

impl App {
    /// This Mac's containers or a paired Mac's changed: rebuild the board's list.
    pub(super) fn docker_changed(&mut self) {
        let mut st = docker::merge(&self.docker_here, &self.docker_peers.got);
        if let (Some((id, lines)), Level::Detail(open)) = (&self.docker_peers.logs, &self.level) {
            if id == open {
                st.logs = Some((id.clone(), lines.clone()));
            }
        }
        let news = docker_news(&self.docker, &st);
        self.docker = st;
        let ids: Vec<String> = self.docker.containers.iter().map(|c| c.id.clone()).collect();
        self.history.docker_cpu.retain(|id, _| ids.contains(id));
        for ct in self.docker.containers.iter().filter(|c| c.state == "running") {
            render::History::push(self.history.docker_cpu.entry(ct.id.clone()).or_default(), ct.cpu.unwrap_or(0.0));
        }
        let from: Vec<&str> = news.iter().map(|(m, _)| m.as_str()).collect();
        // The side arrow when it's all on one Mac.
        let machine = from.first().copied().filter(|m| from.iter().all(|x| x == m)).unwrap_or("").to_string();
        if let Some(r) = Self::summarize(news.into_iter().map(|(_, n)| n).collect()) {
            self.alert_from(PageId::Docker, r, true, Urgency::of(PageId::Docker), Some(&machine));
        }
        if let Level::Detail(id) = &self.level {
            if self.page == PageId::Docker && !self.docker.containers.iter().any(|c| &c.id == id) {
                self.level = Level::Focus;
            }
        }
        self.tile = self.tile.min(self.tile_count().saturating_sub(1));
        if self.page == PageId::Docker {
            self.redraw();
        }
    }

    /// Where a container runs: Ok(None) here, Ok(Some((name, host, port))) on a paired Mac
    /// that's answering, Err(why not) when that Mac isn't (or it's gone).
    fn docker_peer_of(&self, id: &str) -> Result<Option<(String, String, u16)>, String> {
        let ct = self.docker.containers.iter().find(|c| c.id == id).ok_or("it's gone")?;
        if ct.machine.is_empty() {
            return Ok(None);
        }
        let p = self.peers.peers.iter().find(|p| p.ok && p.name == ct.machine).ok_or_else(|| format!("{} isn't answering", ct.machine))?;
        Ok(Some((p.name.clone(), p.host.clone(), p.port)))
    }

    /// Fetches the open container's logs, from whichever Mac it runs on.
    pub(super) fn docker_logs(&mut self, id: &str) {
        match self.docker_peer_of(id) {
            Err(e) => self.docker_peer_logs(id.to_string(), vec![format!("(logs: {e})")]),
            Ok(None) => {
                let _ = self.docker_cmd.send(docker::Cmd::Logs(id.to_string()));
            }
            Ok(Some((mac, host, port))) => {
                if self.docker_peers.logs_busy.swap(true, Ordering::SeqCst) {
                    return;
                }
                let (key, proxy, busy, id) = (self.cfg.sync.key.clone(), self.proxy.clone(), self.docker_peers.logs_busy.clone(), id.to_string());
                std::thread::Builder::new()
                    .name("docker-peer-logs".into())
                    .spawn(move || {
                        let lines = match sync::request(&host, port, &key, &format!("ctl docker-logs {id}")) {
                            Ok(r) => serde_json::from_slice::<Vec<String>>(&r).unwrap_or_else(|_| vec![format!("(logs: {})", String::from_utf8_lossy(&r).trim())]),
                            Err(e) => vec![format!("(logs from {mac}: {e})")],
                        };
                        busy.store(false, Ordering::SeqCst);
                        let _ = proxy.send_event(Event::PeerDockerLogs { id, lines });
                    })
                    .ok();
            }
        }
    }

    /// A Start / Stop / Restart / Pause button, for the container on whichever Mac.
    pub(super) fn docker_act(&mut self, id: &str, action: &'static str) {
        match self.docker_peer_of(id) {
            Err(e) => {
                self.alert_from(PageId::Docker, format!("Couldn't {action} it: {e}"), false, Urgency::Low, None);
            }
            Ok(None) => {
                let _ = self.docker_cmd.send(docker::Cmd::Action(id.to_string(), action));
            }
            Ok(Some((mac, host, port))) => {
                let name = self.docker.containers.iter().find(|c| c.id == id).map(|c| c.name.clone()).unwrap_or_default();
                device::log(format!("docker: {action} {name} on {mac}"));
                let (key, proxy, id) = (self.cfg.sync.key.clone(), self.proxy.clone(), id.to_string());
                std::thread::Builder::new()
                    .name("docker-peer-action".into())
                    .spawn(move || {
                        let done = sync::request(&host, port, &key, &format!("ctl docker-action {id} {action}")).and_then(|r| crate::calendar::peer_done(&r));
                        let _ = proxy.send_event(Event::PeerDockerDone { mac, name, action, done });
                    })
                    .ok();
            }
        }
    }

    /// A paired Mac answered a button: look again soon, or say why it didn't.
    pub(super) fn docker_peer_done(&mut self, mac: String, name: String, action: &'static str, done: Result<(), String>) {
        match done {
            Ok(()) => self.docker_peers.next = Some(Instant::now() + Duration::from_secs(1)),
            Err(e) => {
                device::log(format!("docker: {mac} couldn't {action} {name}: {e}"));
                self.alert_from(PageId::Docker, format!("Couldn't {action} {name} on {mac}: {e}"), false, Urgency::Low, Some(&mac));
            }
        }
    }

    /// On the board: pulls paired Macs' containers now and then (often on the Docker page).
    /// On a paired Mac: stops polling fast once the board stops looking.
    pub(super) fn docker_tick(&mut self, now: Instant) -> Instant {
        let watched = self.docker_peers.watched_until.is_some_and(|t| now < t);
        self.docker_fast.store(self.page == PageId::Docker || watched, Ordering::Relaxed);
        let next = self.docker_peers.next.unwrap_or(now);
        if now < next {
            return next;
        }
        let fast = self.page == PageId::Docker;
        let next = now + if fast { PEER_FAST } else { PEER_SLOW };
        self.docker_peers.next = Some(next);
        let peers: Vec<(String, String, u16)> = if self.connected { self.peers.peers.iter().filter(|p| p.ok).map(|p| (p.name.clone(), p.host.clone(), p.port)).collect() } else { Vec::new() };
        if peers.is_empty() {
            if !self.docker_peers.got.is_empty() {
                self.docker_peers.got.clear();
                self.docker_changed();
            }
            return next;
        }
        if self.docker_peers.busy.swap(true, Ordering::SeqCst) {
            return next;
        }
        let (key, proxy, busy) = (self.cfg.sync.key.clone(), self.proxy.clone(), self.docker_peers.busy.clone());
        let ask = if fast { "ctl docker watch" } else { "ctl docker" };
        std::thread::Builder::new()
            .name("docker-peers".into())
            .spawn(move || {
                // An older quaked answers "error: unknown command": no containers from it.
                let got: Vec<(String, docker::Status)> =
                    peers.into_iter().filter_map(|(name, host, port)| sync::request(&host, port, &key, ask).ok().and_then(|r| serde_json::from_slice(&r).ok()).map(|st| (name, st))).collect();
                busy.store(false, Ordering::SeqCst);
                let _ = proxy.send_event(Event::PeerDocker(got));
            })
            .ok();
        next
    }

    pub(super) fn docker_from_peers(&mut self, got: Vec<(String, docker::Status)>) {
        if got != self.docker_peers.got {
            self.docker_peers.got = got;
            self.docker_changed();
        }
    }

    pub(super) fn docker_peer_logs(&mut self, id: String, lines: Vec<String>) {
        self.docker_peers.logs = Some((id, lines));
        self.docker_changed();
    }

    // ------------------------------------------------------------ for the board

    /// `ctl docker [watch]`: this Mac's engine and containers, as JSON. "watch" = the board
    /// is on its Docker page, so poll the engine fast for a while.
    pub(super) fn docker_serve(&mut self, watch: bool) -> Vec<u8> {
        if watch {
            self.docker_peers.watched_until = Some(Instant::now() + WATCHED_FOR);
            self.docker_fast.store(true, Ordering::Relaxed);
        }
        let mut out = serde_json::to_vec(&self.docker_here).unwrap_or_default();
        out.push(b'\n');
        out
    }

    /// `ctl docker-action <id> <action>`: a button pressed on the board for a container here.
    pub(super) fn docker_action_from_peer(&mut self, args: &str) -> Vec<u8> {
        let (id, a) = args.trim().split_once(' ').unwrap_or((args.trim(), ""));
        let Some(a) = docker::action(a.trim()) else { return b"error: unknown action\n".to_vec() };
        if !self.docker_here.containers.iter().any(|c| c.id == id) {
            return b"error: no such container here\n".to_vec();
        }
        let _ = self.docker_cmd.send(docker::Cmd::Action(id.to_string(), a));
        b"ok\n".to_vec()
    }

    /// `ctl docker-logs <id>`: a container's logs here, as a JSON list of lines (read off the
    /// event loop; the engine can be slow).
    pub(super) fn docker_logs_for_peer(&mut self, id: &str, reply: Sender<Vec<u8>>) {
        let id = id.trim().to_string();
        if !self.docker_here.containers.iter().any(|c| c.id == id) {
            let _ = reply.send(b"error: no such container here\n".to_vec());
            return;
        }
        let cfg = self.cfg.docker.clone();
        std::thread::Builder::new()
            .name("docker-logs-for-peer".into())
            .spawn(move || {
                let mut out = serde_json::to_vec(&docker::logs(&cfg, &id)).unwrap_or_default();
                out.push(b'\n');
                let _ = reply.send(out);
            })
            .ok();
    }
}

/// Running containers that stopped or started restarting: (their Mac, what happened).
pub(super) fn docker_news(old: &docker::Status, new: &docker::Status) -> Vec<(String, String)> {
    new.containers
        .iter()
        .filter_map(|c| {
            let o = old.containers.iter().find(|o| o.id == c.id)?;
            let what = if c.state == "restarting" { "is restarting" } else { "stopped" };
            let on = if c.machine.is_empty() { String::new() } else { format!(" on {}", c.machine) };
            (o.state == "running" && c.state != "running" && c.state != "paused").then(|| (c.machine.clone(), format!("{}{on} {what}", c.name)))
        })
        .collect()
}
