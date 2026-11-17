//! Music visualizer: listens to what the Mac plays through a Core Audio process tap
//! (macOS 14.2+), turns it into a spectrum and a beat pulse, and hands that to the board,
//! which draws it behind the Music page and pulses the knob's ring on the beat.
//!
//! It only listens while the app asks it to (`Cmd::Active`: enabled, Music page showing, a
//! track playing), so there is no capture, CPU or recording indicator otherwise. The first
//! capture shows macOS's "System Audio Recording" prompt for Quaked.app; if that is denied
//! the tap just delivers silence, so the board shows nothing and the log says why, once.

use crate::config::VizSettings as Cfg;
use crate::device::log;
use std::collections::VecDeque;
use std::f32::consts::PI;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// Spectrum bands, log-spaced from `F_LO` to `F_HI`.
pub const BANDS: usize = 44;
const F_LO: f32 = 40.0;
const F_HI: f32 = 16_000.0;
/// FFT windows (Hann), in samples: the short one for most bands, the long one for the bass.
const N: usize = 2048;
const N_LONG: usize = 8192;
/// Bands centred below this use the long window.
const SPLIT_HZ: f32 = 250.0;
/// A band at `DB_FLOOR` (dB relative to a full-scale sine) draws as 0, at `DB_CEIL` as 1.
const DB_FLOOR: f32 = -60.0;
const DB_CEIL: f32 = -6.0;
/// Music falls off faster than pink noise; lift the highs this much per octave above 1 kHz.
const TILT_DB_PER_OCTAVE: f32 = 2.5;
const ATTACK_S: f32 = 0.025;
const RELEASE_S: f32 = 0.16;
const PEAK_HOLD_S: f32 = 0.5;
/// Peaks fall this much (of full scale) per second once their hold is over.
const PEAK_FALL: f32 = 0.8;
/// Values this small snap to 0, so silence settles and stops sending updates.
const SNAP: f32 = 0.004;

// Beats: energy in the kick drum's range against its own recent average.
const BEAT_LO: f32 = 40.0;
const BEAT_HI: f32 = 150.0;
/// The low band must be this far over its ~1 s average (dB) ...
const BEAT_OVER_AVG_DB: f32 = 3.0;
/// ... and have risen this much from its minimum over the last few frames.
const BEAT_RISE_DB: f32 = 4.0;
/// ... and come within this much of the recent loudest hits (so a busy bass line between
/// the kicks doesn't count).
const BEAT_NEAR_PEAK_DB: f32 = 7.0;
/// That peak level falls this much per second (adapting to quieter passages).
const BEAT_PEAK_FALL_DB: f32 = 4.0;
/// Quieter than this (dB) is never a beat.
const BEAT_MIN_DB: f32 = -50.0;
const BEAT_AVG_S: f32 = 1.2;
const BEAT_RECENT: usize = 4;
/// At most one beat this often (200 BPM).
const BEAT_REFRACTORY_S: f32 = 0.3;
/// The ring pulses while beats came within this long.
const PULSING_S: f32 = 3.0;
/// The drawn beat glow decays with this time constant.
const GLOW_S: f32 = 0.22;

/// How often the thread analyses and (if anything changed) sends a status: ~30 a second.
const FRAME: Duration = Duration::from_millis(33);
/// Never heard a thing since starting this long ago: say why in the log (once).
const SILENCE_HINT: Duration = Duration::from_secs(10);
/// After a failed start, don't try again (on the next activation) for this long.
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// How often to re-read the tap's sample rate (it follows the output device).
const RATE_CHECK: Duration = Duration::from_secs(2);

/// What the board shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// Spectrum bands, 0.0-1.0, low to high. Empty while not listening or before anything
    /// was heard (so nothing is drawn when capture isn't available).
    pub bands: Vec<f32>,
    /// Peak hold per band, 0.0-1.0.
    pub peaks: Vec<f32>,
    /// Overall loudness, 0.0-1.0.
    pub level: f32,
    /// Smooth beat glow for drawing: 1.0 on a beat, decaying.
    pub beat: f32,
    /// The ring's beat pulse in steps (1.0 on the beat, then 0.4, then 0.0). It changes at
    /// most three times per beat, ~7 times a second, so the USB ring keeps up.
    pub pulse: f32,
    /// Beats came in the last few seconds (the ring pulses only then).
    pub pulsing: bool,
    /// Beats since listening started.
    pub beats: u64,
    /// The tap is running.
    pub capturing: bool,
}

#[derive(Debug)]
pub enum Cmd {
    Configure(Cfg),
    /// Listen (true) or stop (false). The app sends this when "visualizer enabled, Music
    /// page showing, a track playing" changes.
    Active(bool),
}

/// What the module's thread sends the app (handled in app/viz.rs on the main thread).
#[derive(Debug)]
pub enum Update {
    Status(Status),
}

/// Starts the module's background work; `emit` hands updates to the app.
pub fn spawn(cfg: Cfg, emit: impl Fn(Update) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("viz".into()).spawn(move || Engine::new(cfg, emit).run(rx)).unwrap();
    tx
}

/// Sample data for `quaked preview`: a synthetic mix (kick, bass, chords, hats) run through
/// the real analyser, caught just after a kick.
pub fn sample() -> Status {
    let rate = 48_000.0;
    let mut an = Analyzer::new(rate);
    let hop = (rate * FRAME.as_secs_f32()) as usize;
    let mut window = vec![0.0; N_LONG];
    // 124 BPM: kicks every ~0.484 s; stop ~50 ms after the 9th kick.
    let end = (8.0 * 60.0 / 124.0 + 0.05) * rate;
    let mut t = N_LONG;
    while (t as f32) < end {
        synth_mix(t - N_LONG, rate, &mut window);
        an.step(&window, hop as f32 / rate);
        t += hop;
    }
    an.status()
}

// ---------------------------------------------------------------- DSP

/// In-place radix-2 complex FFT.
struct Fft {
    rev: Vec<u32>,
    twiddles: Vec<(f32, f32)>,
}

