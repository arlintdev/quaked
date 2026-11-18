//! The Update pane: the new release on the left (version, when, its notes) and your Macs on
//! the right, each with its version and an Install button (press twice to install).
use super::*;
use crate::update::Status;

/// Tap index of the release card (opens its page on GitHub); rows are 0...
pub const TAP_RELEASE: usize = 100;

const LEFT_W: f32 = 1010.0;
const LEFT: R = (M, CY, LEFT_W, CH);
const MACS: R = (M + LEFT_W + G, CY, W - 2.0 * M - LEFT_W - G, CH);
const ROWS_TOP: f32 = CY + 46.0;
const ROW_H: f32 = 116.0;
const INSTALL_GREEN: u32 = 0x2FA35C;

/// Where one Mac stands.
#[derive(Debug, Clone, PartialEq)]
pub enum RowState {
    /// Has the latest release (or newer).
    Current,
    /// The latest release is newer: install it.
    Behind,
    /// Installing ("Downloading 0.4.0…").
    Busy(String),
    /// The last try failed, and why (press to try again).
    Failed(String),
    /// Its quaked is too old to say its version (or to update itself).
    Unknown,
}

/// One Mac on the pane.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub name: String,
    /// This Mac.
    pub here: bool,
    pub version: String,
    pub build: String,
    pub state: RowState,
}

/// What the pane shows (from the app).
pub struct UpdateView<'a> {
    pub status: &'a Status,
    pub rows: Vec<Row>,
    /// The row a first press armed.
    pub armed: Option<usize>,
}

fn row_rect(i: usize) -> R {
    (MACS.0 + 12.0, ROWS_TOP + i as f32 * ROW_H, MACS.2 - 24.0, ROW_H - 10.0)
}

fn button_rect(i: usize) -> R {
    let (x, y, w, h) = row_rect(i);
    (x + w - 236.0, y + (h - 64.0) / 2.0, 220.0, 64.0)
}

/// The release notes (Markdown) as plain blocks: (is a bullet, text).
pub fn note_blocks(md: &str) -> Vec<(bool, String)> {
    let mut out: Vec<(bool, String)> = Vec::new();
    let mut para = String::new();
    let flush = |para: &mut String, out: &mut Vec<(bool, String)>| {
        if !para.trim().is_empty() {
            out.push((false, para.trim().to_string()));
        }
        para.clear();
    };
    for line in md.lines() {
        let t = line.trim();
        if t.is_empty() {
            flush(&mut para, &mut out);
            continue;
        }
        if let Some(item) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            flush(&mut para, &mut out);
            out.push((true, plain(item)));
        } else if t.starts_with('#') {
            flush(&mut para, &mut out);
            out.push((false, plain(t.trim_start_matches('#').trim())));
        } else {
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(&plain(t));
        }
    }
    flush(&mut para, &mut out);
    out
}

/// Markdown emphasis, code and links as plain text.
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    // [text](url) -> text
    while let Some(i) = rest.find('[') {
        let Some(j) = rest[i..].find("](").map(|j| i + j) else { break };
        let Some(k) = rest[j..].find(')').map(|k| j + k) else { break };
        out.push_str(&rest[..i]);
        out.push_str(&rest[i + 1..j]);
        rest = &rest[k + 1..];
    }
    out.push_str(rest);
    out.replace("**", "").replace("__", "").replace('`', "")
}

pub(super) fn draw(c: &mut Canvas, v: &View) {
    release(c, v);
    macs(c, v);
}

