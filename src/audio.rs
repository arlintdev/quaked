//! Soundboard: plays clips into a virtual mic (BlackHole) mixed with your real mic, so
//! people on a call hear them, and on your own output so you hear them too.
//!
//! Routing goes live only while another app is capturing from the output device (your
//! call app with "BlackHole 2ch" picked as its microphone), so your mic isn't held open
//! all day. `live` = "on" / "off" overrides that.

use crate::config::{self, Soundboard};
use crate::device::log;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

const AUDIO_EXT: [&str; 9] = ["wav", "mp3", "m4a", "aac", "aiff", "aif", "flac", "caf", "mp4"];
const MAX_CLIP_SECS: f32 = 120.0;
const MIC_MAX_MS: usize = 60; // drop mic audio older than this to keep latency low
const LIVE_GRACE: Duration = Duration::from_secs(3);
const MONITOR_IDLE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq)]
pub struct ClipInfo {
    pub title: String,
    pub path: PathBuf,
    pub color: String,
    pub symbol: String,
    pub gain: f32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub clips: Vec<ClipInfo>,
    /// Indices into `clips` that are playing.
    pub playing: Vec<usize>,
    /// Mic + clips are going to the virtual mic.
    pub live: bool,
    /// Bundle id of the app capturing from the virtual mic, if any.
    pub listener: Option<String>,
    pub mic_muted: bool,
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum Cmd {
    Play(usize),
    Stop(usize),
    PlayTitle(String),
    StopAll,
    ToggleMute,
    Configure(Soundboard),
    Devices(Sender<(Vec<String>, Vec<String>)>),
}

// ---------------------------------------------------------------- mixing

struct Voice {
    clip: usize,
    data: Arc<Vec<f32>>, // stereo interleaved at the stream's rate
    pos: usize,
    gain: f32,
}

#[derive(Default)]
struct Mixer {
    voices: Vec<Voice>,
    mic: VecDeque<f32>,
    mic_on: bool,
    mic_max: usize,
}

impl Mixer {
    fn render(&mut self, out: &mut [f32], channels: usize) {
        let frames = out.len() / channels.max(1);
        if !self.mic_on {
            self.mic.clear();
        } else if self.mic.len() > self.mic_max + frames {
            let excess = self.mic.len() - (self.mic_max + frames);
            self.mic.drain(..excess);
        }
        for f in 0..frames {
            let m = if self.mic_on { self.mic.pop_front().unwrap_or(0.0) } else { 0.0 };
            let (mut l, mut r) = (m, m);
            for v in self.voices.iter_mut() {
                if v.pos + 1 < v.data.len() {
                    l += v.data[v.pos] * v.gain;
                    r += v.data[v.pos + 1] * v.gain;
                    v.pos += 2;
                }
            }
            let (l, r) = (soft_clip(l), soft_clip(r));
            let frame = &mut out[f * channels..(f + 1) * channels];
            match channels {
                1 => frame[0] = (l + r) * 0.5,
                _ => {
                    frame[0] = l;
                    frame[1] = r;
                    for x in frame.iter_mut().skip(2) {
                        *x = 0.0;
                    }
                }
            }
        }
        self.voices.retain(|v| v.pos + 1 < v.data.len());
    }
}

fn soft_clip(x: f32) -> f32 {
    if x.abs() <= 0.9 { x } else { x.signum() * (0.9 + 0.1 * ((x.abs() - 0.9) * 10.0).tanh()) }
}

struct Output {
    _stream: cpal::Stream,
    mixer: Arc<Mutex<Mixer>>,
    rate: u32,
}

fn find_device(host: &cpal::Host, name: &str, input: bool) -> Option<cpal::Device> {
    if name.is_empty() {
        return if input { host.default_input_device() } else { host.default_output_device() };
    }
    let mut devices = if input { host.input_devices().ok()? } else { host.output_devices().ok()? };
    devices.find(|d| d.name().map(|n| n == name).unwrap_or(false))
}

fn open_output(host: &cpal::Host, name: &str, mic_on: bool) -> Result<Output, String> {
    let dev = find_device(host, name, false).ok_or_else(|| format!("output device \"{name}\" not found"))?;
    let cfg = dev.default_output_config().map_err(|e| e.to_string())?;
    let rate = cfg.sample_rate().0;
    let channels = cfg.channels() as usize;
    let mixer = Arc::new(Mutex::new(Mixer { mic_on, mic_max: rate as usize * MIC_MAX_MS / 1000, ..Default::default() }));
    let m = mixer.clone();
    let err = |e| log(format!("audio: output stream error: {e}"));
    let stream = match cfg.sample_format() {
        cpal::SampleFormat::F32 => dev.build_output_stream(
            &cfg.config(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| match m.try_lock() {
                Ok(mut mx) => mx.render(data, channels),
                Err(_) => data.fill(0.0),
            },
            err,
            None,
        ),
        cpal::SampleFormat::I16 => {
            let mut scratch = Vec::new();
            dev.build_output_stream(
                &cfg.config(),
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    scratch.resize(data.len(), 0.0f32);
                    match m.try_lock() {
                        Ok(mut mx) => mx.render(&mut scratch, channels),
                        Err(_) => scratch.fill(0.0),
                    }
                    for (d, s) in data.iter_mut().zip(&scratch) {
                        *d = (s * i16::MAX as f32) as i16;
                    }
                },
                err,
                None,
            )
        }
        f => return Err(format!("unsupported output sample format {f:?}")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Output { _stream: stream, mixer, rate })
}

/// Opens the mic and feeds it (mono, resampled to `out_rate`) into `mixer`.
fn open_mic(host: &cpal::Host, name: &str, mixer: Arc<Mutex<Mixer>>, out_rate: u32) -> Result<cpal::Stream, String> {
    let dev = find_device(host, name, true).ok_or_else(|| format!("microphone \"{name}\" not found"))?;
    let cfg = dev.default_input_config().map_err(|e| e.to_string())?;
    let channels = cfg.channels() as usize;
    let ratio = cfg.sample_rate().0 as f64 / out_rate as f64;
    let mut rs = Resampler { ratio, phase: 0.0, prev: 0.0 };
    let mut push = move |mono: &mut dyn Iterator<Item = f32>| {
        let mut out = Vec::with_capacity(512);
        for s in mono {
            rs.push(s, &mut out);
        }
        if let Ok(mut m) = mixer.lock() {
            m.mic.extend(out);
        }
    };
    let err = |e| log(format!("audio: mic stream error: {e}"));
    let stream = match cfg.sample_format() {
        cpal::SampleFormat::F32 => dev.build_input_stream(
            &cfg.config(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                push(&mut data.chunks(channels).map(|f| f.iter().sum::<f32>() / f.len() as f32));
            },
            err,
            None,
        ),
        cpal::SampleFormat::I16 => dev.build_input_stream(
            &cfg.config(),
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                push(&mut data.chunks(channels).map(|f| f.iter().map(|&x| x as f32 / i16::MAX as f32).sum::<f32>() / f.len() as f32));
            },
            err,
            None,
        ),
        f => return Err(format!("unsupported mic sample format {f:?}")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

/// Streaming linear resampler (input rate / output rate = ratio).
struct Resampler {
    ratio: f64,
    phase: f64,
    prev: f32,
}

impl Resampler {
    fn push(&mut self, s: f32, out: &mut Vec<f32>) {
        while self.phase <= 1.0 {
            out.push(self.prev + (s - self.prev) * self.phase as f32);
            self.phase += self.ratio;
        }
        self.phase -= 1.0;
        self.prev = s;
    }
}

// ---------------------------------------------------------------- clips

fn scan_clips(cfg: &Soundboard) -> Vec<ClipInfo> {
    let folder = config::expand(&cfg.folder);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&folder)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| AUDIO_EXT.contains(&e.to_lowercase().as_str())))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    let stem = |p: &Path| p.file_stem().and_then(|s| s.to_str()).unwrap_or("clip").to_string();
    let mut out = Vec::new();
    for c in &cfg.clips {
        let p = if c.file.starts_with('/') || c.file.starts_with('~') { config::expand(&c.file) } else { folder.join(&c.file) };
        if !p.exists() {
            continue;
        }
        files.retain(|f| f != &p);
        out.push(ClipInfo {
            title: if c.title.is_empty() { stem(&p) } else { c.title.clone() },
            color: c.color.clone(),
            symbol: c.symbol.clone(),
            gain: c.gain,
            path: p,
        });
    }
    for f in files {
        out.push(ClipInfo { title: stem(&f), path: f, color: String::new(), symbol: String::new(), gain: 1.0 });
    }
    out
}

/// Decodes an audio file to stereo interleaved f32 at `rate`.
fn decode(path: &Path, rate: u32) -> Result<Vec<f32>, String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| e.to_string())?;
    let mut format = probed.format;
    let track = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL).ok_or("no audio track")?.clone();
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(|e| e.to_string())?;
    let src_rate = track.codec_params.sample_rate.unwrap_or(48_000);
    let max_frames = (MAX_CLIP_SECS * src_rate as f32) as usize;
    let mut stereo: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(_)) | Err(Error::ResetRequired) => break,
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id() != track.id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(e.to_string()),
        };
        let spec = *buf.spec();
        let ch = spec.channels.count().max(1);
        let mut sb = SampleBuffer::<f32>::new(buf.capacity() as u64, spec);
        sb.copy_interleaved_ref(buf);
        for frame in sb.samples().chunks(ch) {
            let l = frame[0];
            let r = if ch > 1 { frame[1] } else { l };
            stereo.push(l);
            stereo.push(r);
        }
        if stereo.len() / 2 > max_frames {
            break;
        }
    }
    if src_rate == rate {
        return Ok(stereo);
    }
    // Linear resample, per channel.
    let frames = stereo.len() / 2;
    let out_frames = (frames as f64 * rate as f64 / src_rate as f64) as usize;
    let mut out = Vec::with_capacity(out_frames * 2);
    for i in 0..out_frames {
        let t = i as f64 * src_rate as f64 / rate as f64;
        let i0 = t.floor() as usize;
        let i1 = (i0 + 1).min(frames.saturating_sub(1));
        let k = (t - i0 as f64) as f32;
        for c in 0..2 {
            let a = stereo[i0 * 2 + c];
            let b = stereo[i1 * 2 + c];
            out.push(a + (b - a) * k);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- who is listening (CoreAudio)

#[repr(C)]
struct PropAddr {
    selector: u32,
    scope: u32,
    element: u32,
}

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyDataSize(id: u32, addr: *const PropAddr, qsize: u32, qdata: *const c_void, size: *mut u32) -> i32;
    fn AudioObjectGetPropertyData(id: u32, addr: *const PropAddr, qsize: u32, qdata: *const c_void, size: *mut u32, out: *mut c_void) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringGetCString(s: *const c_void, buf: *mut u8, len: isize, encoding: u32) -> u8;
    fn CFRelease(cf: *const c_void);
}

const fn fourcc(s: &[u8; 4]) -> u32 {
    (s[0] as u32) << 24 | (s[1] as u32) << 16 | (s[2] as u32) << 8 | s[3] as u32
}
const SYSTEM_OBJECT: u32 = 1;
const SCOPE_GLOBAL: u32 = fourcc(b"glob");
const SCOPE_INPUT: u32 = fourcc(b"inpt");

fn prop_vec(id: u32, selector: &[u8; 4], scope: u32) -> Vec<u32> {
    let addr = PropAddr { selector: fourcc(selector), scope, element: 0 };
    let mut size = 0u32;
    unsafe {
        if AudioObjectGetPropertyDataSize(id, &addr, 0, std::ptr::null(), &mut size) != 0 || size == 0 {
            return Vec::new();
        }
        let mut v = vec![0u32; size as usize / 4];
        if AudioObjectGetPropertyData(id, &addr, 0, std::ptr::null(), &mut size, v.as_mut_ptr() as *mut c_void) != 0 {
            return Vec::new();
        }
        v.truncate(size as usize / 4);
        v
    }
}

fn prop_u32(id: u32, selector: &[u8; 4]) -> Option<u32> {
    let addr = PropAddr { selector: fourcc(selector), scope: SCOPE_GLOBAL, element: 0 };
    let mut v = 0u32;
    let mut size = 4u32;
    let ok = unsafe { AudioObjectGetPropertyData(id, &addr, 0, std::ptr::null(), &mut size, &mut v as *mut u32 as *mut c_void) == 0 };
    ok.then_some(v)
}

fn prop_string(id: u32, selector: &[u8; 4]) -> Option<String> {
    let addr = PropAddr { selector: fourcc(selector), scope: SCOPE_GLOBAL, element: 0 };
    let mut cf: *const c_void = std::ptr::null();
    let mut size = std::mem::size_of::<*const c_void>() as u32;
    unsafe {
        if AudioObjectGetPropertyData(id, &addr, 0, std::ptr::null(), &mut size, &mut cf as *mut _ as *mut c_void) != 0 || cf.is_null() {
            return None;
        }
        let mut buf = [0u8; 256];
        let ok = CFStringGetCString(cf, buf.as_mut_ptr(), buf.len() as isize, 0x0800_0100);
        CFRelease(cf);
        if ok == 0 {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..end]).into_owned())
    }
}

