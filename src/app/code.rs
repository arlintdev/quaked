//! CODE page: opening a pull request on this Mac or a paired one. With more than one Mac,
//! a press (or tap) on a PR shows a button per Mac (Level::Detail holds the one picked);
//! turn to pick, press to open there. A paired Mac opens it through `ctl open-link`.
use super::*;
use crate::render::code::TAP_CANCEL;

impl App {
    /// Knob pressed on the page (past the top level): pick a Mac for the selected PR, or
    /// open it on the one picked.
    pub(super) fn code_press(&mut self) {
        match self.level.clone() {
            Level::Detail(name) => self.code_open_on(&name),
            _ => self.code_pick(self.tile),
        }
    }

    /// PR `i` chosen: open it here when this is the only Mac, else ask which.
    pub(super) fn code_pick(&mut self, i: usize) {
        if self.code.all().get(i).is_none() {
            return;
        }
        self.tile = i;
        let macs = self.desk_macs();
        if macs.len() < 2 {
            self.level = Level::Focus;
            self.open_pr(i);
            return;
        }
        let last = &self.cfg.code.open_on;
        let name = macs.iter().find(|m| *m == last).unwrap_or(&macs[0]).clone();
        self.level = Level::Detail(name);
    }

    /// Knob turned while picking a Mac: the next one left or right.
    pub(super) fn code_turn(&mut self, d: i32) {
        let Level::Detail(cur) = &self.level else { return };
        let macs = self.desk_macs();
        let order = config::left_to_right(&macs, &self.cfg.sides);
        if order.is_empty() {
            return;
        }
        let at = order.iter().position(|&i| &macs[i] == cur).unwrap_or(0) as i32;
        let next = (at + d).clamp(0, order.len() as i32 - 1) as usize;
        self.level = Level::Detail(macs[order[next]].clone());
    }

    /// Hit::Feature(_, i): a Mac's button (index into desk_macs), or the card around them.
    pub(super) fn code_tap(&mut self, i: usize) {
        if !matches!(self.level, Level::Detail(_)) {
            return;
        }
        if i == TAP_CANCEL {
            self.level = Level::Focus;
            return;
        }
        if let Some(name) = self.desk_macs().get(i).cloned() {
            self.code_open_on(&name);
        }
    }

    /// Opens the selected PR on the Mac named `name` (this one or a paired one).
    fn code_open_on(&mut self, name: &str) {
        self.level = Level::Focus;
        let Some(p) = self.code.all().get(self.tile).map(|p| (*p).clone()) else { return };
        if self.cfg.code.open_on != name {
            self.cfg.code.open_on = name.to_string();
            self.save_config();
        }
        if name == self.peers.name {
            open_target(&p.url);
            return;
        }
        let what = render::pr_ref(&p);
        let Some(peer) = self.peers.peers.iter().find(|x| x.ok && x.name == name) else {
            open_target(&p.url);
            self.alert_from(PageId::Code, format!("Opened {what} here ({name} isn't answering)"), false, Urgency::Low, None);
            return;
        };
        device::log(format!("code: open {what} on {name}"));
        let (host, port, key, proxy, name, url) = (peer.host.clone(), peer.port, self.cfg.sync.key.clone(), self.proxy.clone(), name.to_string(), p.url.clone());
        std::thread::Builder::new()
            .name("code-on-peer".into())
            .spawn(move || {
                let done = sync::request(&host, port, &key, &format!("ctl open-link {url}")).and_then(|r| crate::calendar::peer_done(&r));
                let _ = proxy.send_event(Event::OpenedOn { mac: name, url, what, done });
            })
            .ok();
    }

    /// A paired Mac answered Event::OpenedOn: say so, or open it here after all.
    pub(super) fn code_opened_on(&mut self, mac: String, url: String, what: String, done: Result<(), String>) {
        match done {
            Ok(()) => {
                self.alert_from(PageId::Code, format!("Opened {what} on {mac}"), false, Urgency::Low, Some(&mac));
            }
            Err(e) => {
                device::log(format!("code: {mac} couldn't open {what}: {e}"));
                open_target(&url);
                self.alert_from(PageId::Code, format!("Opened {what} here ({mac}: {e})"), false, Urgency::Low, None);
            }
        }
    }

    /// `ctl open-link <url>` from a paired Mac: open a web page here (https only).
    pub(super) fn code_open_link(&mut self, url: &str) -> Vec<u8> {
        let url = url.trim();
        if !url.starts_with("https://") || url.chars().any(char::is_whitespace) {
            return b"error: only https links\n".to_vec();
        }
        open_target(url);
        b"ok\n".to_vec()
    }
}