fn release(c: &mut Canvas, v: &View) {
    let st = v.update.status;
    let (x, y, w, h) = LEFT;
    c.rrect(LEFT, 16.0, PANEL, 1.0);
    let Some(r) = &st.latest else {
        c.text(F::Bold, "UPDATES", 14.0, x + 32.0, y + 40.0, MUTED, Align::Left);
        let (msg, sub) = if st.checking {
            ("Checking GitHub…", String::new())
        } else {
            ("No release found", st.error.clone().unwrap_or_default())
        };
        c.text(F::Bold, msg, 40.0, x + 32.0, y + 110.0, TEXT_2, Align::Left);
        c.fit(F::Medium, &sub, 18.0, w - 64.0, x + 32.0, y + 146.0, MUTED, Align::Left);
        return;
    };
    let fresh = st.available().is_some();
    c.rrect((x, y, 6.0, h), 3.0, if fresh { v.accent } else { FAINT }, 1.0);
    let pw = c.pill(x + 30.0, y + 20.0, if fresh { "NEW RELEASE" } else { "LATEST RELEASE" }, 13.0, if fresh { v.accent } else { 0x2A2A31 }, if fresh { 0x101013 } else { TEXT_2 });
    let mut meta = format!("released {} ago", age(v.now_ms.saturating_sub(r.published_ms)));
    if r.zip_size > 0 {
        meta = format!("{meta}   ·   {:.1} MB   ·   Apple Silicon", r.zip_size as f64 / 1_000_000.0);
    }
    c.fit(F::Medium, &meta, 17.0, w - pw - 90.0, x + 44.0 + pw, y + 38.0, TEXT_2, Align::Left);
    let tw = c.text(F::Bold, &format!("quaked {}", r.version), 50.0, x + 30.0, y + 104.0, TEXT, Align::Left);
    c.text(F::Medium, &format!("you have {}", crate::update::CURRENT), 20.0, x + 30.0 + tw + 20.0, y + 102.0, MUTED, Align::Left);
    // Its notes, as much as fits.
    let bottom = y + h - 40.0;
    // `cy` is the top of the next block.
    let mut cy = y + 128.0;
    for (bullet, text) in note_blocks(&r.notes) {
        let lines = ((bottom - cy) / 25.0).floor() as usize;
        if lines == 0 {
            break;
        }
        let (tx, tw) = if bullet { (x + 52.0, w - 84.0) } else { (x + 30.0, w - 62.0) };
        if bullet {
            c.circle(x + 38.0, cy + 12.0, 3.0, MUTED, 1.0);
        }
        cy += c.paragraph(F::Medium, &text, 18.0, tx, cy + 18.0, tw, lines.min(4), 25.0, TEXT_2) + 8.0;
    }
    c.text(F::Medium, "Tap for the release on GitHub", 14.0, x + w - 26.0, y + h - 20.0, FAINT, Align::Right);
}