/// The app (bundle id) capturing audio from the device called `name`, other than us.
fn listener_of(name: &str) -> Option<String> {
    let device = prop_vec(SYSTEM_OBJECT, b"dev#", SCOPE_GLOBAL).into_iter().find(|&d| prop_string(d, b"lnam").as_deref() == Some(name))?;
    let me = std::process::id();
    for p in prop_vec(SYSTEM_OBJECT, b"prs#", SCOPE_GLOBAL) {
        if prop_u32(p, b"ppid") == Some(me) || prop_u32(p, b"piri") != Some(1) {
            continue;
        }
        if prop_vec(p, b"pdv#", SCOPE_INPUT).contains(&device) {
            return Some(prop_string(p, b"pbid").unwrap_or_else(|| format!("pid {}", prop_u32(p, b"ppid").unwrap_or(0))));
        }
    }
    None
}

// ---------------------------------------------------------------- engine thread

pub struct Audio {
    pub cmd: Sender<Cmd>,
    pub status: Arc<Mutex<Status>>,
}

pub fn spawn(cfg: Soundboard, notify: impl Fn() + Send + 'static) -> Audio {
    let (tx, rx) = std::sync::mpsc::channel();
    let status = Arc::new(Mutex::new(Status::default()));
    let st = status.clone();
    std::thread::Builder::new().name("audio".into()).spawn(move || Engine::new(cfg, st, notify).run(rx)).unwrap();
    Audio { cmd: tx, status }
}

