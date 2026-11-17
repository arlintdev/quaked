//! Mac health on the board: a column per Mac (this one first) with CPU, memory, GPU and
//! network sparklines, disk and uptime, over a strip of peripheral batteries.
use super::*;
use crate::health::{Battery, Mac};

const STRIP_H: f32 = 58.0;
/// Columns on screen at once; the knob scrolls through more.
const VISIBLE: usize = 3;
/// Batteries below this are amber, below CRITICAL red.
const LOW: u8 = 20;
const CRITICAL: u8 = 10;
const NET_OUT: u32 = 0x8AB4F8;

fn strip() -> R {
    (M, CY + CH - STRIP_H, W - 2.0 * M, STRIP_H)
}

/// (first Mac shown, how many columns)
fn window(n: usize, tile: usize) -> (usize, usize) {
    let cols = n.clamp(1, VISIBLE);
    (tile.saturating_sub(cols - 1).min(n.saturating_sub(cols)), cols)
}

fn col_rect(slot: usize, cols: usize) -> R {
    let h = CH - STRIP_H - G;
    let w = (W - 2.0 * M - (cols as f32 - 1.0) * G) / cols as f32;
    (M + slot as f32 * (w + G), CY, w, h)
}

fn rate(bps: f64) -> String {
    let b = bps.max(0.0);
    if b >= 1_073_741_824.0 {
        format!("{:.1} GB/s", b / 1_073_741_824.0)
    } else if b >= 1_048_576.0 {
        format!("{:.1} MB/s", b / 1_048_576.0)
    } else if b >= 1024.0 {
        format!("{:.0} KB/s", b / 1024.0)
    } else {
        format!("{b:.0} B/s")
    }
}

fn uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

fn level_color(pct: u8) -> u32 {
    if pct < CRITICAL {
        RED
    } else if pct < LOW {
        AMBER
    } else {
        TEXT
    }
}

pub(super) fn draw(c: &mut Canvas, v: &View) {
    let h = v.health;
    if h.macs.is_empty() {
        let r = (M, CY, W - 2.0 * M, CH - STRIP_H - G);
        c.rrect(r, 14.0, PANEL_DEEP, 1.0);
        c.symbol("waveform.path.ecg", 40.0, W / 2.0, r.1 + r.3 / 2.0 - 30.0, FAINT);
        let what = if h.off { "Mac health is off in the settings" } else { "Measuring this Mac…" };
        c.text(F::Medium, what, 24.0, W / 2.0, r.1 + r.3 / 2.0 + 30.0, FAINT, Align::Center);
    }
    let (first, cols) = window(h.macs.len(), v.tile);
    let focus = v.level != &Level::Top;
    for slot in 0..cols.min(h.macs.len()) {
        let i = first + slot;
        let r = col_rect(slot, cols);
        mac_column(c, v, &h.macs[i], r, i == 0);
        if focus && v.tile == i {
            c.outline(r, 14.0, TEXT, 3.0);
        }
    }
    if first > 0 {
        let r = col_rect(0, cols);
        c.pill(r.0 + 14.0, r.1 + r.3 - 40.0, &format!("‹ {first}"), 15.0, 0x2A2A31, TEXT);
    }
    let right = h.macs.len().saturating_sub(first + cols);
    if right > 0 {
        let r = col_rect(cols - 1, cols);
        let label = format!("{right} ›");
        let w = c.measure(F::Bold, &label, 15.0) + 15.0 * 1.4;
        c.pill(r.0 + r.2 - w - 14.0, r.1 + r.3 - 40.0, &label, 15.0, 0x2A2A31, TEXT);
    }
    battery_strip(c, &h.batteries());
}

