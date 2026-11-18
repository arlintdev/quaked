//! What matters most right now: which page the board goes to when you leave it alone, and
//! whether something new may take the board away from what's showing.
//!
//! Alerts carry an urgency. What's on the board sets the bar: a League game, a focus block
//! or a meeting that's starting only give way to critical things (a meeting in 5 minutes),
//! and a page the board just jumped to only gives way to something more urgent for a minute.
//! While you're using the board nothing jumps; an urgent alert jumps once you stop.
use super::*;
use crate::focus::Phase as FocusPhase;

/// How much something new matters. Higher takes the board from lower.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Urgency {
    /// The top bar says it; no blink, no jump.
    Low,
    /// Blinks and jumps when nothing more important is up.
    Normal,
    /// Someone or something is waiting on you (a review, a mention, a failure).
    High,
    /// Starts in minutes or needs you now.
    Critical,
}

impl Urgency {
    /// The usual urgency of news on each page.
    pub(super) fn of(page: PageId) -> Urgency {
        match page {
            PageId::Ai | PageId::Code | PageId::Ops | PageId::Digin => Urgency::High,
            _ => Urgency::Normal,
        }
    }
}

/// A page the board jumped to keeps it from less urgent news for this long.
const JUMP_HOLD: Duration = Duration::from_secs(60);
/// An urgent alert that came while you were using the board still jumps this long after.
const PENDING_FOR: Duration = Duration::from_secs(120);
const MIN: u64 = 60_000;

/// League phases where the game is (about to be) on.
pub(super) fn in_match(phase: &str) -> bool {
    matches!(phase, "ReadyCheck" | "ChampSelect" | "GameStart" | "InProgress")
}

/// The page to go to when the board has been left alone, given what's going on. Pure, so
/// the rules can be tested.
pub(super) struct Context<'a> {
    pub pages: &'a [PageId],
    pub league_phase: &'a str,
    /// Timed events: (start, end) in unix ms.
    pub meetings: Vec<(u64, u64)>,
    /// When each Claude session waiting on you started waiting (unix ms).
    pub waiting_since: Vec<u64>,
    pub deploying: bool,
    pub focusing: bool,
    pub now: u64,
}

impl Context<'_> {
    fn has(&self, p: PageId) -> bool {
        self.pages.contains(&p)
    }

    /// A meeting starting within `before` or that started less than `after` ago (and isn't over).
    fn meeting(&self, before: u64, after: u64) -> bool {
        self.meetings.iter().any(|&(start, end)| self.now < end && self.now + before >= start && self.now < start + after.max(1))
    }

    pub fn relevant(&self) -> Option<PageId> {
        let order = [
            // In a match, or getting into one.
            (PageId::League, in_match(self.league_phase)),
            // A meeting about to start, or just started (the Join button is there).
            (PageId::Calendar, self.meeting(5 * MIN, 10 * MIN)),
            // A Claude session that started waiting on you in the last 15 minutes.
            (PageId::Ai, self.waiting_since.iter().any(|&t| self.now.saturating_sub(t) < 15 * MIN)),
            // A meeting in the next 15 minutes.
            (PageId::Calendar, self.meeting(15 * MIN, 0)),
            (PageId::Deploy, self.deploying),
            (PageId::Focus, self.focusing),
        ];
        order.iter().find(|(p, on)| *on && self.has(*p)).map(|(p, _)| *p).or_else(|| self.has(PageId::Dash).then_some(PageId::Dash))
    }
}

