//! Call mode on the board (shown over every page while a call is on): a huge LIVE /
//! MUTED card that the knob's press toggles, the call app and how long it's been, the
//! next meeting, and the soundboard's first clips as big buttons.
use super::*;
use crate::call::{self as call_mode, Mute};

const CALL_RED: u32 = 0xFF3B30;
const LIVE_BG: u32 = 0xB3261E;
const STATE_W: f32 = 610.0;
const STATE: R = (M, M, STATE_W, H - 2.0 * M);
const RX: f32 = M + STATE_W + G;
const RW: f32 = W - M - RX;
const HIDE_W: f32 = 132.0;
const TOP_H: f32 = 112.0;
const EVENT: R = (RX, M, RW - HIDE_W - G, TOP_H);
const HIDE: R = (W - M - HIDE_W, M, HIDE_W, TOP_H);
const STRIP_Y: f32 = M + TOP_H + G;
const GRID_Y: f32 = STRIP_Y + 34.0;
const COLS: usize = 4;
const ROWS: usize = 2;

fn clip_rect(i: usize) -> R {
    let w = (RW - (COLS as f32 - 1.0) * G) / COLS as f32;
    let h = (H - M - GRID_Y - (ROWS as f32 - 1.0) * G) / ROWS as f32;
    let (col, row) = (i % COLS, i / COLS);
    (RX + col as f32 * (w + G), GRID_Y + row as f32 * (h + G), w, h)
}

fn clips(v: &View) -> usize {
    v.sounds.clips.len().min(call_mode::CLIPS).min(COLS * ROWS)
}

/// What the big card says: (word, symbol, background, word color, what a press does).
fn state(v: &View) -> (&'static str, &'static str, u32, u32, String) {
    let k = v.call;
    match &k.mute {
        Mute::Soundboard if !k.passthrough => ("MIC OFF", "mic.slash.fill", 0x1C1C21, RED, "your mic isn't mixed in: soundboard passthrough is off".into()),
        Mute::Soundboard if v.sounds.mic_muted => ("MUTED", "mic.slash.fill", 0x1C1C21, RED, "press to unmute".into()),
        Mute::Soundboard => ("LIVE", "mic.fill", LIVE_BG, 0xFFFFFF, "press to mute".into()),
        Mute::Keys(combo) => ("ON CALL", "phone.fill", 0x1C1C21, CALL_RED, format!("press sends {} to {}", call_mode::keys_label(combo), k.app)),
        Mute::App => ("ON CALL", "phone.fill", 0x1C1C21, CALL_RED, format!("mute in {}  ·  press brings it forward", k.app)),
    }
}

pub(super) fn draw(c: &mut Canvas, v: &View) {
    let k = v.call;
    let (word, sym, bg, fg, press) = state(v);
    let live = word == "LIVE";
    let (x, y, w, h) = STATE;
    // The big card.
    c.rrect(STATE, 20.0, bg, 1.0);
    if !live {
        c.outline(STATE, 20.0, if word == "MUTED" { 0x5A2020 } else { 0x3A1A1A }, 2.0);
    }
    let sub = if live { 0xFFD9D6 } else { TEXT_2 };
    c.symbol("phone.fill", 18.0, x + 40.0, y + 44.0, if live { 0xFFFFFF } else { CALL_RED });
    let aw = c.text(F::Bold, &k.app.to_uppercase(), 18.0, x + 62.0, y + 51.0, if live { 0xFFFFFF } else { TEXT }, Align::Left);
    if !k.meeting.is_empty() {
        c.clip(F::Medium, &format!("·   {}", k.meeting), 18.0, w - aw - 110.0, x + 76.0 + aw, y + 51.0, sub);
    }
    c.symbol(sym, 64.0, x + 68.0, y + 140.0, fg);
    c.fit(F::Bold, word, 150.0, w - 70.0, x + 34.0, y + 300.0, fg, Align::Left);
    let note = k.note.as_ref().map(|(t, _)| t.clone());
    let line = note.clone().unwrap_or(press);
    c.fit(F::Medium, &line, 24.0, w - 70.0, x + 40.0, y + 346.0, if note.is_some() { if live { 0xFFFFFF } else { AMBER } } else { sub }, Align::Left);
    // Bottom: how long, and the knob.
    c.rect((x + 36.0, y + h - 82.0, w - 72.0, 1.0), if live { 0xD9554C } else { LINE }, 1.0);
    let secs = v.now_ms.saturating_sub(k.started_ms) / 1000;
    c.text(F::Bold, &call_mode::timer(secs), 44.0, x + 38.0, y + h - 26.0, if live { 0xFFFFFF } else { TEXT }, Align::Left);
    let hint = "double-click  hide";
    let (key, rest) = hint.split_once("  ").unwrap_or((hint, ""));
    let rw = c.measure(F::Medium, rest, 16.0);
    c.text(F::Medium, rest, 16.0, x + w - 36.0, y + h - 36.0, sub, Align::Right);
    c.text(F::Bold, key, 16.0, x + w - 42.0 - rw, y + h - 36.0, if live { 0xFFFFFF } else { MUTED }, Align::Right);

    next_event(c, v);
    hide_button(c);
    clip_grid(c, v);
}

