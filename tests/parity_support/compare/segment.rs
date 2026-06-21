//! Region segmentation (spec §1.8): a hand-rolled 4-connectivity flood fill over
//! all real-diff pixels (every class except `Match`/`AaEdge`), each labelled with
//! its dominant class, magnitude, and a per-region translation diagnosis.
//!
//! The shift diagnosis (`is_translation`/`shift_css`) runs AFTER the verdict
//! pixels are counted, so it can describe a displacement but can never reduce the
//! score.

use image::RgbaImage;

use super::super::config::{CSS_PX, REGION_MIN_AREA_PX, RESIDUAL_JITTER_PX};
use super::classify::{ClassMap, PixelClass};
use super::color::{ciede2000, srgb_to_lab};

/// One connected blob of real-diff pixels with its diagnosis. Several fields
/// (`bbox_css`, `fill_ratio`, `modal_drgb`) are populated now but consumed by the
/// C4 diagnosis and C5 region-table/overlay — kept on the struct so the pipeline
/// is complete and the goldens lock the full shape.
#[allow(dead_code)]
pub(crate) struct DiffRegion {
    /// Bounding box in CSS px [x0, y0, x1, y1], relative to the union crop origin.
    pub(crate) bbox_css: [f64; 4],
    pub(crate) dominant: PixelClass,
    pub(crate) area_px: u32,
    pub(crate) area_pct: f64,
    /// `area_px / bbox_area` — discriminates a thin line from a solid blob.
    pub(crate) fill_ratio: f64,
    /// Median (cand − ref) over the region's `ColorErr` pixels (0..255 signed).
    pub(crate) modal_drgb: [i16; 3],
    /// Mean CIEDE2000 over the region's `ColorErr` pixels.
    pub(crate) delta_e: f64,
    /// Dominant translation (CSS px) for `GeomShift` regions.
    pub(crate) shift_css: (f64, f64),
    /// Whether a single consistent shift > `RESIDUAL_JITTER_PX` explains the region.
    pub(crate) is_translation: bool,
}

#[inline]
fn is_real_diff(c: PixelClass) -> bool {
    !matches!(c, PixelClass::Match | PixelClass::AaEdge)
}

/// Flood-fill the real-diff pixels into 4-connected regions, drop specks, and
/// diagnose each survivor.
pub(crate) fn segment(cm: &ClassMap, cand: &RgbaImage, reference: &RgbaImage) -> Vec<DiffRegion> {
    let w = cm.w as usize;
    let h = cm.h as usize;
    let total = w * h;
    if total == 0 {
        return Vec::new();
    }
    let total_px = total as f64;

    let mut visited = vec![false; total];
    let mut regions: Vec<DiffRegion> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();

    for start in 0..total {
        if visited[start] || !is_real_diff(cm.px[start]) {
            continue;
        }
        // BFS/DFS flood fill collecting the connected blob's pixel indices.
        stack.clear();
        stack.push(start);
        visited[start] = true;
        let mut members: Vec<usize> = Vec::new();
        while let Some(i) = stack.pop() {
            members.push(i);
            let x = i % w;
            let y = i / w;
            let push = |nx: usize, ny: usize, stack: &mut Vec<usize>, visited: &mut [bool]| {
                let ni = ny * w + nx;
                if !visited[ni] && is_real_diff(cm.px[ni]) {
                    visited[ni] = true;
                    stack.push(ni);
                }
            };
            if x > 0 {
                push(x - 1, y, &mut stack, &mut visited);
            }
            if x + 1 < w {
                push(x + 1, y, &mut stack, &mut visited);
            }
            if y > 0 {
                push(x, y - 1, &mut stack, &mut visited);
            }
            if y + 1 < h {
                push(x, y + 1, &mut stack, &mut visited);
            }
        }

        if (members.len() as u32) < REGION_MIN_AREA_PX {
            continue;
        }

        regions.push(diagnose_region(&members, cm, cand, reference, w, total_px));
    }

    // Worst-first by area for stable, useful ordering downstream.
    regions.sort_by(|a, b| b.area_pct.partial_cmp(&a.area_pct).unwrap_or(std::cmp::Ordering::Equal));
    regions
}

