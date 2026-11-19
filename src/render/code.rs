//! CODE page: which Mac a pull request opens on, when there's more than one. A press on a
//! PR puts a button per Mac over the lists, left to right as they sit; turn to pick, press
//! to open there. The Mac you picked last is offered first.
use super::*;

/// Tap index for anywhere on the card but a Mac's button (backs out).
pub const TAP_CANCEL: usize = 99;

const CARD: R = (M, CODE_TOP, W - 2.0 * M, CY + CH - CODE_TOP);
const BUTTON_W: f32 = 300.0;
const BUTTON_GAP: f32 = 16.0;

/// The Mac picked (its network name), while choosing.
fn chosen<'a>(v: &'a View) -> Option<&'a str> {
    match v.level {
        Level::Detail(m) => Some(m.as_str()),
        _ => None,
    }
}

/// A button per Mac, left to right as they sit: (index into `desk.macs`, where).
fn buttons(v: &View) -> Vec<(usize, R)> {
    let order = crate::config::left_to_right(&v.desk.macs, v.desk.sides);
    let n = order.len().min(3);
    let (x, y, w, h) = CARD;
    let x0 = x + w - 24.0 - n as f32 * BUTTON_W - n.saturating_sub(1) as f32 * BUTTON_GAP;
    order.into_iter().take(n).enumerate().map(|(k, i)| (i, (x0 + k as f32 * (BUTTON_W + BUTTON_GAP), y + 24.0, BUTTON_W, h - 48.0))).collect()
}

/// The chooser, over the two lists.
pub(super) fn draw(c: &mut Canvas, v: &View) {
    let Some(p) = v.code.all().get(v.tile).copied() else { return };
    let (x, y, w, h) = CARD;
    c.rrect(CARD, 14.0, PANEL_DEEP, 1.0);
    c.outline(CARD, 14.0, v.accent, 2.0);
    let bs = buttons(v);
    let tx = x + 28.0;
    let text_w = bs.first().map(|(_, r)| r.0 - tx - 36.0).unwrap_or(w - 56.0);
    c.text(F::Bold, "OPEN ON WHICH MAC?", 12.0, tx, y + 36.0, v.accent, Align::Left);
    let (sym, col) = match p.ci.as_str() {
        "SUCCESS" => ("checkmark.circle.fill", GREEN),
        "FAILURE" | "ERROR" => ("xmark.circle.fill", RED),
        "PENDING" | "EXPECTED" => ("clock.fill", AMBER),
        _ => ("circle.dashed", FAINT),
    };
    c.symbol(sym, 20.0, tx + 10.0, y + 76.0, col);
    c.clip(F::Mono, &pr_ref(p), 17.0, text_w - 32.0, tx + 30.0, y + 82.0, if p.gitlab { 0xE8A27C } else { MUTED });
    let lines = c.fonts.wrap(F::Bold, &p.title, 30.0, text_w, 3);
    for (i, l) in lines.iter().enumerate() {
        c.text(F::Bold, l, 30.0, tx, y + 132.0 + i as f32 * 40.0, if p.draft { MUTED } else { TEXT }, Align::Left);
    }
    let ago = age(v.now_ms.saturating_sub(p.updated));
    let when = if ago == "now" { "updated just now".to_string() } else { format!("updated {ago} ago") };
    let meta = if p.author.is_empty() { when } else { format!("@{}   ·   {when}", p.author) };
    c.fit(F::Medium, &meta, 17.0, text_w, tx, y + h - 28.0, TEXT_2, Align::Left);
    let picked = chosen(v);
    for (i, r) in bs {
        let name = v.desk.macs[i].as_str();
        mac_button(c, v, r, name, i == 0, picked == Some(name));
    }
}

fn mac_button(c: &mut Canvas, v: &View, r: R, name: &str, here: bool, picked: bool) {
    let (bx, by, bw, bh) = r;
    let (bg, fg, sub) = if picked { (v.accent, 0x111114, 0x3A3A40) } else { (0x1C1C21, TEXT, MUTED) };
    c.rrect(r, 16.0, bg, 1.0);
    if picked {
        c.outline(r, 16.0, TEXT, 2.0);
    }
    let cx = bx + bw / 2.0;
    let icon_y = by + bh * 0.36;
    match v.desk.sides.get(name) {
        Some(&s) => side_arrow(c, s, cx, icon_y, 46.0, fg),
        None => {
            c.symbol("desktopcomputer", 42.0, cx, icon_y, fg);
        }
    }
    c.fit(F::Bold, name, 26.0, bw - 32.0, cx, by + bh * 0.68, fg, Align::Center);
    if here {
        c.text(F::Medium, "with the Quake", 15.0, cx, by + bh * 0.68 + 30.0, sub, Align::Center);
    }
}

pub(super) fn hit(v: &View, x: f32, y: f32) -> Option<Hit> {
    if let Some((i, _)) = buttons(v).into_iter().find(|(_, r)| inside(*r, x, y)) {
        return Some(Hit::Feature(Page::Code, i));
    }
    inside(CARD, x, y).then_some(Hit::Feature(Page::Code, TAP_CANCEL))
}

/// Where Hit::Feature(_, i) is, for the tap flash.
pub(super) fn rect(v: &View, i: usize) -> Option<R> {
    buttons(v).into_iter().find(|(j, _)| *j == i).map(|(_, r)| r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_button_per_mac_as_they_sit() {
        let mut sides = BTreeMap::new();
        sides.insert("Mac mini".to_string(), Side::Right);
        sides.insert("MacBook Pro".to_string(), Side::Left);
        let macs = vec!["Mac mini".to_string(), "MacBook Pro".to_string()];
        let order = crate::config::left_to_right(&macs, &sides);
        // The Mac on the left gets the left button.
        assert_eq!(order, [1, 0]);
        let (x, y, w, h) = CARD;
        let n = 2.0;
        let x0 = x + w - 24.0 - n * BUTTON_W - BUTTON_GAP;
        assert!(x0 > x + 600.0, "room for the pull request on the left");
        assert!(y + 24.0 + (h - 48.0) <= CY + CH);
    }

    #[test]
    fn review_requests_come_first_on_the_knob() {
        let pr = |n: u64| github::Pr { number: n, ..Default::default() };
        let g = github::Status { mine: vec![pr(1), pr(2), pr(3)], reviews: vec![pr(9)], ..Default::default() };
        assert_eq!(g.all().iter().map(|p| p.number).collect::<Vec<_>>(), [9, 1, 2, 3]);
        let [mine, reviews] = pr_lists(&g, 0);
        assert_eq!((mine.2, mine.3, reviews.2, reviews.3), (1, 3, 0, 1));
        // Turning onto your PRs keeps the right one in view.
        let [mine, _] = pr_lists(&g, 3);
        assert_eq!(mine.1, 0);
    }
}