fn next_event(c: &mut Canvas, v: &View) {
    let (x, y, w, h) = EVENT;
    c.rrect(EVENT, 16.0, PANEL, 1.0);
    let cal = v.calendar;
    let now = v.now_ms;
    let Some(e) = cal.next(now) else {
        c.symbol("calendar", 22.0, x + 40.0, y + h / 2.0, FAINT);
        let msg = if cal.authorized { "Nothing else on the calendar today" } else { "Calendar not connected" };
        c.text(F::Medium, msg, 22.0, x + 66.0, y + h / 2.0 + 8.0, MUTED, Align::Left);
        return;
    };
    let cal_color = crate::config::hex(v.ring.color_for(Page::Calendar), AMBER);
    let col = crate::config::hex(&e.color, cal_color);
    c.rrect((x, y, 6.0, h), 3.0, col, 1.0);
    let label = format!("NEXT   ·   {}", crate::calendar::hm_ampm(e.start_ms));
    c.text(F::Bold, &label, 14.0, x + 30.0, y + 38.0, MUTED, Align::Left);
    let until = crate::calendar::when(now, e.start_ms, e.end_ms);
    let soon = e.start_ms - now <= 15 * 60_000;
    let mut right = x + w - 26.0;
    if !e.join_url.is_empty() {
        let jr = (right - 120.0, y + 26.0, 120.0, h - 52.0);
        c.rrect(jr, 12.0, if soon { 0x2FA35C } else { 0x2A2A31 }, 1.0);
        c.symbol("video.fill", 17.0, jr.0 + 30.0, jr.1 + jr.3 / 2.0, 0xFFFFFF);
        c.text(F::Bold, "Join", 19.0, jr.0 + 50.0, jr.1 + jr.3 / 2.0 + 7.0, 0xFFFFFF, Align::Left);
        right = jr.0 - 22.0;
    }
    let uw = c.text(F::Bold, &until, 28.0, right, y + 82.0, if soon { cal_color } else { TEXT }, Align::Right);
    c.fit(F::Bold, &e.title, 30.0, right - uw - x - 60.0, x + 30.0, y + 82.0, TEXT, Align::Left);
}

fn hide_button(c: &mut Canvas) {
    let (x, y, w, _) = HIDE;
    c.rrect(HIDE, 16.0, PANEL, 1.0);
    c.symbol("eye.slash", 26.0, x + w / 2.0, y + 42.0, TEXT_2);
    c.text(F::Medium, "Hide", 18.0, x + w / 2.0, y + 86.0, TEXT_2, Align::Center);
}