fn mac_column(c: &mut Canvas, v: &View, m: &Mac, r: R, local: bool) {
    let (x, y, w, h) = r;
    c.rrect(r, 14.0, PANEL, 1.0);
    c.rrect((x, y, 6.0, h), 3.0, if m.ok { v.accent } else { 0x3A3A42 }, 1.0);
    let ix = x + 26.0;
    let right = x + w - 24.0;

    // Header: name (+ THIS MAC), chip · cores · uptime; disk and battery on the right.
    let mut rx = right;
    if m.ok {
        rx = disk_block(c, v, m, rx, y);
        if let Some(p) = &m.power {
            rx = power_block(c, p, rx - 28.0, y);
        }
    }
    let name_max = (rx - 24.0 - ix - if local { 100.0 } else { 0.0 }).max(80.0);
    let nw = c.fit(F::Bold, &m.name, 26.0, name_max, ix, y + 40.0, if m.ok { TEXT } else { TEXT_2 }, Align::Left);
    if local {
        c.pill(ix + nw + 12.0, y + 20.0, "THIS MAC", 11.0, 0x1E3634, v.accent);
    }
    let mut sub: Vec<String> = Vec::new();
    if !m.chip.is_empty() {
        sub.push(m.chip.clone());
    }
    if m.cores > 0 {
        sub.push(format!("{} cores", m.cores));
    }
    if m.ok && m.uptime_s > 0 {
        sub.push(format!("up {}", uptime(m.uptime_s)));
    }
    c.clip(F::Medium, &sub.join("  ·  "), 15.0, (rx - 24.0 - ix).max(80.0), ix, y + 64.0, MUTED);

    if !m.ok {
        let why = m.error.as_deref().unwrap_or("not answering");
        let waiting = why.ends_with('…');
        let cy = y + h / 2.0 + 20.0;
        c.symbol(if waiting { "ellipsis" } else { "exclamationmark.triangle" }, 30.0, x + w / 2.0, cy - 28.0, FAINT);
        let lines = c.fonts.wrap(F::Medium, why, 19.0, w - 80.0, 2);
        for (k, l) in lines.iter().enumerate() {
            c.text(F::Medium, l, 19.0, x + w / 2.0, cy + 20.0 + k as f32 * 26.0, FAINT, Align::Center);
        }
        return;
    }

    // Metrics: four across when there's room, two by two, or a row each in narrow columns.
    let area = (ix - 6.0, y + 84.0, w - 40.0, h - 98.0);
    let per_row = if area.2 > 1300.0 {
        4
    } else if area.2 > 760.0 {
        2
    } else {
        1
    };
    let rows = 4 / per_row;
    let cw = (area.2 - (per_row as f32 - 1.0) * G) / per_row as f32;
    let gap = if per_row == 1 { 6.0 } else { G };
    let ch = (area.3 - (rows as f32 - 1.0) * gap) / rows as f32;
    let cell = |k: usize| -> R { (area.0 + (k % per_row) as f32 * (cw + G), area.1 + (k / per_row) as f32 * (ch + gap), cw, ch) };

    let cpu_col = usage_color(m.cpu / 100.0, v.accent);
    metric(c, cell(0), &Metric { label: "CPU", value: format!("{:.0}%", m.cpu), sub: avg_peak(&m.hist.cpu), lines: vec![(&m.hist.cpu, cpu_col)], floor: 40.0, ..Default::default() });

    let mem_col = match m.pressure {
        4 => RED,
        2 => AMBER,
        _ => v.accent,
    };
    let mut mem_sub = format!("of {}", whole_gb(m.mem_total));
    match m.pressure {
        4 => mem_sub.push_str("  ·  pressure critical"),
        2 => mem_sub.push_str("  ·  pressure high"),
        _ if m.swap_used > 0 => mem_sub.push_str(&format!("  ·  swap {}", bytes(m.swap_used))),
        _ => {}
    }
    metric(c, cell(1), &Metric { label: "MEMORY", value: bytes(m.mem_used), sub: mem_sub, lines: vec![(&m.hist.mem, mem_col)], floor: 1.0, ..Default::default() });

    let gpu = match m.gpu {
        Some(g) => Metric { label: "GPU", value: format!("{g:.0}%"), sub: avg_peak(&m.hist.gpu), lines: vec![(&m.hist.gpu, usage_color(g / 100.0, v.accent))], floor: 40.0, ..Default::default() },
        None => Metric { label: "GPU", value: "—".into(), sub: "no reading".into(), floor: 40.0, ..Default::default() },
    };
    metric(c, cell(2), &gpu);

    // Down (in) in the accent, up (out) in blue, on one scale.
    metric(
        c,
        cell(3),
        &Metric {
            label: "NETWORK",
            value: rate(m.net_in),
            value_sym: Some(("arrow.down", v.accent)),
            sub: format!("{} up", rate(m.net_out)),
            sub_sym: Some(("arrow.up", NET_OUT)),
            lines: vec![(&m.hist.net_out, NET_OUT), (&m.hist.net_in, v.accent)],
            floor: 64.0 * 1024.0,
        },
    );
}

