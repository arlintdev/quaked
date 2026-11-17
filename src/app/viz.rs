//! Music visualizer: the app side. Tells the viz thread when to listen (visualizer on, Music
//! page showing, a track playing), redraws on its updates, and pulses the ring on the beat.
//! Runs on the main thread with full access to App (it's a child module of app).
use super::*;

/// What we last asked the viz thread for. App's fields live in app.rs, so this bit of
/// main-thread bookkeeping lives here (there is one App per process).
static LISTENING: AtomicBool = AtomicBool::new(false);

/// Between beats the ring rests at this share of its brightness; a beat flashes it to full.
const RING_FLOOR: f32 = 0.3;

impl App {
    /// An update from crate::viz's thread.
    pub(super) fn on_viz(&mut self, u: crate::viz::Update) {
        match u {
            crate::viz::Update::Status(st) => {
                if st != self.viz {
                    self.viz = st;
                    if self.page == PageId::Music {
                        self.redraw();
                    }
                }
            }
        }
    }

    // No knob / touch / page hooks: the visualizer isn't a page, it lives behind Music.

    /// Music is on screen and playing.
    fn viz_music_showing(&self) -> bool {
        self.page == PageId::Music && self.track.as_ref().is_some_and(|t| t.playing)
    }

    /// The ring, when this feature wants it (after League ready checks, questions, alerts).
    /// A solid ring in the page color whose brightness steps with the beat; the viz thread
    /// limits those steps to ~7 a second (device.rs sends ~10 commands a second at most)
    /// and identical commands are never re-sent.
    pub(super) fn viz_ring(&self) -> Option<Command> {
        if !self.cfg.viz.enabled || !self.cfg.viz.ring || !self.viz.pulsing || !self.viz_music_showing() {
            return None;
        }
        let (hue, sat) = config::hue_sat(self.accent());
        let full = self.cfg.ring.brightness as f32;
        let brightness = (full * (RING_FLOOR + (1.0 - RING_FLOOR) * self.viz.pulse.clamp(0.0, 1.0))).round() as u8;
        Some(Command::Ring { effect: proto::EFFECT_SOLID, hue, sat, brightness, speed: 128 })
    }

    /// Called on every loop; return when it next needs waking (timers, animation).
    pub(super) fn viz_tick(&mut self, _now: Instant) -> Option<Instant> {
        // Listen only while it's useful (and the board's screen is there to show it).
        let want = self.cfg.viz.enabled && self.gfx.is_some() && self.viz_music_showing();
        if LISTENING.swap(want, Ordering::Relaxed) != want {
            let _ = self.viz_cmd.send(crate::viz::Cmd::Active(want));
        }
        None
    }

    /// While true the board redraws every second.
    pub(super) fn viz_animating(&self) -> bool {
        false // the viz thread's updates drive the redraws
    }

    /// Its part of `quaked ctl status`.
    pub(super) fn viz_json(&self) -> serde_json::Value {
        let v = &self.viz;
        serde_json::json!({
            "enabled": self.cfg.viz.enabled,
            "ring": self.cfg.viz.ring,
            "listening": LISTENING.load(Ordering::Relaxed),
            "capturing": v.capturing,
            "hearing": !v.bands.is_empty(),
            "level": (v.level * 100.0).round() / 100.0,
            "beats": v.beats,
            "pulsing": v.pulsing,
        })
    }
}