fn clip_grid(c: &mut Canvas, v: &View) {
    let s = v.sounds;
    let k = v.call;
    // What happens to the clips, and the knob.
    c.text(F::Bold, "SOUNDBOARD", 13.0, RX + 4.0, STRIP_Y + 20.0, FAINT, Align::Left);
    let n = clips(v);
    if n == 0 {
        let r = (RX, GRID_Y, RW, H - M - GRID_Y);
        c.rrect(r, 16.0, PANEL_DEEP, 1.0);
        c.text(F::Medium, "No soundboard clips yet", 26.0, RX + RW / 2.0, GRID_Y + r.3 / 2.0, TEXT_2, Align::Center);
        c.text(F::Regular, "Drop audio files into ~/Music/Quaked Soundboard", 18.0, RX + RW / 2.0, GRID_Y + r.3 / 2.0 + 32.0, FAINT, Align::Center);
        return;
    }
    let (note, col) = if k.mute == Mute::Soundboard {
        (format!("{} hears these", k.app), GREEN)
    } else {
        (format!("only you hear these: pick BlackHole 2ch as the mic in {} to share them", k.app), AMBER)
    };
    let nw = c.text(F::Medium, &note, 15.0, RX + 110.0, STRIP_Y + 20.0, col, Align::Left);
    let mut hx = RX + RW - 4.0;
    for (key, rest) in [("hold", "play"), ("turn", "pick")] {
        let rw = c.text(F::Medium, rest, 15.0, hx, STRIP_Y + 20.0, FAINT, Align::Right);
        let kw = c.text(F::Bold, key, 15.0, hx - rw - 6.0, STRIP_Y + 20.0, MUTED, Align::Right);
        hx -= rw + kw + 6.0 + 26.0;
        if hx < RX + 110.0 + nw + 40.0 {
            break;
        }
    }
    for i in 0..COLS * ROWS {
        let r = clip_rect(i);
        let Some(clip) = s.clips.get(i).filter(|_| i < n) else {
            c.rrect(r, 14.0, PANEL_DEEP, 1.0);
            continue;
        };
        let (x, y, w, h) = r;
        let col = crate::config::hex(&clip.color, 0xB36BFF);
        let playing = s.playing.contains(&i);
        c.rrect(r, 14.0, PANEL, 1.0);
        if playing {
            c.rrect(r, 14.0, col, 0.28);
            c.outline(r, 14.0, col, 2.0);
        }
        c.rrect((x, y, 6.0, h), 3.0, col, 1.0);
        let sym = if clip.symbol.is_empty() { if playing { "speaker.wave.3.fill" } else { "waveform" } } else { clip.symbol.as_str() };
        c.symbol(sym, 34.0, x + 46.0, y + 42.0, col);
        if playing {
            c.pill(x + w - 100.0, y + 16.0, "PLAYING", 12.0, col, 0x111114);
        }
        c.paragraph(F::Bold, &clip.title, 24.0, x + 24.0, y + h - 50.0, w - 44.0, 2, 28.0, TEXT);
        if i == k.sel {
            c.outline(r, 14.0, TEXT, 3.0);
        }
    }
}

/// Touch while it's showing: return Hit::Feature(Page::Dash, i) for card / button `i`.
pub(super) fn hit(v: &View, x: f32, y: f32) -> Option<Hit> {
    let i = if inside(STATE, x, y) {
        call_mode::TAP_MUTE
    } else if inside(HIDE, x, y) {
        call_mode::TAP_HIDE
    } else if inside(EVENT, x, y) {
        v.calendar.next(v.now_ms).filter(|e| !e.join_url.is_empty())?;
        call_mode::TAP_EVENT
    } else {
        call_mode::TAP_CLIP + (0..clips(v)).find(|&i| inside(clip_rect(i), x, y))?
    };
    Some(Hit::Feature(Page::Dash, i))
}

/// Where Hit::Feature(_, i) is, for the tap flash.
pub(super) fn rect(_v: &View, i: usize) -> Option<R> {
    match i {
        call_mode::TAP_MUTE => Some(STATE),
        call_mode::TAP_HIDE => Some(HIDE),
        call_mode::TAP_EVENT => Some(EVENT),
        i if i >= call_mode::TAP_CLIP => Some(clip_rect(i - call_mode::TAP_CLIP)),
        _ => None,
    }
}