fn macs(c: &mut Canvas, v: &View) {
    let (x, y, w, _) = MACS;
    c.rrect(MACS, 16.0, PANEL_DEEP, 1.0);
    c.text(F::Bold, "YOUR MACS", 12.0, x + 24.0, y + 28.0, FAINT, Align::Left);
    let latest = v.update.status.latest.as_ref().map(|r| r.version.as_str()).unwrap_or("");
    let focus = v.level != &Level::Top;
    for (i, row) in v.update.rows.iter().enumerate().take(3) {
        let r = row_rect(i);
        let (rx, ry, rw, rh) = r;
        let selected = focus && v.tile == i;
        c.rrect(r, 12.0, if selected { 0x22222A } else { 0x17171C }, 1.0);
        if selected {
            c.outline(r, 12.0, TEXT, 2.0);
        }
        let side = v.desk.side(if row.here { "" } else { &row.name });
        let name = if row.name.is_empty() { "This Mac".to_string() } else { row.name.clone() };
        let bx = button_rect(i).0;
        let nw = mac_tag(c, &name, side, 22.0, rx + 20.0, ry + 44.0, bx - rx - 140.0);
        if row.here {
            c.pill(rx + 30.0 + nw, ry + 26.0, "THIS MAC", 11.0, 0x1F3A2A, 0x7FD9A0);
        }
        let mut sub = if row.version.is_empty() { "version unknown".to_string() } else { row.version.clone() };
        if !row.build.is_empty() {
            sub = format!("{sub}  ·  {}", row.build);
        }
        let (line2, col2) = match &row.state {
            RowState::Failed(e) => (e.clone(), RED),
            RowState::Unknown => ("older quaked: update it by hand once".to_string(), AMBER),
            _ => (sub, MUTED),
        };
        c.fit(F::Mono, &line2, 14.0, bx - rx - 36.0, rx + 20.0, ry + 80.0, col2, Align::Left);
        let b = button_rect(i);
        let mid = ry + rh / 2.0;
        match &row.state {
            RowState::Current => {
                let label = "UP TO DATE";
                let lw = c.measure(F::Bold, label, 13.0) + 24.0;
                c.pill(rx + rw - 20.0 - lw, mid - 13.0, label, 13.0, 0x1F3A2A, 0x7FD9A0);
            }
            RowState::Behind | RowState::Failed(_) => {
                let again = matches!(row.state, RowState::Failed(_));
                if v.update.armed == Some(i) {
                    c.rrect(b, 14.0, AMBER, 1.0);
                    c.text(F::Bold, "Press again", 22.0, b.0 + b.2 / 2.0, b.1 + 30.0, 0x1C1500, Align::Center);
                    c.text(F::Medium, "to install", 13.0, b.0 + b.2 / 2.0, b.1 + 50.0, 0x3A2E00, Align::Center);
                } else {
                    c.rrect(b, 14.0, INSTALL_GREEN, 1.0);
                    c.symbol(if again { "arrow.clockwise" } else { "arrow.down.circle.fill" }, 22.0, b.0 + 30.0, b.1 + b.3 / 2.0, 0xFFFFFF);
                    let label = if again { "Try again".to_string() } else { format!("Install {latest}") };
                    c.fit(F::Bold, &label, 22.0, b.2 - 64.0, b.0 + 52.0, b.1 + b.3 / 2.0 + 8.0, 0xFFFFFF, Align::Left);
                }
            }
            RowState::Busy(t) => {
                c.circle(b.0 + 14.0, mid, 6.0, v.accent, if v.tick { 1.0 } else { 0.35 });
                c.fit(F::Bold, t, 18.0, b.2 - 20.0, b.0 + 30.0, mid + 6.0, v.accent, Align::Left);
            }
            RowState::Unknown => {
                c.text(F::Medium, "update by hand", 16.0, rx + rw - 20.0, mid + 6.0, MUTED, Align::Right);
            }
        }
    }
    if let Some(e) = v.update.status.error.as_ref().filter(|_| v.update.status.latest.is_some()) {
        c.fit(F::Medium, &format!("Last check: {e}"), 13.0, w - 48.0, x + 24.0, y + CH - 16.0, AMBER, Align::Left);
    }
}

/// Touch on this page.
pub(super) fn hit(v: &View, x: f32, y: f32) -> Option<Hit> {
    if let Some(i) = (0..v.update.rows.len().min(3)).find(|&i| inside(row_rect(i), x, y)) {
        return Some(Hit::Feature(Page::Update, i));
    }
    (v.update.status.latest.is_some() && inside(LEFT, x, y)).then_some(Hit::Feature(Page::Update, TAP_RELEASE))
}

/// Where Hit::Feature(_, i) is, for the tap flash.
pub(super) fn rect(_v: &View, i: usize) -> Option<R> {
    if i == TAP_RELEASE {
        return Some(LEFT);
    }
    (i < 3).then(|| row_rect(i))
}

pub(super) fn hints(level: &Level) -> &'static str {
    match level {
        Level::Top => "turn  switch page   ·   press  choose a Mac   ·   tap twice  install",
        _ => "turn  choose a Mac   ·   press twice  install   ·   double-click  back",
    }
}

/// The breadcrumb next to the tabs.
pub(super) fn crumb(v: &View) -> Option<String> {
    if v.level == &Level::Top {
        return None;
    }
    v.update.rows.get(v.tile).map(|r| format!("›  {}", if r.name.is_empty() { "This Mac" } else { &r.name }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_read_as_plain_text() {
        let md = "**New:** an Update pane\nthat installs releases.\n\n## Fixes\n- Meetings join on the [right Mac](https://x)\n* `ctl join` takes links only\n";
        assert_eq!(
            note_blocks(md),
            vec![
                (false, "New: an Update pane that installs releases.".to_string()),
                (false, "Fixes".to_string()),
                (true, "Meetings join on the right Mac".to_string()),
                (true, "ctl join takes links only".to_string()),
            ]
        );
    }
}