struct Engine<N: Fn()> {
    cfg: Soundboard,
    host: cpal::Host,
    status: Arc<Mutex<Status>>,
    notify: N,
    clips: Vec<ClipInfo>,
    folder_mtime: Option<SystemTime>,
    decoded: HashMap<(PathBuf, u32), Arc<Vec<f32>>>,
    call: Option<Output>,
    mic: Option<cpal::Stream>,
    monitor: Option<Output>,
    monitor_used: Instant,
    live: bool,
    listener: Option<String>,
    last_listener_seen: Instant,
    mic_muted: bool,
    error: Option<String>,
    published: Status,
}

impl<N: Fn()> Engine<N> {
    fn new(cfg: Soundboard, status: Arc<Mutex<Status>>, notify: N) -> Self {
        Engine {
            cfg,
            host: cpal::default_host(),
            status,
            notify,
            clips: Vec::new(),
            folder_mtime: None,
            decoded: HashMap::new(),
            call: None,
            mic: None,
            monitor: None,
            monitor_used: Instant::now(),
            live: false,
            listener: None,
            last_listener_seen: Instant::now() - LIVE_GRACE * 2,
            mic_muted: false,
            error: None,
            published: Status { clips: vec![], playing: vec![], live: true, listener: None, mic_muted: false, error: Some(String::new()) },
        }
    }