impl Fft {
    fn new(n: usize) -> Fft {
        assert!(n.is_power_of_two() && n >= 2);
        let bits = n.trailing_zeros();
        let rev = (0..n as u32).map(|i| i.reverse_bits() >> (32 - bits)).collect();
        let twiddles = (0..n / 2)
            .map(|k| {
                let a = -2.0 * PI * k as f32 / n as f32;
                (a.cos(), a.sin())
            })
            .collect();
        Fft { rev, twiddles }
    }

    fn run(&self, re: &mut [f32], im: &mut [f32]) {
        let n = self.rev.len();
        for i in 0..n {
            let j = self.rev[i] as usize;
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..half {
                    let (wr, wi) = self.twiddles[k * step];
                    let (a, b) = (start + k, start + k + half);
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len <<= 1;
        }
    }
}

/// Band edges in Hz: `BANDS + 1` log-spaced values from `F_LO` to `F_HI` (or just under
/// Nyquist at low sample rates).
fn band_edges(rate: f32) -> Vec<f32> {
    let hi = F_HI.min(rate * 0.48);
    (0..=BANDS).map(|i| F_LO * (hi / F_LO).powf(i as f32 / BANDS as f32)).collect()
}

/// Integral of the linearly interpolated `power` over [x0, x1] (in bins).
fn integrate(power: &[f32], x0: f32, x1: f32) -> f32 {
    let last = power.len() - 1;
    let (x0, x1) = (x0.clamp(0.0, last as f32), x1.clamp(0.0, last as f32));
    let mut sum = 0.0;
    let mut k = x0.floor() as usize;
    while (k as f32) < x1 && k < last {
        let (a, b) = ((k as f32).max(x0), ((k + 1) as f32).min(x1));
        if b > a {
            let at = |x: f32| power[k] + (power[k + 1] - power[k]) * (x - k as f32);
            sum += (b - a) * (at(a) + at(b)) * 0.5;
        }
        k += 1;
    }
    sum
}

/// One FFT size: its window, buffers and the latest power spectrum (normalised so a
/// full-scale sine sums to 1).
struct Res {
    n: usize,
    fft: Fft,
    hann: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    power: Vec<f32>,
    /// Hz per bin.
    bin: f32,
}

impl Res {
    fn new(n: usize, rate: f32) -> Res {
        Res {
            n,
            fft: Fft::new(n),
            hann: (0..n).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos()).collect(),
            re: vec![0.0; n],
            im: vec![0.0; n],
            power: vec![0.0; n / 2 + 1],
            bin: rate / n as f32,
        }
    }

    /// Spectrum of the newest `n` samples (zero-padded in front if there are fewer).
    fn run(&mut self, samples: &[f32]) {
        let n = self.n;
        let take = samples.len().min(n);
        let (pad, from) = (n - take, samples.len() - take);
        for i in 0..n {
            let s = if i < pad { 0.0 } else { samples[from + i - pad] };
            self.re[i] = s * self.hann[i];
            self.im[i] = 0.0;
        }
        self.fft.run(&mut self.re, &mut self.im);
        // A full-scale sine's Hann-windowed one-sided power sums to 3n²/32.
        let norm = 32.0 / (3.0 * (n * n) as f32);
        for (k, p) in self.power.iter_mut().enumerate() {
            *p = (self.re[k] * self.re[k] + self.im[k] * self.im[k]) * norm;
        }
    }

    /// Power in [lo, hi] Hz. Bands narrower than the window's noise bandwidth (1.5 bins for
    /// Hann) count that much of it, so narrow bands aren't outweighed by their wider
    /// neighbours sharing the same leakage.
    fn band(&self, lo: f32, hi: f32) -> f32 {
        let (x0, x1) = (lo / self.bin, hi / self.bin);
        let width = (x1 - x0).max(1e-6);
        integrate(&self.power, x0, x1) * width.max(1.5) / width
    }
}

/// Turns windows of mono samples into smoothed bands, peaks, a level and beats.
pub struct Analyzer {
    rate: f32,
    /// 2048 points (~43 ms): the mids and highs, the level and the beat timing.
    short: Res,
    /// 8192 points (~170 ms): the bass bands, which are narrower than a short bin.
    long: Res,
    /// Band edges, Hz.
    edges: Vec<f32>,
    /// Per-band tilt (dB).
    tilt: Vec<f32>,
    raw: Vec<f32>,
    bands: Vec<f32>,
    peaks: Vec<f32>,
    peak_age: Vec<f32>,
    level: f32,
    beat: Beat,
}

impl Analyzer {
    pub fn new(rate: f32) -> Analyzer {
        let edges = band_edges(rate);
        let tilt = (0..BANDS)
            .map(|i| {
                let center = (edges[i] * edges[i + 1]).sqrt();
                TILT_DB_PER_OCTAVE * (center / 1000.0).log2().max(0.0)
            })
            .collect();
        Analyzer {
            rate,
            short: Res::new(N, rate),
            long: Res::new(N_LONG, rate),
            edges,
            tilt,
            raw: vec![0.0; BANDS],
            bands: vec![0.0; BANDS],
            peaks: vec![0.0; BANDS],
            peak_age: vec![0.0; BANDS],
            level: 0.0,
            beat: Beat::default(),
        }
    }

    /// Instantaneous (unsmoothed) bands for the newest samples (`N_LONG` of them are used),
    /// plus the kick-range energy and the RMS, both in dB.
    fn analyse(&mut self, samples: &[f32]) -> (f32, f32) {
        self.short.run(samples);
        self.long.run(samples);
        for i in 0..BANDS {
            let (lo, hi) = (self.edges[i], self.edges[i + 1]);
            let res = if (lo * hi).sqrt() < SPLIT_HZ { &self.long } else { &self.short };
            let db = 10.0 * (res.band(lo, hi) + 1e-12).log10() + self.tilt[i];
            self.raw[i] = ((db - DB_FLOOR) / (DB_CEIL - DB_FLOOR)).clamp(0.0, 1.0);
        }
        let low = self.short.band(BEAT_LO, BEAT_HI);
        let recent = &samples[samples.len().saturating_sub(N)..];
        let rms = (recent.iter().map(|s| s * s).sum::<f32>() / N as f32).sqrt();
        (10.0 * (low + 1e-12).log10(), 20.0 * (rms + 1e-9).log10())
    }