impl App {
    fn relevance_context(&self) -> Context<'_> {
        let now = status::now_ms();
        Context {
            pages: &self.cfg.pages,
            league_phase: &self.league.phase,
            meetings: if self.cfg.pages.contains(&PageId::Calendar) { self.calendar.events.iter().filter(|e| !e.all_day).map(|e| (e.start_ms, e.end_ms)).collect() } else { Vec::new() },
            waiting_since: self.sessions.iter().filter(|s| s.state == State::Waiting).map(|s| s.since).collect(),
            deploying: self.deploy.running().is_some(),
            focusing: self.focus.phase == FocusPhase::Running,
            now,
        }
    }

    /// Where the board goes when it's been left alone.
    pub(super) fn idle_page(&self) -> Option<PageId> {
        if self.cfg.idle_smart {
            self.relevance_context().relevant()
        } else {
            self.cfg.pages.contains(&PageId::Dash).then_some(PageId::Dash)
        }
    }

    /// The least urgency that may take the board to `target` right now.
    pub(super) fn jump_bar(&self, target: PageId) -> Urgency {
        let ctx = self.relevance_context();
        let game = self.cfg.alerts.quiet_in_games && self.page == PageId::League && in_match(&self.league.phase);
        let focus = self.cfg.alerts.quiet_in_focus && ctx.focusing && target != PageId::Focus;
        let meeting = self.page == PageId::Calendar && ctx.meeting(5 * MIN, 10 * MIN);
        if game || focus || meeting {
            Urgency::Critical
        } else {
            Urgency::Normal
        }
    }

    /// Whether an alert of `urgency` for `page` may jump there now (input aside).
    pub(super) fn may_jump(&self, page: PageId, urgency: Urgency) -> bool {
        if page == self.page || !self.cfg.alerts.jump || urgency == Urgency::Low {
            return false;
        }
        // The board just jumped here for something: only something more urgent moves it.
        if let Some((p, u, at)) = self.jumped {
            if p == self.page && at.elapsed() < JUMP_HOLD && urgency <= u {
                return false;
            }
        }
        urgency >= self.jump_bar(page)
    }

    /// Goes to `page` for an alert (remembered, so less urgent news doesn't take it away).
    pub(super) fn jump_for(&mut self, page: PageId, urgency: Urgency) {
        self.set_page(page);
        self.jumped = Some((page, urgency, Instant::now()));
        self.pending_jump = None;
    }

    /// An urgent alert that couldn't jump because you were using the board.
    pub(super) fn defer_jump(&mut self, page: PageId, urgency: Urgency) {
        if urgency >= Urgency::High && self.pending_jump.is_none_or(|(_, u, _)| urgency >= u) {
            self.pending_jump = Some((page, urgency, Instant::now() + PENDING_FOR));
        }
    }

    /// A session waiting on you whose prompt didn't reach the Quake: say so and jump to it.
    /// When its question or permission prompt did show, that overlay is enough: just a note,
    /// and the board stays on the page you were on.
    fn waiting_tick(&mut self) {
        let Some((id, name, due)) = self.waiting_alert.clone() else { return };
        let prompted = self.asks.iter().any(|a| a.session_id == id);
        let still = self.sessions.iter().position(|s| s.id == id && s.state == State::Waiting);
        let machine = self.sessions.iter().find(|s| s.id == id).map(|s| s.machine.clone());
        if prompted || still.is_none() {
            self.waiting_alert = None;
            if prompted {
                self.alert_from(PageId::Ai, format!("{name} needs you"), false, Urgency::Low, machine.as_deref());
            }
            return;
        }
        if Instant::now() >= due {
            self.waiting_alert = None;
            if self.alert_from(PageId::Ai, format!("{name} needs you"), false, Urgency::of(PageId::Ai), machine.as_deref()) {
                if let Some(i) = still {
                    self.sel = i;
                }
            }
        }
    }

    /// Every loop: a deferred jump once you've stopped using the board.
    pub(super) fn relevance_tick(&mut self) {
        self.waiting_tick();
        let Some((page, urgency, until)) = self.pending_jump else { return };
        if Instant::now() >= until || self.attention.as_ref().is_none_or(|a| a.page != page) {
            self.pending_jump = None;
            return;
        }
        if self.last_input.elapsed() > JUMP_QUIET && self.asks.is_empty() {
            self.pending_jump = None;
            if self.may_jump(page, urgency) {
                self.jump_for(page, urgency);
                self.redraw();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000_000;

    fn ctx(pages: &[PageId]) -> Context<'_> {
        Context { pages, league_phase: "None", meetings: vec![], waiting_since: vec![], deploying: false, focusing: false, now: NOW }
    }

    const ALL: [PageId; 6] = [PageId::Dash, PageId::Ai, PageId::Calendar, PageId::League, PageId::Deploy, PageId::Focus];

    #[test]
    fn nothing_going_on_is_the_dashboard() {
        assert_eq!(ctx(&ALL).relevant(), Some(PageId::Dash));
        assert_eq!(ctx(&[PageId::Ai]).relevant(), None);
    }

    #[test]
    fn meetings_win_as_they_get_close() {
        let mut c = ctx(&ALL);
        c.meetings = vec![(NOW + 14 * MIN, NOW + 44 * MIN)];
        assert_eq!(c.relevant(), Some(PageId::Calendar));
        // A Claude session waiting beats a meeting 14 minutes out...
        c.waiting_since = vec![NOW - 2 * MIN];
        assert_eq!(c.relevant(), Some(PageId::Ai));
        // ...but not one 4 minutes out, or one that just started.
        c.meetings = vec![(NOW + 4 * MIN, NOW + 34 * MIN)];
        assert_eq!(c.relevant(), Some(PageId::Calendar));
        c.meetings = vec![(NOW - 8 * MIN, NOW + 22 * MIN)];
        assert_eq!(c.relevant(), Some(PageId::Calendar));
        // Twelve minutes in, it's not news any more.
        c.meetings = vec![(NOW - 12 * MIN, NOW + 18 * MIN)];
        assert_eq!(c.relevant(), Some(PageId::Ai));
        // Far away, or over: no.
        c.waiting_since.clear();
        c.meetings = vec![(NOW + 40 * MIN, NOW + 70 * MIN), (NOW - 60 * MIN, NOW - 30 * MIN)];
        assert_eq!(c.relevant(), Some(PageId::Dash));
    }

    #[test]
    fn a_game_comes_first() {
        let mut c = ctx(&ALL);
        c.meetings = vec![(NOW + 3 * MIN, NOW + 33 * MIN)];
        c.league_phase = "InProgress";
        assert_eq!(c.relevant(), Some(PageId::League));
        c.league_phase = "Lobby";
        assert_eq!(c.relevant(), Some(PageId::Calendar));
    }

    #[test]
    fn old_waits_deploys_and_focus() {
        let mut c = ctx(&ALL);
        c.waiting_since = vec![NOW - 40 * MIN];
        assert_eq!(c.relevant(), Some(PageId::Dash));
        c.focusing = true;
        assert_eq!(c.relevant(), Some(PageId::Focus));
        c.deploying = true;
        assert_eq!(c.relevant(), Some(PageId::Deploy));
    }

    #[test]
    fn hidden_pages_are_skipped() {
        let pages = [PageId::Dash, PageId::Ai];
        let mut c = ctx(&pages);
        c.meetings = vec![(NOW + 2 * MIN, NOW + 32 * MIN)];
        assert_eq!(c.relevant(), Some(PageId::Dash));
    }

    #[test]
    fn urgency_order() {
        assert!(Urgency::Critical > Urgency::High && Urgency::High > Urgency::Normal && Urgency::Normal > Urgency::Low);
        assert_eq!(Urgency::of(PageId::Code), Urgency::High);
        assert_eq!(Urgency::of(PageId::Docker), Urgency::Normal);
    }
}
