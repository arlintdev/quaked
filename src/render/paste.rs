//! Clipboard picker on the board: recent copies on the left (knob selects), the selected
//! one in full on the right with where ⌘V will go.
use super::*;
use crate::clipboard::{Clip, Kind, FEEDBACK_MS};

const STRIP: f32 = 44.0;
const TOP: f32 = CY + STRIP + 6.0;
const PANEL_H: f32 = CY + CH - TOP;
const LIST_W: f32 = 1130.0;
const HEAD: f32 = 40.0;
const ROW_H: f32 = 50.0;
const ROWS: usize = ((PANEL_H - HEAD - 6.0) / ROW_H) as usize;
const LINK: u32 = 0x8AB4F8;

fn list_panel() -> R {
    (M, TOP, LIST_W, PANEL_H)
}
fn detail_panel() -> R {
    (M + LIST_W + G, TOP, W - 2.0 * M - LIST_W - G, PANEL_H)
}
fn row(slot: usize) -> R {
    let (x, y, w, _) = list_panel();
    (x + 8.0, y + HEAD + slot as f32 * ROW_H, w - 16.0, ROW_H)
}
/// First visible row.
fn first_row(v: &View) -> usize {
    let n = v.paste.items.len();
    list_window(n, v.tile.min(n.saturating_sub(1)), ROWS)
}

fn kind_symbol(k: Kind) -> &'static str {
    match k {
        Kind::Text => "textformat",
        Kind::Lines => "text.alignleft",
        Kind::Code => "chevron.left.forwardslash.chevron.right",
        Kind::Link => "link",
        Kind::Path => "folder.fill",
        Kind::Email => "envelope.fill",
        Kind::Number => "number",
    }
}
fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Text => "TEXT",
        Kind::Lines => "LINES",
        Kind::Code => "CODE",
        Kind::Link => "LINK",
        Kind::Path => "PATH",
        Kind::Email => "EMAIL",
        Kind::Number => "NUMBER",
    }
}
fn kind_color(k: Kind, accent: u32) -> u32 {
    match k {
        Kind::Link | Kind::Email => LINK,
        Kind::Code => accent,
        _ => MUTED,
    }
}

/// "42 chars", "1.2k chars", "6 lines".
fn size_label(c: &Clip) -> String {
    if c.lines > 1 {
        format!("{} lines", c.lines)
    } else if c.chars >= 1000 {
        format!("{:.1}k chars", c.chars as f32 / 1000.0)
    } else {
        plural(c.chars, "char", "chars")
    }
}

fn empty(c: &mut Canvas, v: &View, title: &str, sub: &str) {
    c.rrect((M, CY, W - 2.0 * M, CH), 16.0, PANEL_DEEP, 1.0);
    c.symbol(page_symbol(Page::Paste), 46.0, W / 2.0, CY + CH / 2.0 - 60.0, v.accent);
    c.text(F::Medium, title, 34.0, W / 2.0, CY + CH / 2.0 + 10.0, TEXT_2, Align::Center);
    c.text(F::Regular, sub, 20.0, W / 2.0, CY + CH / 2.0 + 50.0, FAINT, Align::Center);
}

pub(super) fn draw(c: &mut Canvas, v: &View) {
    let p = v.paste;
    if p.off {
        return empty(c, v, "Clipboard picker is off", "Turn it on in Quaked Settings (paste → enabled).");
    }
    if p.items.is_empty() {
        let sub = if p.skipped > 0 {
            format!("Copy some text and it shows up here. {} skipped; nothing is kept on disk.", plural(p.skipped, "private copy", "private copies"))
        } else {
            "Copy some text and it shows up here. Kept in memory only; password manager copies are skipped.".to_string()
        };
        return empty(c, v, "Nothing copied yet", &sub);
    }
    strip(c, v);
    list(c, v);
    detail(c, v);
}

fn strip(c: &mut Canvas, v: &View) {
    let p = v.paste;
    let y = CY + 27.0;
    c.symbol("lock.fill", 15.0, M + 12.0, CY + 21.0, MUTED);
    let mut x = M + 28.0;
    x += c.text(F::Medium, &plural(p.items.len(), "recent copy", "recent copies"), 18.0, x, y, TEXT, Align::Left);
    let mut parts = vec!["kept in memory only".to_string()];
    if p.skipped > 0 {
        parts.push(format!("{} skipped", plural(p.skipped, "private copy", "private copies")));
    }
    for part in parts {
        x += c.text(F::Medium, "·", 18.0, x + 14.0, y, FAINT, Align::Left) + 28.0;
        x += c.text(F::Medium, &part, 18.0, x, y, TEXT_2, Align::Left);
    }
    if !p.front.is_empty() {
        let w = c.fit(F::Bold, &p.front, 18.0, 320.0, W - M - 6.0, y, TEXT, Align::Right);
        c.text(F::Medium, "pastes into", 18.0, W - M - 6.0 - w - 10.0, y, MUTED, Align::Right);
    }
}