    /// One frame: the newest samples, `dt` seconds after the last frame. True on a beat.
    pub fn step(&mut self, samples: &[f32], dt: f32) -> bool {
        let (low_db, rms_db) = self.analyse(samples);
        self.smooth(dt, ((rms_db + 48.0) / 42.0).clamp(0.0, 1.0));
        self.beat.step(low_db, dt)
    }

    /// One frame of silence (no FFT needed).
    pub fn silence(&mut self, dt: f32) {
        self.raw.iter_mut().for_each(|r| *r = 0.0);
        self.smooth(dt, 0.0);
        self.beat.step(-120.0, dt);
    }

    fn smooth(&mut self, dt: f32, level: f32) {
        let up = 1.0 - (-dt / ATTACK_S).exp();
        let down = 1.0 - (-dt / RELEASE_S).exp();
        for i in 0..BANDS {
            let (target, cur) = (self.raw[i], self.bands[i]);
            let mut v = cur + (target - cur) * if target > cur { up } else { down };
            if v < SNAP {
                v = 0.0;
            }
            self.bands[i] = v;
            if v >= self.peaks[i] {
                self.peaks[i] = v;
                self.peak_age[i] = 0.0;
            } else {
                self.peak_age[i] += dt;
                if self.peak_age[i] > PEAK_HOLD_S {
                    self.peaks[i] = (self.peaks[i] - PEAK_FALL * dt).max(v);
                }
                if self.peaks[i] < SNAP {
                    self.peaks[i] = 0.0;
                }
            }
        }
        let k = if level > self.level { 1.0 - (-dt / 0.05).exp() } else { 1.0 - (-dt / 0.3).exp() };
        self.level += (level - self.level) * k;
        if self.level < SNAP {
            self.level = 0.0;
        }
    }

    pub fn status(&self) -> Status {
        Status {
            bands: self.bands.clone(),
            peaks: self.peaks.clone(),
            level: self.level,
            beat: self.beat.glow,
            pulse: self.beat.pulse(),
            pulsing: self.beat.pulsing(),
            beats: self.beat.count,
            capturing: true,
        }
    }
}

/// Kick detector: the low band's energy against its own moving average.
struct Beat {
    avg_db: Option<f32>,
    /// Peak follower of the low band (dB): jumps up to each hit, falls slowly.
    peak_db: f32,
    recent: VecDeque<f32>,
    /// Seconds since the last beat (meaningless before the first).
    since: f32,
    /// Seconds between the last two beats.
    interval: f32,
    count: u64,
    glow: f32,
}

impl Default for Beat {
    fn default() -> Self {
        Beat { avg_db: None, peak_db: -120.0, recent: VecDeque::new(), since: 0.0, interval: 0.0, count: 0, glow: 0.0 }
    }
}

impl Beat {
    fn step(&mut self, e_db: f32, dt: f32) -> bool {
        let e_db = e_db.max(-120.0);
        self.since += dt;
        let avg = *self.avg_db.get_or_insert(e_db);
        let low = self.recent.iter().copied().fold(e_db, f32::min);
        self.peak_db = (self.peak_db - BEAT_PEAK_FALL_DB * dt).max(e_db);
        let onset = e_db > BEAT_MIN_DB
            && e_db - avg > BEAT_OVER_AVG_DB
            && e_db - low > BEAT_RISE_DB
            && e_db > self.peak_db - BEAT_NEAR_PEAK_DB
            && (self.count == 0 || self.since >= BEAT_REFRACTORY_S);
        self.avg_db = Some(avg + (e_db - avg) * (1.0 - (-dt / BEAT_AVG_S).exp()));
        self.recent.push_back(e_db);
        if self.recent.len() > BEAT_RECENT {
            self.recent.pop_front();
        }
        self.glow *= (-dt / GLOW_S).exp();
        if self.glow < 0.01 {
            self.glow = 0.0;
        }
        if onset {
            self.interval = if self.count == 0 { 1.0 } else { self.since };
            self.since = 0.0;
            self.count += 1;
            self.glow = 1.0;
        }
        onset
    }

    fn pulsing(&self) -> bool {
        self.count > 0 && self.since < PULSING_S
    }

    /// The ring's steps: a flash on the beat, a middle step (when the tempo leaves room),
    /// then the floor. At most 3 changes per beat and >= 0.3 s per beat: ~7 a second.
    fn pulse(&self) -> f32 {
        if !self.pulsing() {
            return 0.0;
        }
        let fast = self.interval < 0.42;
        if self.since < 0.12 {
            1.0
        } else if self.since < 0.26 && !fast {
            0.4
        } else {
            0.0
        }
    }
}

/// A deterministic little mix (kick, bass, chords, lead, hats) at `rate`, starting at
/// sample `start`, for the preview and the tests.
fn synth_mix(start: usize, rate: f32, out: &mut [f32]) {
    let beat = 60.0 / 124.0;
    let mut noise = 0x9E37_79B9u32 ^ start as u32;
    let mut prev = 0.0f32;
    for (i, o) in out.iter_mut().enumerate() {
        let t = (start + i) as f32 / rate;
        let in_beat = t % beat;
        let in_eighth = t % (beat / 2.0);
        // Kick: a falling 50-110 Hz thump.
        let kick = 0.85 * (2.0 * PI * (52.0 * in_beat + 1.6 * (1.0 - (-in_beat / 0.03).exp()))).sin() * (-in_beat / 0.11).exp();
        // Bass: D2 saw-ish, gated on eighths.
        let bass: f32 = (1..=6).map(|h| (2.0 * PI * 73.42 * h as f32 * t).sin() / h as f32).sum::<f32>() * 0.16 * (-in_eighth / 0.18).exp();
        // Chords: D minor, soft.
        let chord: f32 = [293.66f32, 349.23, 440.0]
            .iter()
            .map(|f| (1..=8).map(|h| (2.0 * PI * f * h as f32 * t).sin() / (h as f32).powf(1.6)).sum::<f32>())
            .sum::<f32>()
            * 0.035;
        let lead = 0.03 * (2.0 * PI * 880.0 * t + 0.3 * (2.0 * PI * 5.0 * t).sin()).sin();
        // Hats: white noise, differentiated (brighter), on the off-beat eighths.
        noise ^= noise << 13;
        noise ^= noise >> 17;
        noise ^= noise << 5;
        let white = noise as f32 / u32::MAX as f32 * 2.0 - 1.0;
        let bright = white - prev;
        prev = white;
        let hat = if in_beat >= beat / 2.0 { 0.10 * bright * (-in_eighth / 0.035).exp() } else { 0.025 * bright * (-in_eighth / 0.02).exp() };
        *o = kick + bass + chord + lead + hat + 0.002 * white;
    }
}

