//! Window switcher on the board: a row of big app cards, front to back, like ⌘-Tab with
//! room for each app's window titles. Knob selects, press or tap brings it forward.
use super::*;
use crate::windows::{AppWin, FEEDBACK_MS, ICON_PX};

const STRIP: f32 = 44.0;
const TOP: f32 = CY + STRIP + 6.0;
const CARD_H: f32 = CY + CH - TOP;
const ICON: f32 = ICON_PX as f32;
const TITLE_TOP: f32 = 240.0;
const TITLE_H: f32 = 25.0;

/// Cards across: fewer, wider cards when there are only a few apps.
fn cols(n: usize) -> usize {
    n.clamp(4, 7)
}
fn card(slot: usize, cols: usize) -> R {
    let w = (W - 2.0 * M - (cols as f32 - 1.0) * G) / cols as f32;
    (M + slot as f32 * (w + G), TOP, w, CARD_H)
}
/// (first visible card, columns).
fn layout(v: &View) -> (usize, usize) {
    let n = v.windows.apps.len();
    let cols = cols(n);
    (list_window(n, v.tile.min(n.saturating_sub(1)), cols), cols)
}

fn empty(c: &mut Canvas, v: &View, title: &str, sub: &str) {
    c.rrect((M, CY, W - 2.0 * M, CH), 16.0, PANEL_DEEP, 1.0);
    c.symbol(page_symbol(Page::Windows), 46.0, W / 2.0, CY + CH / 2.0 - 60.0, v.accent);
    c.text(F::Medium, title, 34.0, W / 2.0, CY + CH / 2.0 + 10.0, TEXT_2, Align::Center);
    c.text(F::Regular, sub, 20.0, W / 2.0, CY + CH / 2.0 + 50.0, FAINT, Align::Center);
}

pub(super) fn draw(c: &mut Canvas, v: &View) {
    let st = v.windows;
    if st.off {
        return empty(c, v, "Window switcher is off", "Turn it on in Quaked Settings (Extras › Window switcher).");
    }
    if !st.loaded {
        return empty(c, v, "Looking at your windows…", "Apps show up here front to back.");
    }
    if st.apps.is_empty() {
        return empty(c, v, "No apps open", "Open an app on this Mac and it shows up here.");
    }
    strip(c, v);
    let (first, cols) = layout(v);
    for slot in 0..cols {
        let i = first + slot;
        let Some(a) = st.apps.get(i) else { break };
        app_card(c, v, a, card(slot, cols), v.level != &Level::Top && v.tile == i);
    }
}

fn strip(c: &mut Canvas, v: &View) {
    let st = v.windows;
    let y = CY + 27.0;
    let n = st.apps.len();
    let windows: usize = st.apps.iter().map(|a| a.windows).sum();
    let mut x = M + 10.0;
    x += c.text(F::Medium, &plural(n, "app", "apps"), 18.0, x, y, TEXT, Align::Left);
    x += c.text(F::Medium, "·", 18.0, x + 14.0, y, FAINT, Align::Left) + 28.0;
    x += c.text(F::Medium, &format!("{} on screen", plural(windows, "window", "windows")), 18.0, x, y, TEXT_2, Align::Left);
    let (first, cols) = layout(v);
    if n > cols {
        x += c.text(F::Medium, "·", 18.0, x + 14.0, y, FAINT, Align::Left) + 28.0;
        c.text(F::Medium, &format!("{}–{} of {n}", first + 1, (first + cols).min(n)), 18.0, x, y, MUTED, Align::Left);
    }
    let recent = st.switched.as_ref().filter(|(_, at)| v.now_ms < at + FEEDBACK_MS);
    if let Some((name, _)) = recent {
        let w = c.fit(F::Bold, &format!("Switched to {name}"), 18.0, 520.0, W - M - 6.0, y, GREEN, Align::Right);
        c.symbol("checkmark.circle.fill", 18.0, W - M - 6.0 - w - 16.0, CY + 21.0, GREEN);
    } else if st.titles_hidden {
        let w = c.text(F::Medium, "window titles need Screen Recording permission", 15.0, W - M - 6.0, y - 1.0, FAINT, Align::Right);
        c.symbol("eye.slash", 14.0, W - M - 6.0 - w - 14.0, CY + 21.0, FAINT);
    }
}