fn list(c: &mut Canvas, v: &View) {
    let p = v.paste;
    let focus = v.level != &Level::Top;
    let panel = list_panel();
    let (px, py, pw, _) = panel;
    c.rrect(panel, 14.0, PANEL_DEEP, 1.0);
    let n = p.items.len();
    let first = first_row(v);
    c.text(F::Bold, "RECENT COPIES", 12.0, px + 22.0, py + 26.0, FAINT, Align::Left);
    let count = if n > ROWS { format!("{}–{} of {n}", first + 1, (first + ROWS).min(n)) } else { n.to_string() };
    c.text(F::Bold, &count, 12.0, px + pw - 22.0, py + 26.0, FAINT, Align::Right);
    for slot in 0..ROWS {
        let i = first + slot;
        let Some(clip) = p.items.get(i) else { break };
        let r = row(slot);
        let (rx, ry, rw, rh) = r;
        let mid = ry + rh / 2.0;
        if focus && v.tile == i {
            c.rrect(r, 10.0, 0x22222A, 1.0);
            c.outline(r, 10.0, TEXT, 2.0);
        } else if slot > 0 {
            c.rect((rx + 14.0, ry, rw - 28.0, 1.0), LINE, 0.7);
        }
        c.symbol(kind_symbol(clip.kind), 17.0, rx + 24.0, mid, kind_color(clip.kind, v.accent));
        // Right to left: age, app, size, and a mark on what's on the clipboard now.
        let mut mx = rx + rw - 14.0;
        mx -= c.text(F::Medium, &age(v.now_ms.saturating_sub(clip.at_ms)), 14.0, mx, mid + 5.0, FAINT, Align::Right) + 16.0;
        if !clip.app.is_empty() {
            mx -= c.fit(F::Medium, &clip.app, 14.0, 130.0, mx, mid + 5.0, TEXT_2, Align::Right) + 16.0;
        }
        mx -= c.text(F::Medium, &size_label(clip), 13.0, mx, mid + 5.0, FAINT, Align::Right) + 14.0;
        if p.current == Some(clip.id) {
            let label = "ON CLIPBOARD";
            let w = c.measure(F::Bold, label, 10.0) + 14.0;
            c.pill(mx - w, mid - 9.0, label, 10.0, 0x1A2A44, LINK);
            mx -= w + 12.0;
        }
        let tx = rx + 50.0;
        let room = mx - 12.0 - tx;
        let fw = c.clip(F::Medium, &clip.first, 18.0, if clip.rest.is_empty() { room } else { room * 0.62 }, tx, mid + 6.0, TEXT);
        let left = room - fw - 14.0;
        if !clip.rest.is_empty() && left > 40.0 {
            c.clip(F::Regular, &clip.rest, 15.0, left, tx + fw + 14.0, mid + 6.0, MUTED);
        }
    }
}