/// Memory sizes as Macs are sold: "24 GB", "64 GB".
fn whole_gb(n: u64) -> String {
    let gb = n as f64 / 1_073_741_824.0;
    if gb >= 10.0 { format!("{gb:.0} GB") } else { bytes(n) }
}

/// "avg 21%  ·  peak 48%" over the history.
fn avg_peak(v: &[f32]) -> String {
    if v.is_empty() {
        return String::new();
    }
    let avg = v.iter().sum::<f32>() / v.len() as f32;
    let peak = v.iter().copied().fold(0.0, f32::max);
    format!("avg {avg:.0}%  ·  peak {peak:.0}%")
}

#[derive(Default)]
struct Metric<'a> {
    label: &'a str,
    value: String,
    /// A small symbol after the value / after the line under it.
    value_sym: Option<(&'static str, u32)>,
    sub: String,
    sub_sym: Option<(&'static str, u32)>,
    /// Sparklines sharing one scale, drawn in order.
    lines: Vec<(&'a Vec<f32>, u32)>,
    /// The scale's least maximum.
    floor: f32,
}

/// Where a metric cell's parts go.
struct Layout {
    label: (f32, f32, f32),
    /// x, baseline, size, max width
    value: (f32, f32, f32, f32),
    sub: (f32, f32, f32, f32),
    graph: R,
}

/// Tall cells put the graph under the numbers, short ones to their right, and rows (narrow
/// columns) read across: label, value, line, graph.
fn layout((x, y, w, h): R) -> Layout {
    if h > 160.0 {
        Layout { label: (x + 18.0, y + 30.0, 13.0), value: (x + 18.0, y + 84.0, 40.0, w - 64.0), sub: (x + 18.0, y + 110.0, 14.0, w - 56.0), graph: (x + 18.0, y + 128.0, w - 36.0, h - 146.0) }
    } else if h > 70.0 {
        let tw = w * 0.47 - 18.0;
        Layout { label: (x + 18.0, y + 30.0, 13.0), value: (x + 18.0, y + 70.0, 34.0, tw - 28.0), sub: (x + 18.0, y + 96.0, 14.0, tw - 4.0), graph: (x + w * 0.47 + 10.0, y + 18.0, w * 0.53 - 28.0, h - 36.0) }
    } else {
        let cy = y + h / 2.0;
        let gx = x + w * 0.66;
        Layout { label: (x + 16.0, cy + 5.0, 12.0), value: (x + 90.0, cy + 9.0, 25.0, 100.0), sub: (x + 214.0, cy + 5.0, 13.0, gx - x - 228.0), graph: (gx, y + 8.0, w * 0.34 - 14.0, h - 16.0) }
    }
}

/// A metric cell: label, big value, a line under it, and its sparkline(s).
fn metric(c: &mut Canvas, r: R, m: &Metric) {
    c.rrect(r, if r.3 < 70.0 { 10.0 } else { 12.0 }, PANEL_DEEP, 1.0);
    let l = layout(r);
    c.text(F::Bold, m.label, l.label.2, l.label.0, l.label.1, FAINT, Align::Left);
    let (vx, vy, vpx, vmax) = l.value;
    let vw = c.fit(F::Bold, &m.value, vpx, vmax, vx, vy, TEXT, Align::Left);
    if let Some((sym, col)) = m.value_sym {
        c.symbol(sym, vpx * 0.45, vx + vw + vpx * 0.4, vy - vpx * 0.33, col);
    }
    let (sx, sy, spx, smax) = l.sub;
    let sw = c.clip(F::Medium, &m.sub, spx, smax, sx, sy, MUTED);
    if let Some((sym, col)) = m.sub_sym {
        c.symbol(sym, spx, sx + sw + spx * 0.85, sy - spx * 0.36, col);
    }
    let g = l.graph;
    if m.lines.iter().all(|(v, _)| v.len() < 2) {
        c.rrect((g.0, g.1 + g.3 - 2.0, g.2, 2.0), 1.0, LINE, 1.0);
        return;
    }
    let max = m.lines.iter().flat_map(|(v, _)| v.iter().copied()).fold(m.floor, f32::max);
    for (vals, col) in &m.lines {
        sparkline(c, g, vals, max, *col);
    }
}

/// "412 GB free" over a used bar, right-aligned at `rx`; returns where it starts.
fn disk_block(c: &mut Canvas, v: &View, m: &Mac, rx: f32, y: f32) -> f32 {
    if m.disk_total == 0 {
        return rx;
    }
    let label = format!("{} free", bytes(m.disk_free));
    let tw = c.text(F::Mono, &label, 16.0, rx, y + 38.0, TEXT_2, Align::Right);
    let bw = tw.max(130.0);
    let used = 1.0 - m.disk_free as f32 / m.disk_total.max(1) as f32;
    bar(c, rx - bw, y + 52.0, bw, used, usage_color(used, v.accent));
    let lw = c.text(F::Bold, "DISK", 12.0, rx - bw - 12.0, y + 37.0, FAINT, Align::Right);
    rx - bw - 12.0 - lw
}

/// The Mac's own battery (laptops), right-aligned at `rx`; returns where it starts.
fn power_block(c: &mut Canvas, p: &crate::health::Power, rx: f32, y: f32) -> f32 {
    let col = if p.charging || p.plugged { GREEN } else { level_color(p.percent) };
    let label = format!("{}%", p.percent);
    let tw = c.text(F::Mono, &label, 16.0, rx, y + 38.0, if p.charging || p.plugged { TEXT_2 } else { level_color(p.percent) }, Align::Right);
    let ix = rx - tw - 46.0;
    battery_icon(c, ix, y + 32.0, p.percent, col);
    if p.charging {
        c.symbol("bolt.fill", 12.0, ix + 15.0, y + 32.0, 0x111114);
    }
    ix - 4.0
}

/// A small battery outline filled to `pct`, its left edge at x, centered on cy.
fn battery_icon(c: &mut Canvas, x: f32, cy: f32, pct: u8, color: u32) {
    let (w, h) = (32.0, 15.0);
    c.outline((x, cy - h / 2.0, w, h), 4.0, 0x55555E, 1.5);
    c.rrect((x + w + 1.0, cy - 3.0, 2.5, 6.0), 1.0, 0x55555E, 1.0);
    let fill = (w - 5.0) * (pct.min(100) as f32 / 100.0);
    if fill > 0.5 {
        c.rrect((x + 2.5, cy - h / 2.0 + 2.5, fill.max(3.0), h - 5.0), 2.0, color, 1.0);
    }
}

fn kind_symbol(b: &Battery) -> &'static str {
    match b.kind.as_str() {
        "keyboard" => "keyboard.fill",
        "mouse" => "magicmouse.fill",
        "trackpad" => "rectangle.and.hand.point.up.left.fill",
        "headphones" if b.name.contains("Max") => "airpods.max",
        "headphones" if b.left.is_some() || b.right.is_some() => "airpods.pro",
        "headphones" => "headphones",
        _ => "battery.75percent",
    }
}

fn battery_strip(c: &mut Canvas, list: &[&Battery]) {
    let r = strip();
    let (x, y, w, h) = r;
    c.rrect(r, 14.0, PANEL_DEEP, 1.0);
    let cy = y + h / 2.0;
    c.text(F::Bold, "BATTERIES", 12.0, x + 22.0, cy + 5.0, FAINT, Align::Left);
    let mut cx = x + 120.0;
    if list.is_empty() {
        c.text(F::Medium, "No Bluetooth keyboard, mouse, trackpad or AirPods reporting a battery", 16.0, cx, cy + 6.0, FAINT, Align::Left);
        return;
    }
    let end = x + w - 14.0;
    for (k, b) in list.iter().enumerate() {
        let parts: Vec<(&str, u8)> = [("", b.main), ("L", b.left), ("R", b.right), ("Case", b.case)].into_iter().filter_map(|(l, p)| p.map(|p| (l, p))).collect();
        let low = b.lowest().is_some_and(|p| p < LOW);
        let name_w = c.measure(F::Bold, &b.name, 16.0).min(240.0);
        let parts_w: f32 = parts.iter().map(|(l, p)| c.measure(F::Medium, l, 14.0) + c.measure(F::Mono, &format!("{p}%"), 15.0) + if l.is_empty() { 44.0 } else { 18.0 }).sum();
        let low_w = if low { 52.0 } else { 0.0 };
        let cw = 50.0 + name_w + 14.0 + parts_w + low_w + 8.0;
        if cx + cw > end {
            let more = list.len() - k;
            c.text(F::Medium, &format!("+{more}"), 15.0, end - 4.0, cy + 6.0, MUTED, Align::Right);
            break;
        }
        let chip_r = (cx, cy - 20.0, cw, 40.0);
        c.rrect(chip_r, 12.0, if low { 0x2A2210 } else { PANEL }, 1.0);
        let worst = b.lowest().unwrap_or(100);
        c.symbol(kind_symbol(b), 18.0, cx + 26.0, cy, if low { level_color(worst) } else { TEXT_2 });
        let mut px = cx + 50.0;
        c.clip(F::Bold, &b.name, 16.0, name_w, px, cy + 6.0, TEXT);
        px += name_w + 14.0;
        for (l, p) in &parts {
            if l.is_empty() {
                battery_icon(c, px, cy, *p, if *p >= LOW { GREEN } else { level_color(*p) });
                px += 44.0;
            } else {
                px += c.text(F::Medium, l, 14.0, px, cy + 5.0, FAINT, Align::Left) + 5.0;
            }
            let col = if *l == "Case" && *p >= CRITICAL { TEXT_2 } else { level_color(*p) };
            px += c.text(F::Mono, &format!("{p}%"), 15.0, px, cy + 6.0, col, Align::Left) + 13.0;
        }
        if low {
            c.pill(px - 4.0, cy - 11.0, "LOW", 11.0, level_color(worst), 0x1C1500);
        }
        cx += cw + 10.0;
    }
}

/// Touch on this page: Hit::Feature(Page::Health, i) for Mac column `i`.
pub(super) fn hit(v: &View, x: f32, y: f32) -> Option<Hit> {
    let n = v.health.macs.len();
    let (first, cols) = window(n, v.tile);
    (0..cols.min(n)).find(|&s| inside(col_rect(s, cols), x, y)).map(|s| Hit::Feature(Page::Health, first + s))
}

/// Where Hit::Feature(_, i) is, for the tap flash.
pub(super) fn rect(v: &View, i: usize) -> Option<R> {
    let (first, cols) = window(v.health.macs.len(), v.tile);
    let slot = i.checked_sub(first).filter(|&s| s < cols && i < v.health.macs.len())?;
    Some(col_rect(slot, cols))
}

pub(super) fn hints(level: &Level) -> &'static str {
    match level {
        Level::Top => "turn  switch page   ·   press  pick a Mac   ·   tap  Activity Monitor",
        _ => "turn  choose a Mac   ·   press  Activity Monitor   ·   double-click  back",
    }
}

/// The breadcrumb next to the tabs.
pub(super) fn crumb(v: &View) -> Option<String> {
    match v.level {
        Level::Top => None,
        _ => v.health.macs.get(v.tile).map(|m| format!("›  {}", m.name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_scroll_with_the_knob() {
        assert_eq!(window(0, 0), (0, 1));
        assert_eq!(window(2, 1), (0, 2));
        assert_eq!(window(5, 0), (0, 3));
        assert_eq!(window(5, 3), (1, 3));
        assert_eq!(window(5, 4), (2, 3));
        assert_eq!(window(5, 9), (2, 3));
    }

    #[test]
    fn formatting() {
        assert_eq!(rate(0.0), "0 B/s");
        assert_eq!(rate(318_000.0), "311 KB/s");
        assert_eq!(rate(2_480_000.0), "2.4 MB/s");
        assert_eq!(uptime(3 * 86_400 + 6 * 3600 + 120), "3d 6h");
        assert_eq!(uptime(2 * 3600 + 5 * 60), "2h 5m");
        assert_eq!(uptime(59), "0m");
    }
}