fn app_card(c: &mut Canvas, v: &View, a: &AppWin, r: R, selected: bool) {
    let (x, y, w, h) = r;
    let cx = x + w / 2.0;
    c.rrect(r, 16.0, if selected { 0x1C1C22 } else { PANEL_DEEP }, 1.0);
    if a.front {
        c.rrect((x + 30.0, y, w - 60.0, 3.0), 1.5, v.accent, 1.0);
    }
    // Icon
    let ir = (cx - ICON / 2.0, y + 22.0, ICON, ICON);
    match &a.icon {
        Some(icon) => c.image(&icon.0, ir, 0.0),
        None => {
            let inset = (ir.0 + 8.0, ir.1 + 8.0, ir.2 - 16.0, ir.3 - 16.0);
            c.rrect(inset, 22.0, 0x2A2A31, 1.0);
            let drawn = !a.symbol.is_empty() && c.symbol(&a.symbol, 44.0, cx, ir.1 + ICON / 2.0, TEXT_2);
            if !drawn {
                let initial: String = a.name.chars().take(1).collect::<String>().to_uppercase();
                c.text(F::Bold, &initial, 46.0, cx, ir.1 + ICON / 2.0 + 16.0, TEXT_2, Align::Center);
            }
        }
    }
    if a.hidden {
        c.rrect(ir, 24.0, if selected { 0x1C1C22 } else { PANEL_DEEP }, 0.55);
    }
    // Name and what it has open.
    c.fit(F::Bold, &a.name, 24.0, w - 28.0, cx, y + 168.0, if a.hidden { TEXT_2 } else { TEXT }, Align::Center);
    let what = if a.hidden {
        "hidden".to_string()
    } else if a.windows == 0 {
        "no windows here".to_string()
    } else {
        plural(a.windows, "window", "windows")
    };
    let (line, color) = if a.front { (format!("in front  ·  {what}"), v.accent) } else { (what, MUTED) };
    c.fit(F::Medium, &line, 15.0, w - 28.0, cx, y + 194.0, color, Align::Center);
    c.rect((x + 18.0, y + 212.0, w - 36.0, 1.0), LINE, 1.0);

    // Window titles (or one glyph per window when macOS keeps the titles to itself).
    let rows = ((h - 18.0 - TITLE_TOP) / TITLE_H) as usize + 1;
    if !a.titles.is_empty() {
        let more = a.titles.len() > rows;
        let shown = if more { rows - 1 } else { a.titles.len() };
        for (k, t) in a.titles.iter().take(shown).enumerate() {
            let ty = y + TITLE_TOP + k as f32 * TITLE_H;
            c.symbol("macwindow", 13.0, x + 27.0, ty - 5.0, FAINT);
            c.clip(F::Medium, t, 15.0, w - 58.0, x + 42.0, ty, TEXT_2);
        }
        if more {
            let ty = y + TITLE_TOP + shown as f32 * TITLE_H;
            c.text(F::Medium, &format!("+{} more", a.titles.len() - shown), 14.0, x + 42.0, ty, FAINT, Align::Left);
        }
    } else if a.windows > 0 {
        let k = a.windows.min(((w - 40.0) / 38.0) as usize);
        let step = 38.0;
        let sx = cx - (k as f32 - 1.0) * step / 2.0;
        for j in 0..k {
            c.symbol("macwindow", 26.0, sx + j as f32 * step, y + TITLE_TOP + 10.0, if a.front { TEXT_2 } else { MUTED });
        }
    } else {
        let note = if a.hidden { "press to show it" } else { "minimized or on another Space" };
        c.fit(F::Medium, note, 14.0, w - 28.0, cx, y + TITLE_TOP + 8.0, FAINT, Align::Center);
    }
    if selected {
        c.outline(r, 16.0, TEXT, 3.0);
    }
}

/// Touch on this page: a card brings that app forward.
pub(super) fn hit(v: &View, x: f32, y: f32) -> Option<Hit> {
    let st = v.windows;
    if st.off || st.apps.is_empty() {
        return None;
    }
    let (first, cols) = layout(v);
    let slot = (0..cols).find(|&s| inside(card(s, cols), x, y))?;
    let i = first + slot;
    (i < st.apps.len()).then_some(Hit::Feature(Page::Windows, i))
}

/// Where Hit::Feature(_, i) is, for the tap flash.
pub(super) fn rect(v: &View, i: usize) -> Option<R> {
    let (first, cols) = layout(v);
    let slot = i.checked_sub(first)?;
    (slot < cols && i < v.windows.apps.len()).then(|| card(slot, cols))
}

pub(super) fn hints(level: &Level) -> &'static str {
    match level {
        Level::Top => "turn  switch page   ·   press  pick an app   ·   tap  switch",
        _ => "turn  choose   ·   press  bring forward   ·   double-click  back",
    }
}

/// The breadcrumb next to the tabs.
pub(super) fn crumb(v: &View) -> Option<String> {
    if v.level == &Level::Top {
        return None;
    }
    v.windows.apps.get(v.tile).map(|a| format!("›  {}", a.name))
}