    fn run(mut self, rx: Receiver<Cmd>) {
        let _ = std::fs::create_dir_all(config::expand(&self.cfg.folder));
        let mut next_scan = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(150)) {
                Ok(cmd) => self.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if Instant::now() >= next_scan {
                self.rescan();
                self.reconcile();
                next_scan = Instant::now() + Duration::from_secs(1);
            }
            self.publish();
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Play(i) => self.play(i),
            Cmd::PlayTitle(t) => {
                if let Some(i) = self.clips.iter().position(|c| c.title.eq_ignore_ascii_case(&t)) {
                    self.play(i);
                }
            }
            Cmd::Stop(i) => {
                for out in [&self.call, &self.monitor].into_iter().flatten() {
                    if let Ok(mut m) = out.mixer.lock() {
                        m.voices.retain(|v| v.clip != i);
                    }
                }
            }
            Cmd::StopAll => {
                for out in [&self.call, &self.monitor].into_iter().flatten() {
                    if let Ok(mut m) = out.mixer.lock() {
                        m.voices.clear();
                    }
                }
            }
            Cmd::ToggleMute => {
                self.mic_muted = !self.mic_muted;
                self.apply_mic_flag();
            }
            Cmd::Configure(cfg) => {
                let reopen = cfg.output != self.cfg.output || cfg.input != self.cfg.input || cfg.monitor_device != self.cfg.monitor_device;
                let rescan = cfg.folder != self.cfg.folder || cfg.clips != self.cfg.clips;
                self.cfg = cfg;
                if reopen {
                    self.call = None;
                    self.mic = None;
                    self.monitor = None;
                }
                if rescan {
                    self.folder_mtime = None;
                    let _ = std::fs::create_dir_all(config::expand(&self.cfg.folder));
                }
                self.rescan();
                self.reconcile();
            }
            Cmd::Devices(reply) => {
                let names = |it: Result<_, _>| -> Vec<String> {
                    let it: Box<dyn Iterator<Item = cpal::Device>> = match it {
                        Ok(i) => i,
                        Err(_) => return Vec::new(),
                    };
                    it.filter_map(|d| d.name().ok()).collect()
                };
                let inputs = names(self.host.input_devices().map(|i| Box::new(i) as Box<dyn Iterator<Item = cpal::Device>>));
                let outputs = names(self.host.output_devices().map(|i| Box::new(i) as Box<dyn Iterator<Item = cpal::Device>>));
                let _ = reply.send((inputs, outputs));
            }
        }
    }

    fn apply_mic_flag(&self) {
        if let Some(call) = &self.call {
            if let Ok(mut m) = call.mixer.lock() {
                m.mic_on = self.cfg.passthrough && !self.mic_muted && self.mic.is_some();
            }
        }
    }

    fn rescan(&mut self) {
        let folder = config::expand(&self.cfg.folder);
        let mtime = std::fs::metadata(&folder).and_then(|m| m.modified()).ok();
        if mtime == self.folder_mtime && !self.clips.is_empty() {
            return;
        }
        self.folder_mtime = mtime;
        let clips = scan_clips(&self.cfg);
        if clips != self.clips {
            log(format!("audio: {} clips in {}", clips.len(), folder.display()));
            self.clips = clips;
        }
    }

    /// Opens or closes the call route and the monitor as the situation requires.
    fn reconcile(&mut self) {
        let listener = if self.cfg.live == "auto" { listener_of(&self.cfg.output) } else { None };
        if listener.is_some() {
            self.last_listener_seen = Instant::now();
        }
        let want_live = match self.cfg.live.as_str() {
            "on" => true,
            "off" => false,
            _ => listener.is_some() || (self.live && self.last_listener_seen.elapsed() < LIVE_GRACE),
        };
        if listener.is_some() || self.cfg.live != "auto" {
            self.listener = listener;
        }
        if want_live && self.call.is_none() {
            match open_output(&self.host, &self.cfg.output, false) {
                Ok(out) => {
                    let rate = out.rate;
                    let mixer = out.mixer.clone();
                    self.call = Some(out);
                    self.error = None;
                    if self.cfg.passthrough {
                        match open_mic(&self.host, &self.cfg.input, mixer, rate) {
                            Ok(s) => self.mic = Some(s),
                            Err(e) => self.error = Some(format!("Mic: {e}")),
                        }
                    }
                    log(format!("audio: live on {} ({})", self.cfg.output, self.listener.as_deref().unwrap_or(&self.cfg.live)));
                }
                Err(e) => self.error = Some(e),
            }
        } else if !want_live && self.call.is_some() {
            self.mic = None;
            self.call = None;
            self.listener = None;
            log("audio: off the call route");
        } else if want_live && self.cfg.passthrough != self.mic.is_some() {
            // passthrough toggled while live
            self.mic = None;
            if self.cfg.passthrough {
                if let Some(call) = &self.call {
                    match open_mic(&self.host, &self.cfg.input, call.mixer.clone(), call.rate) {
                        Ok(s) => self.mic = Some(s),
                        Err(e) => self.error = Some(format!("Mic: {e}")),
                    }
                }
            }
        }
        self.live = self.call.is_some();
        self.apply_mic_flag();
        if self.monitor.is_some() && self.monitor_used.elapsed() > MONITOR_IDLE && self.voices_in(&self.monitor) == 0 {
            self.monitor = None;
        }
    }

    fn voices_in(&self, out: &Option<Output>) -> usize {
        out.as_ref().and_then(|o| o.mixer.lock().ok().map(|m| m.voices.len())).unwrap_or(0)
    }

    fn clip_data(&mut self, i: usize, rate: u32) -> Option<Arc<Vec<f32>>> {
        let path = self.clips.get(i)?.path.clone();
        if let Some(d) = self.decoded.get(&(path.clone(), rate)) {
            return Some(d.clone());
        }
        match decode(&path, rate) {
            Ok(d) => {
                let d = Arc::new(d);
                self.decoded.insert((path, rate), d.clone());
                Some(d)
            }
            Err(e) => {
                self.error = Some(format!("{}: {e}", path.display()));
                None
            }
        }
    }

    fn play(&mut self, i: usize) {
        let Some(clip) = self.clips.get(i).cloned() else { return };
        let gain = (self.cfg.gain * clip.gain).clamp(0.0, 2.0);
        if self.cfg.monitor && self.monitor.is_none() {
            match open_output(&self.host, &self.cfg.monitor_device, false) {
                Ok(o) => self.monitor = Some(o),
                Err(e) => self.error = Some(format!("Monitor: {e}")),
            }
        }
        let targets: Vec<(Arc<Mutex<Mixer>>, u32)> = [&self.call, if self.cfg.monitor { &self.monitor } else { &None }]
            .into_iter()
            .flatten()
            .map(|o| (o.mixer.clone(), o.rate))
            .collect();
        for (mixer, rate) in targets {
            if let Some(data) = self.clip_data(i, rate) {
                if let Ok(mut m) = mixer.lock() {
                    m.voices.retain(|v| v.clip != i); // replay restarts the clip
                    m.voices.push(Voice { clip: i, data, pos: 0, gain });
                }
            }
        }
        self.monitor_used = Instant::now();
        log(format!("audio: play \"{}\"{}", clip.title, if self.live { " (live)" } else { "" }));
    }

    fn publish(&mut self) {
        let mut playing: Vec<usize> = Vec::new();
        for out in [&self.call, &self.monitor].into_iter().flatten() {
            if let Ok(m) = out.mixer.lock() {
                playing.extend(m.voices.iter().map(|v| v.clip));
            }
        }
        playing.sort_unstable();
        playing.dedup();
        let s = Status {
            clips: self.clips.clone(),
            playing,
            live: self.live,
            listener: self.listener.clone(),
            mic_muted: self.mic_muted,
            error: self.error.clone(),
        };
        if s != self.published {
            if let Ok(mut st) = self.status.lock() {
                *st = s.clone();
            }
            self.published = s;
            (self.notify)();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_keeps_rate() {
        let mut r = Resampler { ratio: 1.0, phase: 0.0, prev: 0.0 };
        let mut out = Vec::new();
        for i in 0..100 {
            r.push(i as f32, &mut out);
        }
        assert!((100..=101).contains(&out.len()));
        let mut r = Resampler { ratio: 0.5, phase: 0.0, prev: 0.0 }; // upsample 2x
        let mut out = Vec::new();
        for i in 0..100 {
            r.push(i as f32, &mut out);
        }
        assert!((199..=202).contains(&out.len()));
    }

    #[test]
    fn decodes_and_resamples_wav() {
        let dir = std::env::temp_dir().join(format!("quaked-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav");
        let rate = 44_100u32;
        let mut pcm = Vec::new();
        for i in 0..rate / 2 {
            let s = ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 12_000.0) as i16;
            pcm.extend_from_slice(&s.to_le_bytes());
        }
        let mut wav = Vec::new();
        wav.extend(b"RIFF");
        wav.extend((36 + pcm.len() as u32).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes()); // PCM
        wav.extend(1u16.to_le_bytes()); // mono
        wav.extend(rate.to_le_bytes());
        wav.extend((rate * 2).to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend((pcm.len() as u32).to_le_bytes());
        wav.extend(&pcm);
        std::fs::write(&path, wav).unwrap();
        let out = decode(&path, 48_000).unwrap();
        let frames = out.len() / 2;
        assert!((frames as i64 - 24_000).abs() < 50, "frames {frames}");
        assert!(out.iter().any(|s| s.abs() > 0.3));
        assert_eq!(out[100], out[101], "mono should be copied to both channels");
        let cfg = Soundboard { folder: dir.to_string_lossy().into_owned(), ..Default::default() };
        let clips = scan_clips(&cfg);
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].title, "tone");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn mixer_sums_and_retires_voices() {
        let mut m = Mixer { mic_on: true, mic_max: 1000, ..Default::default() };
        m.mic.extend([0.1, 0.1]);
        m.voices.push(Voice { clip: 0, data: Arc::new(vec![0.2, 0.3]), pos: 0, gain: 1.0 });
        let mut out = vec![0.0; 4];
        m.render(&mut out, 2);
        assert!((out[0] - 0.3).abs() < 1e-6 && (out[1] - 0.4).abs() < 1e-6);
        assert!((out[2] - 0.1).abs() < 1e-6);
        assert!(m.voices.is_empty());
    }
}