fn diagnose_region(
    members: &[usize],
    cm: &ClassMap,
    cand: &RgbaImage,
    reference: &RgbaImage,
    w: usize,
    total_px: f64,
) -> DiffRegion {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    let mut counts = [0u32; 6];
    // Per-channel deltas + ΔE accumulated over the region's ColorErr pixels.
    let mut dr: Vec<i16> = Vec::new();
    let mut dg: Vec<i16> = Vec::new();
    let mut db: Vec<i16> = Vec::new();
    let mut de_sum = 0.0;
    let mut de_n = 0u32;
    // Shift estimate: average (ref-position - matching-cand-position) is implicit;
    // we instead accumulate the per-pixel displacement that best matched.
    let mut shift_dx_sum = 0.0;
    let mut shift_dy_sum = 0.0;
    let mut shift_n = 0u32;

    for &i in members {
        let x = i % w;
        let y = i / w;
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
        let cls = cm.px[i];
        counts[class_index(cls)] += 1;

        let c = cand.get_pixel(x as u32, y as u32).0;
        let r = reference.get_pixel(x as u32, y as u32).0;

        if cls == PixelClass::ColorErr {
            dr.push(c[0] as i16 - r[0] as i16);
            dg.push(c[1] as i16 - r[1] as i16);
            db.push(c[2] as i16 - r[2] as i16);
            de_sum += ciede2000(srgb_to_lab([r[0], r[1], r[2]]), srgb_to_lab([c[0], c[1], c[2]]));
            de_n += 1;
        }
        if cls == PixelClass::GeomShift {
            if let Some((sx, sy)) = best_local_shift(cand, reference, x as u32, y as u32) {
                shift_dx_sum += sx as f64;
                shift_dy_sum += sy as f64;
                shift_n += 1;
            }
        }
    }

    let dominant = dominant_class(&counts);
    let area_px = members.len() as u32;
    let bbox_w = (x1 - x0 + 1) as f64;
    let bbox_h = (y1 - y0 + 1) as f64;
    let fill_ratio = area_px as f64 / (bbox_w * bbox_h);

    let modal_drgb = [median(&mut dr), median(&mut dg), median(&mut db)];
    let delta_e = if de_n > 0 { de_sum / de_n as f64 } else { 0.0 };

    // Phase-correlation lite: the average matched local displacement. A single
    // dominant shift > residual jitter marks a translation. For pure 1px boundary
    // noise the average displacement stays at/below the residual band.
    let (shift_x, shift_y) = if shift_n > 0 {
        (shift_dx_sum / shift_n as f64, shift_dy_sum / shift_n as f64)
    } else {
        (0.0, 0.0)
    };
    let shift_mag = (shift_x * shift_x + shift_y * shift_y).sqrt();
    let is_translation = shift_mag > RESIDUAL_JITTER_PX as f64;
    let shift_css = (shift_x / CSS_PX, shift_y / CSS_PX);

    DiffRegion {
        bbox_css: [
            x0 as f64 / CSS_PX,
            y0 as f64 / CSS_PX,
            x1 as f64 / CSS_PX,
            y1 as f64 / CSS_PX,
        ],
        dominant,
        area_px,
        area_pct: 100.0 * area_px as f64 / total_px,
        fill_ratio,
        modal_drgb,
        delta_e,
        shift_css,
        is_translation,
    }
}

/// The integer displacement (dx,dy) within ±RESIDUAL that makes the reference at
/// `(x,y)` reappear in the candidate (i.e. how far the candidate boundary moved).
fn best_local_shift(cand: &RgbaImage, reference: &RgbaImage, x: u32, y: u32) -> Option<(i32, i32)> {
    let (w, h) = cand.dimensions();
    let r = reference.get_pixel(x, y);
    let mut best: Option<(i32, i32, f64)> = None;
    let radius = RESIDUAL_JITTER_PX;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            if nx < 0 || ny < 0 || nx as u32 >= w || ny as u32 >= h {
                continue;
            }
            let d = super::color_delta(r, cand.get_pixel(nx as u32, ny as u32), false).abs();
            match best {
                Some((_, _, bd)) if d >= bd => {}
                _ => best = Some((dx, dy, d)),
            }
        }
    }
    best.map(|(dx, dy, _)| (dx, dy))
}

#[inline]
fn class_index(c: PixelClass) -> usize {
    match c {
        PixelClass::Match => 0,
        PixelClass::AaEdge => 1,
        PixelClass::ColorErr => 2,
        PixelClass::GeomShift => 3,
        PixelClass::Missing => 4,
        PixelClass::Extra => 5,
    }
}

/// The dominant real-diff class in a region (Match/AaEdge never dominate).
fn dominant_class(counts: &[u32; 6]) -> PixelClass {
    let candidates = [
        (PixelClass::ColorErr, counts[2]),
        (PixelClass::GeomShift, counts[3]),
        (PixelClass::Missing, counts[4]),
        (PixelClass::Extra, counts[5]),
    ];
    candidates
        .iter()
        .max_by_key(|(_, n)| *n)
        .map(|(c, _)| *c)
        .unwrap_or(PixelClass::ColorErr)
}

/// Median of a signed-channel sample, clamped to i16. Empty -> 0.
fn median(v: &mut [i16]) -> i16 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[v.len() / 2]
}