// ---------------------------------------------------------------- the thread

struct Engine<E: Fn(Update)> {
    cfg: Cfg,
    emit: E,
    active: bool,
    cap: Option<tap::Capture>,
    an: Option<Analyzer>,
    window: Vec<f32>,
    /// The API isn't there (macOS < 14.2): never try again this run.
    unavailable: bool,
    /// When the last start failed; the next activation retries only after `RETRY_AFTER`.
    failed_at: Option<Instant>,
    logged: Vec<String>,
    /// Heard anything (non-silent) since starting this capture / since the daemon started.
    heard: bool,
    heard_ever: bool,
    started: Instant,
    last_frame: Instant,
    last_written: usize,
    last_rate_check: Instant,
    sent: Status,
}

impl<E: Fn(Update)> Engine<E> {
    fn new(cfg: Cfg, emit: E) -> Self {
        let now = Instant::now();
        Engine {
            cfg,
            emit,
            active: false,
            cap: None,
            an: None,
            window: vec![0.0; N_LONG],
            unavailable: false,
            failed_at: None,
            logged: Vec::new(),
            heard: false,
            heard_ever: false,
            started: now,
            last_frame: now,
            last_written: 0,
            last_rate_check: now,
            sent: Status::default(),
        }
    }

    fn run(mut self, rx: Receiver<Cmd>) {
        loop {
            let wait = if self.cap.is_some() { FRAME.saturating_sub(self.last_frame.elapsed()) } else { Duration::from_secs(3600) };
            match rx.recv_timeout(wait) {
                Ok(Cmd::Configure(c)) => {
                    self.cfg = c;
                    self.reconcile();
                }
                Ok(Cmd::Active(a)) => {
                    self.active = a;
                    self.reconcile();
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if self.cap.is_some() && self.last_frame.elapsed() >= FRAME {
                self.frame();
            }
        }
        self.stop();
    }

    fn reconcile(&mut self) {
        let want = self.cfg.enabled && self.active;
        if want && self.cap.is_none() {
            self.start();
        } else if !want && self.cap.is_some() {
            self.stop();
        }
    }

    fn log_once(&mut self, msg: String) {
        if !self.logged.contains(&msg) {
            log(format!("viz: {msg}"));
            self.logged.push(msg);
        }
    }

    fn start(&mut self) {
        if self.unavailable || self.failed_at.is_some_and(|t| t.elapsed() < RETRY_AFTER) {
            return;
        }
        match tap::Capture::start() {
            Ok(cap) => {
                log(format!("viz: listening to system audio ({} Hz)", cap.rate));
                self.an = Some(Analyzer::new(cap.rate));
                self.cap = Some(cap);
                self.failed_at = None;
                self.heard = false;
                self.started = Instant::now();
                self.last_frame = self.started;
                self.last_rate_check = self.started;
                self.last_written = usize::MAX;
                self.send(Status { capturing: true, ..Default::default() });
            }
            Err(tap::Failure::Unavailable(msg)) => {
                self.unavailable = true;
                self.log_once(msg);
            }
            Err(tap::Failure::Error(msg)) => {
                self.failed_at = Some(Instant::now());
                self.log_once(msg);
            }
        }
    }

    fn stop(&mut self) {
        if let Some(cap) = self.cap.take() {
            cap.stop();
            log("viz: stopped listening");
        }
        self.an = None;
        self.send(Status::default());
    }

    fn send(&mut self, st: Status) {
        if st != self.sent {
            (self.emit)(Update::Status(st.clone()));
            self.sent = st;
        }
    }

    fn frame(&mut self) {
        let (Some(cap), Some(an)) = (self.cap.as_ref(), self.an.as_mut()) else { return };
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.25);
        self.last_frame = now;
        if now - self.last_rate_check >= RATE_CHECK {
            self.last_rate_check = now;
            if let Some(rate) = cap.rate().filter(|r| (r - an.rate).abs() > 1.0) {
                log(format!("viz: sample rate now {rate} Hz"));
                *an = Analyzer::new(rate);
            }
        }
        let written = cap.latest(&mut self.window);
        let fresh = written != self.last_written;
        self.last_written = written;
        let silent = !fresh || self.window.iter().all(|&s| s == 0.0);
        if silent {
            an.silence(dt);
        } else {
            an.step(&self.window, dt);
            self.heard = true;
            self.heard_ever = true;
        }
        let st = if self.heard { an.status() } else { Status { capturing: true, ..Default::default() } };
        if !self.heard_ever && self.started.elapsed() >= SILENCE_HINT {
            let msg = if written == 0 {
                "the system audio tap delivers no audio at all".to_string()
            } else {
                "the system audio tap is silent; if music is playing, allow Quaked under System Settings > Privacy & Security > Screen & System Audio Recording (System Audio Recording Only)".to_string()
            };
            self.log_once(msg);
        }
        self.send(st);
    }
}

// ---------------------------------------------------------------- Core Audio process tap

mod tap {
    //! A private global process tap (CATapDescription + AudioHardwareCreateProcessTap) inside
    //! a private aggregate device, read with an IOProc into a lock-free ring of mono samples.

    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::msg_send;
    use objc2_foundation::NSString;
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

    const RING: usize = 1 << 14;

