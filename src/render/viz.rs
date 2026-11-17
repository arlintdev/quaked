//! Music visualizer on the board, drawn behind the Music page: a soft spectrum "lens" in the
//! page color, mirrored top/bottom around the progress bar and left/right around the bass
//! (so it breathes from the middle on every kick), low enough in alpha that the art, title
//! and controls stay readable.
use super::*;
use tiny_skia::{GradientStop, LinearGradient, Point, Shader, SpreadMode};

/// Fill alpha at the spine (outer body, inner core) and of the peak outline.
const BODY_ALPHA: f32 = 0.12;
const CORE_ALPHA: f32 = 0.07;
const PEAK_ALPHA: f32 = 0.10;
/// A beat brightens it by up to this much.
const BEAT_LIFT: f32 = 0.3;
/// Heights are band^GAMMA: dense mixes keep a shape instead of filling the whole height.
const GAMMA: f32 = 1.5;
/// The tallest band reaches this share of the half-height.
const REACH: f32 = 0.88;

/// Drawn first on the Music page, behind the art and controls. `area` is the content area.
pub(super) fn draw_behind(c: &mut Canvas, v: &View, area: R) {
    draw(c, v.viz, v.accent, area);
}

fn draw(c: &mut Canvas, st: &crate::viz::Status, accent: u32, area: R) {
    if st.bands.len() < 2 || st.bands.iter().all(|&b| b <= 0.0) {
        return; // not listening, or silence: nothing (no fake animation)
    }
    // From just right of the art to the right edge (the art covers the rest), mirrored around
    // the middle of the content (where the progress bar runs).
    let (ax, _, aw, _) = ART;
    let (x0, x1) = (ax + aw + 6.0, area.0 + area.2);
    let mid = area.1 + area.3 * 0.5;
    let half = area.3 * 0.5 - 4.0;
    let glow = 1.0 + BEAT_LIFT * st.beat.clamp(0.0, 1.0);

    let bands = soften(&st.bands, 3);
    let body = points(&bands, x0, x1, half * REACH);
    fill_mirrored(c, &body, mid, half, accent, BODY_ALPHA * glow);
    let core: Vec<(f32, f32)> = body.iter().map(|&(x, h)| (x, h * 0.55)).collect();
    fill_mirrored(c, &core, mid, half * 0.55, accent, CORE_ALPHA * glow);
    if st.peaks.len() == st.bands.len() {
        let peaks = points(&soften(&st.peaks, 3), x0, x1, half * REACH);
        let top: Vec<(f32, f32)> = peaks.iter().map(|&(x, h)| (x, mid - h)).collect();
        let bottom: Vec<(f32, f32)> = peaks.iter().map(|&(x, h)| (x, mid + h)).collect();
        stroke(c, &top, accent, PEAK_ALPHA, 1.5);
        stroke(c, &bottom, accent, PEAK_ALPHA, 1.5);
    }
}

/// A light spatial blur ([1 2 1] / 4, `passes` times) so the curve reads as one smooth shape,
/// not a row of spikes.
fn soften(v: &[f32], passes: usize) -> Vec<f32> {
    let mut v = v.to_vec();
    let n = v.len();
    for _ in 0..passes {
        v = (0..n).map(|i| (v[i.saturating_sub(1)] + 2.0 * v[i] + v[(i + 1).min(n - 1)]) * 0.25).collect();
    }
    v
}

/// (x, height) across the span: the lowest band in the middle, the highest at both ends,
/// tapering to a point at the ends.
fn points(vals: &[f32], x0: f32, x1: f32, half: f32) -> Vec<(f32, f32)> {
    let n = vals.len();
    let at = |d: f32| {
        let pos = d * (n - 1) as f32;
        let (i, k) = (pos.floor() as usize, pos.fract());
        let (a, b) = (vals[i.min(n - 1)], vals[(i + 1).min(n - 1)]);
        (a + (b - a) * k).clamp(0.0, 1.0).powf(GAMMA)
    };
    let m = 2 * n - 1;
    (0..m)
        .map(|j| {
            let f = j as f32 / (m - 1) as f32;
            let d = (f - 0.5).abs() * 2.0; // 0 in the middle, 1 at the ends
            let taper = ((1.0 - d) / 0.08).clamp(0.0, 1.0);
            (x0 + (x1 - x0) * f, at(d) * half * taper)
        })
        .collect()
}

/// A smooth (Catmull-Rom) path through `pts` (already in design units), scaled by `s`.
fn smooth_path(pb: &mut PathBuilder, pts: &[(f32, f32)], s: f32, start: bool) {
    let p = |i: isize| {
        let i = i.clamp(0, pts.len() as isize - 1) as usize;
        (pts[i].0 * s, pts[i].1 * s)
    };
    if start {
        pb.move_to(p(0).0, p(0).1);
    } else {
        pb.line_to(p(0).0, p(0).1);
    }
    for i in 0..pts.len() as isize - 1 {
        let (a, b, cc, d) = (p(i - 1), p(i), p(i + 1), p(i + 2));
        let c1 = (b.0 + (cc.0 - a.0) / 6.0, b.1 + (cc.1 - a.1) / 6.0);
        let c2 = (cc.0 - (d.0 - b.0) / 6.0, cc.1 - (d.1 - b.1) / 6.0);
        pb.cubic_to(c1.0, c1.1, c2.0, c2.1, cc.0, cc.1);
    }
}

fn with_alpha(color: u32, a: f32) -> Color {
    let mut k = rgb(color);
    k.set_alpha(a.clamp(0.0, 1.0));
    k
}

/// The shape above and below `mid`, strongest at the spine and fading toward its edges.
fn fill_mirrored(c: &mut Canvas, pts: &[(f32, f32)], mid: f32, half: f32, color: u32, alpha: f32) {
    let s = c.scale;
    let top: Vec<(f32, f32)> = pts.iter().map(|&(x, h)| (x, mid - h)).collect();
    let bottom: Vec<(f32, f32)> = pts.iter().rev().map(|&(x, h)| (x, mid + h)).collect();
    let mut pb = PathBuilder::new();
    smooth_path(&mut pb, &top, s, true);
    smooth_path(&mut pb, &bottom, s, false);
    pb.close();
    let Some(path) = pb.finish() else { return };
    let stops = vec![
        GradientStop::new(0.0, with_alpha(color, alpha * 0.15)),
        GradientStop::new(0.5, with_alpha(color, alpha)),
        GradientStop::new(1.0, with_alpha(color, alpha * 0.15)),
    ];
    let (from, to) = (Point::from_xy(0.0, (mid - half) * s), Point::from_xy(0.0, (mid + half) * s));
    let Some(shader) = LinearGradient::new(from, to, stops, SpreadMode::Pad, Transform::identity()) else { return };
    let paint = Paint { shader, anti_alias: true, ..Default::default() };
    c.pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
}

fn stroke(c: &mut Canvas, pts: &[(f32, f32)], color: u32, alpha: f32, width: f32) {
    let s = c.scale;
    let mut pb = PathBuilder::new();
    smooth_path(&mut pb, pts, s, true);
    let Some(path) = pb.finish() else { return };
    let paint = Paint { shader: Shader::SolidColor(with_alpha(color, alpha)), anti_alias: true, ..Default::default() };
    let st = Stroke { width: width * s, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
    c.pixmap.stroke_path(&path, &paint, &st, Transform::identity(), None);
}