fn detail(c: &mut Canvas, v: &View) {
    let p = v.paste;
    let focus = v.level != &Level::Top;
    let panel = detail_panel();
    let (dx, dy, dw, dh) = panel;
    c.rrect(panel, 14.0, PANEL_DEEP, 1.0);
    let Some(clip) = p.items.get(v.tile.min(p.items.len().saturating_sub(1))) else { return };
    let kc = kind_color(clip.kind, v.accent);
    c.symbol(kind_symbol(clip.kind), 14.0, dx + 30.0, dy + 21.0, kc);
    c.text(F::Bold, kind_name(clip.kind), 12.0, dx + 46.0, dy + 26.0, FAINT, Align::Left);
    let when = age(v.now_ms.saturating_sub(clip.at_ms));
    let meta = if clip.app.is_empty() { when } else { format!("{}  ·  {when}", clip.app) };
    c.fit(F::Medium, &meta, 13.0, dw - 160.0, dx + dw - 22.0, dy + 26.0, FAINT, Align::Right);

    // The text itself: code and lists keep their lines, prose wraps.
    let body_top = dy + 50.0;
    let body_bottom = dy + dh - 76.0;
    let tw = dw - 56.0;
    match clip.kind {
        Kind::Code | Kind::Lines => {
            let lh = 22.0;
            let max = ((body_bottom - body_top) / lh) as usize;
            let lines: Vec<&str> = clip.text.trim_matches('\n').lines().collect();
            let shown = if lines.len() > max { max.saturating_sub(1) } else { lines.len() };
            for (k, line) in lines.iter().take(shown).enumerate() {
                c.clip(F::Mono, &line.replace('\t', "    "), 15.0, tw, dx + 28.0, body_top + 16.0 + k as f32 * lh, TEXT_2);
            }
            if lines.len() > shown {
                let more = format!("… {} more", plural(lines.len() - shown, "line", "lines"));
                c.text(F::Medium, &more, 14.0, dx + 28.0, body_top + 16.0 + shown as f32 * lh, FAINT, Align::Left);
            }
        }
        _ => {
            let preview: String = clip.text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(900).collect();
            let color = if matches!(clip.kind, Kind::Link | Kind::Email) { LINK } else { TEXT };
            let lh = 31.0;
            let max = ((body_bottom - body_top) / lh) as usize;
            c.paragraph(F::Medium, &preview, 23.0, dx + 28.0, body_top + 22.0, tw, max, lh, color);
        }
    }

    // Footer: size on the left, the action (or what just happened) on the right.
    let fy = dy + dh - 64.0;
    c.rect((dx + 20.0, fy, dw - 40.0, 1.0), LINE, 1.0);
    let by = fy + 14.0;
    let bh = 38.0;
    let mut size = size_label(clip);
    if clip.lines > 1 {
        size = format!("{size}  ·  {}", plural(clip.chars, "char", "chars"));
    }
    c.text(F::Medium, &size, 14.0, dx + 28.0, by + 25.0, FAINT, Align::Left);
    let pasted = p.pasted.as_ref().filter(|x| x.id == clip.id && v.now_ms < x.at_ms + FEEDBACK_MS);
    let into = |app: &str| if app.is_empty() { String::new() } else { format!(" into {app}") };
    let (label, symbol, bg, fg) = if let Some(x) = pasted {
        (format!("Pasted{}", into(&x.into)), "checkmark.circle.fill", GREEN, 0xFFFFFF)
    } else if focus {
        (format!("Paste{}", into(&p.front)), "doc.on.clipboard.fill", v.accent, 0x101013)
    } else {
        ("Tap a copy to paste it".to_string(), "hand.tap.fill", 0x22222A, TEXT_2)
    };
    let lw = c.measure(F::Bold, &label, 17.0).min(dw * 0.62);
    let bw = lw + 66.0;
    let bx = dx + dw - 20.0 - bw;
    c.rrect((bx, by, bw, bh), bh / 2.0, bg, 1.0);
    c.symbol(symbol, 17.0, bx + 26.0, by + bh / 2.0, fg);
    c.fit(F::Bold, &label, 17.0, lw, bx + 44.0, by + 25.0, fg, Align::Left);
}

/// Touch on this page: a row pastes that copy; the preview pastes the selected one.
pub(super) fn hit(v: &View, x: f32, y: f32) -> Option<Hit> {
    let p = v.paste;
    let n = p.items.len();
    if p.off || n == 0 {
        return None;
    }
    let first = first_row(v);
    if let Some(slot) = (0..ROWS).find(|&s| inside(row(s), x, y)) {
        let i = first + slot;
        return (i < n).then_some(Hit::Feature(Page::Paste, i));
    }
    if inside(detail_panel(), x, y) {
        return Some(Hit::Feature(Page::Paste, v.tile.min(n - 1)));
    }
    None
}

/// Where Hit::Feature(_, i) is, for the tap flash.
pub(super) fn rect(v: &View, i: usize) -> Option<R> {
    let slot = i.checked_sub(first_row(v))?;
    (slot < ROWS && i < v.paste.items.len()).then(|| row(slot))
}

pub(super) fn hints(level: &Level) -> &'static str {
    match level {
        Level::Top => "turn  switch page   ·   press  pick a copy   ·   tap  paste",
        _ => "turn  choose   ·   press  paste   ·   hold  forget   ·   double-click  back",
    }
}

/// The breadcrumb next to the tabs.
pub(super) fn crumb(v: &View) -> Option<String> {
    if v.level == &Level::Top {
        return None;
    }
    v.paste.items.get(v.tile).map(|c| format!("›  {}", c.first))
}