    #[repr(C)]
    struct PropAddr {
        selector: u32,
        scope: u32,
        element: u32,
    }

    #[repr(C)]
    struct AudioBuffer {
        channels: u32,
        bytes: u32,
        data: *mut c_void,
    }

    #[repr(C)]
    struct AudioBufferList {
        count: u32,
        buffers: [AudioBuffer; 1],
    }

    /// AudioStreamBasicDescription.
    #[repr(C)]
    #[derive(Default)]
    struct Asbd {
        rate: f64,
        format: u32,
        flags: u32,
        bytes_per_packet: u32,
        frames_per_packet: u32,
        bytes_per_frame: u32,
        channels: u32,
        bits: u32,
        reserved: u32,
    }

    type IoProc = extern "C" fn(u32, *const c_void, *const AudioBufferList, *const c_void, *mut AudioBufferList, *const c_void, *mut c_void) -> i32;
    /// AudioDeviceIOProcID (an opaque function pointer).
    type IoProcId = *mut c_void;
    type CreateTap = unsafe extern "C" fn(*mut AnyObject, *mut u32) -> i32;
    type DestroyTap = unsafe extern "C" fn(u32) -> i32;

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectGetPropertyData(id: u32, addr: *const PropAddr, qsize: u32, qdata: *const c_void, size: *mut u32, out: *mut c_void) -> i32;
        fn AudioHardwareCreateAggregateDevice(description: *const c_void, out: *mut u32) -> i32;
        fn AudioHardwareDestroyAggregateDevice(id: u32) -> i32;
        fn AudioDeviceCreateIOProcID(device: u32, proc_: IoProc, client: *mut c_void, out: *mut IoProcId) -> i32;
        fn AudioDeviceDestroyIOProcID(device: u32, id: IoProcId) -> i32;
        fn AudioDeviceStart(device: u32, id: IoProcId) -> i32;
        fn AudioDeviceStop(device: u32, id: IoProcId) -> i32;
    }

    /// CFDictionaryKeyCallBacks / CFDictionaryValueCallBacks / CFArrayCallBacks (only their
    /// addresses are used).
    #[repr(C)]
    struct Callbacks6([usize; 6]);
    #[repr(C)]
    struct Callbacks5([usize; 5]);

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFTypeDictionaryKeyCallBacks: Callbacks6;
        static kCFTypeDictionaryValueCallBacks: Callbacks5;
        static kCFTypeArrayCallBacks: Callbacks5;
        fn CFDictionaryCreateMutable(alloc: *const c_void, capacity: isize, keys: *const Callbacks6, values: *const Callbacks5) -> *mut c_void;
        fn CFDictionarySetValue(dict: *mut c_void, key: *const c_void, value: *const c_void);
        fn CFArrayCreate(alloc: *const c_void, values: *const *const c_void, count: isize, callbacks: *const Callbacks5) -> *const c_void;
        fn CFStringCreateWithCString(alloc: *const c_void, s: *const c_char, encoding: u32) -> *const c_void;
        fn CFNumberCreate(alloc: *const c_void, kind: isize, value: *const c_void) -> *const c_void;
        fn CFRelease(cf: *const c_void);
    }

    const fn fourcc(s: &[u8; 4]) -> u32 {
        (s[0] as u32) << 24 | (s[1] as u32) << 16 | (s[2] as u32) << 8 | s[3] as u32
    }
    const SCOPE_GLOBAL: u32 = fourcc(b"glob");
    /// kAudioTapPropertyFormat.
    const TAP_FORMAT: u32 = fourcc(b"tfmt");
    const FORMAT_LPCM: u32 = fourcc(b"lpcm");
    const FLAG_FLOAT: u32 = 1;
    const UTF8: u32 = 0x0800_0100;
    const NUMBER_SINT32: isize = 3;

    pub enum Failure {
        /// Not on this macOS: don't try again.
        Unavailable(String),
        Error(String),
    }

    fn err(what: &str, status: i32) -> Failure {
        let b = status.to_be_bytes();
        let code = if b.iter().all(|c| c.is_ascii_graphic()) { format!("'{}'", String::from_utf8_lossy(&b)) } else { status.to_string() };
        Failure::Error(format!("{what} failed ({code})"))
    }

    /// The newest mono samples, written by the IOProc (real-time thread) without locks.
    struct Shared {
        ring: Box<[AtomicU32]>,
        written: AtomicUsize,
    }

    extern "C" fn io_proc(
        _device: u32,
        _now: *const c_void,
        input: *const AudioBufferList,
        _input_time: *const c_void,
        output: *mut AudioBufferList,
        _output_time: *const c_void,
        client: *mut c_void,
    ) -> i32 {
        // SAFETY: `client` is the Shared that Capture keeps alive until this IOProc is
        // destroyed; the buffer lists are valid for the duration of the call.
        unsafe {
            let shared = &*(client as *const Shared);
            if !input.is_null() {
                mix(shared, buffers(input));
            }
            if !output.is_null() {
                for b in buffers(output) {
                    if !b.data.is_null() {
                        std::ptr::write_bytes(b.data as *mut u8, 0, b.bytes as usize);
                    }
                }
            }
        }
        0
    }

    unsafe fn buffers<'a>(list: *const AudioBufferList) -> &'a [AudioBuffer] {
        std::slice::from_raw_parts(std::ptr::addr_of!((*list).buffers) as *const AudioBuffer, (*list).count as usize)
    }

    /// Averages every channel of every buffer (interleaved or not) into mono samples.
    unsafe fn mix(shared: &Shared, bufs: &[AudioBuffer]) {
        let mut frames = usize::MAX;
        let mut channels = 0usize;
        for b in bufs.iter().filter(|b| !b.data.is_null() && b.channels > 0) {
            frames = frames.min(b.bytes as usize / 4 / b.channels as usize);
            channels += b.channels as usize;
        }
        if channels == 0 || frames == usize::MAX {
            return;
        }
        let scale = 1.0 / channels as f32;
        let w = shared.written.load(Ordering::Relaxed);
        for i in 0..frames {
            let mut s = 0.0f32;
            for b in bufs.iter().filter(|b| !b.data.is_null() && b.channels > 0) {
                let ch = b.channels as usize;
                let d = b.data as *const f32;
                for c in 0..ch {
                    s += *d.add(i * ch + c);
                }
            }
            shared.ring[w.wrapping_add(i) & (RING - 1)].store((s * scale).to_bits(), Ordering::Relaxed);
        }
        shared.written.store(w.wrapping_add(frames), Ordering::Release);
    }

    pub struct Capture {
        tap: u32,
        device: u32,
        proc_id: IoProcId,
        shared: Box<Shared>,
        destroy_tap: DestroyTap,
        pub rate: f32,
    }

    // SAFETY: the IOProc id is an opaque handle used only from the owning (viz) thread.
    unsafe impl Send for Capture {}

    fn tap_api() -> Option<(CreateTap, DestroyTap)> {
        // Looked up at run time so the binary still loads (and just goes without) on an
        // older macOS.
        unsafe {
            let create = libc::dlsym(libc::RTLD_DEFAULT, c"AudioHardwareCreateProcessTap".as_ptr());
            let destroy = libc::dlsym(libc::RTLD_DEFAULT, c"AudioHardwareDestroyProcessTap".as_ptr());
            if create.is_null() || destroy.is_null() {
                return None;
            }
            Some((std::mem::transmute::<*mut c_void, CreateTap>(create), std::mem::transmute::<*mut c_void, DestroyTap>(destroy)))
        }
    }

    fn tap_format(tap: u32) -> Option<Asbd> {
        let addr = PropAddr { selector: TAP_FORMAT, scope: SCOPE_GLOBAL, element: 0 };
        let mut asbd = Asbd::default();
        let mut size = std::mem::size_of::<Asbd>() as u32;
        let ok = unsafe { AudioObjectGetPropertyData(tap, &addr, 0, std::ptr::null(), &mut size, &mut asbd as *mut Asbd as *mut c_void) == 0 };
        ok.then_some(asbd)
    }

    /// A global stereo tap of every process (none excluded), private to us, not muting
    /// anything. Returns its AudioObjectID and UUID string.
    fn create_tap(create: CreateTap) -> Result<(u32, String), Failure> {
        autoreleasepool(|_| unsafe {
            let Some(cls) = AnyClass::get("CATapDescription") else {
                return Err(Failure::Unavailable("CATapDescription isn't available (needs macOS 14.2+)".into()));
            };
            let empty: *mut AnyObject = msg_send![objc2::class!(NSArray), array];
            let alloc: *mut AnyObject = msg_send![cls, alloc];
            let desc: *mut AnyObject = msg_send![alloc, initStereoGlobalTapButExcludeProcesses: empty];
            if desc.is_null() {
                return Err(Failure::Error("CATapDescription init failed".into()));
            }
            let name = NSString::from_str("Quaked visualizer");
            let _: () = msg_send![desc, setName: &*name];
            let _: () = msg_send![desc, setPrivate: true];
            let _: () = msg_send![desc, setMuteBehavior: 0isize]; // CATapUnmuted
            let uuid: *mut AnyObject = msg_send![desc, UUID];
            let uuid_str: *mut AnyObject = if uuid.is_null() { std::ptr::null_mut() } else { msg_send![uuid, UUIDString] };
            let utf8: *const c_char = if uuid_str.is_null() { std::ptr::null() } else { msg_send![uuid_str, UTF8String] };
            let uid = if utf8.is_null() { String::new() } else { CStr::from_ptr(utf8).to_string_lossy().into_owned() };
            let mut tap = 0u32;
            let status = create(desc, &mut tap);
            objc2::ffi::objc_release(desc as *mut objc2::ffi::objc_object);
            if status != 0 || tap == 0 {
                return Err(err("AudioHardwareCreateProcessTap", status));
            }
            if uid.is_empty() {
                return Err(Failure::Error("the tap has no UUID".into()));
            }
            Ok((tap, uid))
        })
    }

    /// Owns the CF objects made while building a description, releasing them on drop.
    #[derive(Default)]
    struct Cf(Vec<*const c_void>);

    impl Cf {
        fn keep(&mut self, obj: *const c_void) -> *const c_void {
            if !obj.is_null() {
                self.0.push(obj);
            }
            obj
        }
        fn string(&mut self, s: &str) -> *const c_void {
            let c = CString::new(s).unwrap_or_default();
            self.keep(unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) })
        }
        fn number(&mut self, v: i32) -> *const c_void {
            self.keep(unsafe { CFNumberCreate(std::ptr::null(), NUMBER_SINT32, &v as *const i32 as *const c_void) })
        }
        fn dict(&mut self, pairs: &[(&str, *const c_void)]) -> *const c_void {
            unsafe {
                let d = CFDictionaryCreateMutable(std::ptr::null(), 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
                self.keep(d);
                for (k, v) in pairs {
                    let key = self.string(k);
                    CFDictionarySetValue(d, key, *v);
                }
                d
            }
        }
        fn array(&mut self, items: &[*const c_void]) -> *const c_void {
            self.keep(unsafe { CFArrayCreate(std::ptr::null(), items.as_ptr(), items.len() as isize, &kCFTypeArrayCallBacks) })
        }
    }

    impl Drop for Cf {
        fn drop(&mut self) {
            for obj in self.0.drain(..).rev() {
                unsafe { CFRelease(obj) };
            }
        }
    }

    /// A private aggregate device whose only input is the tap (started with it).
    fn create_aggregate(tap_uid: &str) -> Result<u32, Failure> {
        let mut cf = Cf::default();
        let sub_tap = {
            let uid = cf.string(tap_uid);
            let drift = cf.number(1);
            cf.dict(&[("uid", uid), ("drift", drift)]) // kAudioSubTapUIDKey, kAudioSubTapDriftCompensationKey
        };
        let taps = cf.array(&[sub_tap]);
        let uid = cf.string(&format!("dev.arlint.quaked.viz.{tap_uid}"));
        let name = cf.string("Quaked Visualizer");
        let (one, zero) = (cf.number(1), cf.number(0));
        let desc = cf.dict(&[
            ("uid", uid),           // kAudioAggregateDeviceUIDKey
            ("name", name),         // kAudioAggregateDeviceNameKey
            ("private", one),       // kAudioAggregateDeviceIsPrivateKey
            ("stacked", zero),      // kAudioAggregateDeviceIsStackedKey
            ("taps", taps),         // kAudioAggregateDeviceTapListKey
            ("tapautostart", one),  // kAudioAggregateDeviceTapAutoStartKey
        ]);
        let mut device = 0u32;
        let status = unsafe { AudioHardwareCreateAggregateDevice(desc, &mut device) };
        if status != 0 || device == 0 {
            return Err(err("AudioHardwareCreateAggregateDevice", status));
        }
        Ok(device)
    }

    impl Capture {
        pub fn start() -> Result<Capture, Failure> {
            let Some((create, destroy_tap)) = tap_api() else {
                return Err(Failure::Unavailable("Core Audio process taps aren't available (needs macOS 14.2+)".into()));
            };
            let (tap, tap_uid) = create_tap(create)?;
            let fail = |e: Failure, device: u32| {
                unsafe {
                    if device != 0 {
                        AudioHardwareDestroyAggregateDevice(device);
                    }
                    destroy_tap(tap);
                }
                Err(e)
            };
            let Some(fmt) = tap_format(tap) else { return fail(Failure::Error("couldn't read the tap's format".into()), 0) };
            if fmt.format != FORMAT_LPCM || fmt.flags & FLAG_FLOAT == 0 || fmt.bits != 32 {
                return fail(Failure::Error(format!("unexpected tap format (flags {:#x}, {} bits)", fmt.flags, fmt.bits)), 0);
            }
            let device = match create_aggregate(&tap_uid) {
                Ok(d) => d,
                Err(e) => return fail(e, 0),
            };
            let shared = Box::new(Shared { ring: (0..RING).map(|_| AtomicU32::new(0)).collect(), written: AtomicUsize::new(0) });
            let mut proc_id: IoProcId = std::ptr::null_mut();
            let client = &*shared as *const Shared as *mut c_void;
            let status = unsafe { AudioDeviceCreateIOProcID(device, io_proc, client, &mut proc_id) };
            if status != 0 || proc_id.is_null() {
                return fail(err("AudioDeviceCreateIOProcID", status), device);
            }
            let status = unsafe { AudioDeviceStart(device, proc_id) };
            if status != 0 {
                unsafe { AudioDeviceDestroyIOProcID(device, proc_id) };
                return fail(err("AudioDeviceStart", status), device);
            }
            let rate = if fmt.rate > 1000.0 { fmt.rate as f32 } else { 48_000.0 };
            Ok(Capture { tap, device, proc_id, shared, destroy_tap, rate })
        }

        /// The tap's current sample rate.
        pub fn rate(&self) -> Option<f32> {
            tap_format(self.tap).map(|f| f.rate as f32).filter(|r| *r > 1000.0)
        }

        /// Copies the newest `out.len()` samples into `out`; returns how many were ever written.
        pub fn latest(&self, out: &mut [f32]) -> usize {
            let w = self.shared.written.load(Ordering::Acquire);
            let n = out.len().min(RING / 2);
            let start = w.wrapping_sub(n);
            for (j, o) in out.iter_mut().take(n).enumerate() {
                *o = f32::from_bits(self.shared.ring[start.wrapping_add(j) & (RING - 1)].load(Ordering::Relaxed));
            }
            w
        }

        pub fn stop(self) {
            unsafe {
                let stopped = AudioDeviceStop(self.device, self.proc_id) == 0;
                let destroyed = AudioDeviceDestroyIOProcID(self.device, self.proc_id) == 0;
                AudioHardwareDestroyAggregateDevice(self.device);
                (self.destroy_tap)(self.tap);
                if !(stopped && destroyed) {
                    // The IOProc might still hold it: leak rather than free under it.
                    std::mem::forget(self.shared);
                }
            }
        }
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    //! DSP only, on synthetic signals: never creates a tap or touches audio devices.
    use super::*;

    const RATE: f32 = 48_000.0;

    fn sine(f: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| amp * (2.0 * PI * f * i as f32 / RATE).sin()).collect()
    }

    fn band_of(f: f32) -> usize {
        let e = band_edges(RATE);
        (0..BANDS).find(|&i| e[i] <= f && f < e[i + 1]).unwrap()
    }

    #[test]
    fn fft_matches_a_plain_dft() {
        let n = 64;
        let x: Vec<f32> = (0..n).map(|i| ((i * 7919) % 31) as f32 / 31.0 - 0.5).collect();
        let (mut re, mut im) = (x.clone(), vec![0.0; n]);
        Fft::new(n).run(&mut re, &mut im);
        for k in 0..n {
            let (mut r, mut m) = (0.0f32, 0.0f32);
            for (i, v) in x.iter().enumerate() {
                let a = -2.0 * PI * (k * i) as f32 / n as f32;
                r += v * a.cos();
                m += v * a.sin();
            }
            assert!((re[k] - r).abs() < 1e-3 && (im[k] - m).abs() < 1e-3, "bin {k}");
        }
    }

    #[test]
    fn band_layout_is_log_spaced_40_hz_to_16_khz() {
        let e = band_edges(RATE);
        assert_eq!(e.len(), BANDS + 1);
        assert!((e[0] - 40.0).abs() < 1e-3);
        assert!((e[BANDS] - 16_000.0).abs() < 1.0);
        let ratio = e[1] / e[0];
        for w in e.windows(2) {
            assert!((w[1] / w[0] - ratio).abs() < 1e-3);
        }
        // Low sample rates stay under Nyquist.
        assert!(*band_edges(22_050.0).last().unwrap() < 11_025.0);
    }

    #[test]
    fn a_440_hz_sine_lands_in_its_band() {
        let mut an = Analyzer::new(RATE);
        an.analyse(&sine(440.0, 0.5, N_LONG));
        let best = (0..BANDS).max_by(|&a, &b| an.raw[a].total_cmp(&an.raw[b])).unwrap();
        assert_eq!(best, band_of(440.0));
        assert!(an.raw[best] > 0.8, "a loud sine is near the top: {}", an.raw[best]);
        // Well away from it: nothing.
        assert!(an.raw[band_of(100.0)] < 0.05 && an.raw[band_of(4000.0)] < 0.05);
    }

    #[test]
    fn sines_across_the_range_land_in_their_bands() {
        for f in [60.0, 150.0, 1000.0, 3000.0, 9000.0] {
            let mut an = Analyzer::new(RATE);
            an.analyse(&sine(f, 0.3, N_LONG));
            let best = (0..BANDS).max_by(|&a, &b| an.raw[a].total_cmp(&an.raw[b])).unwrap();
            assert!(best.abs_diff(band_of(f)) <= 1, "{f} Hz -> band {best}, want {}", band_of(f));
        }
    }

    #[test]
    fn silence_gives_zeros() {
        let mut an = Analyzer::new(RATE);
        let zeros = vec![0.0; N_LONG];
        for _ in 0..30 {
            assert!(!an.step(&zeros, 0.033));
        }
        let st = an.status();
        assert!(st.bands.iter().chain(&st.peaks).all(|&v| v == 0.0));
        assert_eq!((st.level, st.beats, st.pulsing), (0.0, 0, false));
    }

    #[test]
    fn bands_attack_fast_and_decay_slowly_to_zero() {
        let mut an = Analyzer::new(RATE);
        let tone = sine(1000.0, 0.5, N_LONG);
        let b = band_of(1000.0);
        an.step(&tone, 0.033);
        assert!(an.bands[b] > 0.6, "attack: {}", an.bands[b]);
        for _ in 0..10 {
            an.step(&tone, 0.033);
        }
        let full = an.bands[b];
        an.silence(0.033);
        assert!(an.bands[b] > full * 0.7, "decay is gradual");
        assert!(an.peaks[b] >= full - 1e-6, "peak holds");
        for _ in 0..90 {
            an.silence(0.033);
        }
        assert_eq!((an.bands[b], an.peaks[b]), (0.0, 0.0), "settles to exactly zero");
    }

    /// Runs `signal` through the analyser in ~30 Hz frames; returns beat times (s).
    fn beats_in(signal: &[f32]) -> Vec<f32> {
        let mut an = Analyzer::new(RATE);
        let hop = 1600; // 33.3 ms
        let mut out = Vec::new();
        let mut end = N_LONG;
        while end <= signal.len() {
            if an.step(&signal[end - N_LONG..end], hop as f32 / RATE) {
                out.push(end as f32 / RATE);
            }
            end += hop;
        }
        out
    }

    #[test]
    fn beats_follow_a_kick_pattern() {
        // 120 BPM kicks over a steady pad and hats.
        let secs = 8.0;
        let n = (secs * RATE) as usize;
        let mut s = vec![0.0f32; n];
        let mut noise = 12345u32;
        for (i, v) in s.iter_mut().enumerate() {
            let t = i as f32 / RATE;
            let tk = t % 0.5;
            let kick = 0.8 * (2.0 * PI * 55.0 * tk).sin() * (-tk / 0.08).exp();
            let pad = 0.05 * (2.0 * PI * 220.0 * t).sin() + 0.04 * (2.0 * PI * 330.0 * t).sin();
            noise ^= noise << 13;
            noise ^= noise >> 17;
            noise ^= noise << 5;
            let hat = 0.05 * (noise as f32 / u32::MAX as f32 - 0.5) * (-(t % 0.25) / 0.03).exp();
            *v = kick + pad + hat;
        }
        let beats = beats_in(&s);
        let kicks: Vec<f32> = (0..16).map(|k| k as f32 * 0.5).filter(|&t| t >= N_LONG as f32 / RATE).collect();
        assert!(beats.len().abs_diff(kicks.len()) <= 1, "{} beats for {} kicks: {beats:?}", beats.len(), kicks.len());
        for b in &beats {
            let lag = b % 0.5;
            assert!(lag < 0.08, "beat at {b:.3}s is {lag:.3}s after its kick");
        }
    }

    #[test]
    fn no_beats_in_a_steady_tone_or_silence() {
        let n = (5.0 * RATE) as usize;
        let tone: Vec<f32> = (0..n).map(|i| 0.5 * (2.0 * PI * 80.0 * i as f32 / RATE).sin()).collect();
        assert!(beats_in(&tone).len() <= 1, "at most the tone's start");
        assert!(beats_in(&vec![0.0; n]).is_empty());
    }

    #[test]
    fn the_ring_pulse_changes_at_most_three_times_per_beat() {
        let mut b = Beat::default();
        let dt = 1.0 / 30.0;
        let mut changes = 0;
        let mut last = b.pulse();
        let frames = 300; // 10 s
        for f in 0..frames {
            // A kick every 15 frames (120 BPM): loud, then quiet.
            let e = if f % 15 == 0 { -10.0 } else { -40.0 };
            b.step(e, dt);
            let p = b.pulse();
            if p != last {
                changes += 1;
                last = p;
            }
        }
        assert!(b.count >= 19, "beats: {}", b.count);
        assert!(changes as f32 / 10.0 <= 7.5, "{changes} ring changes in 10 s");
        // No beats for a while: stops pulsing, rests at 0.
        for _ in 0..120 {
            b.step(-40.0, dt);
        }
        assert!(!b.pulsing() && b.pulse() == 0.0);
    }

    #[test]
    fn the_preview_sample_looks_like_music() {
        let st = sample();
        assert_eq!(st.bands.len(), BANDS);
        assert_eq!(st.peaks.len(), BANDS);
        assert!(st.bands.iter().all(|v| (0.0..=1.0).contains(v)));
        assert!(st.bands.iter().filter(|&&v| v > 0.2).count() > BANDS / 2, "{:?}", st.bands);
        assert!(st.peaks.iter().zip(&st.bands).all(|(p, b)| p >= b));
        assert!(st.beats >= 6 && st.pulsing && st.beat > 0.5, "beats {} glow {}", st.beats, st.beat);
    }
}
