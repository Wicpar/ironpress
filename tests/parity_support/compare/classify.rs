//! Per-pixel classification (spec §1.7) — assigns every union-cropped pixel a
//! `PixelClass` in a fixed precedence order, first match wins.
//!
//! This is where AA forgiveness is bounded: a differing pixel becomes `AaEdge`
//! ONLY inside the shared edge band (and within the wider `t_aa()` budget). Off
//! the shared band there is no AA mercy, so a wrong glyph/weight/recolour cannot
//! launder itself as anti-aliasing. A ≤1px boundary displacement becomes
//! `GeomShift` (counted, never zeroed); a genuine recolour becomes `ColorErr`.

use image::RgbaImage;

use super::super::config::{t_aa, t_match, COLOR_DE_PASS, RESIDUAL_JITTER_PX};
use super::super::geom::Mask;
use super::color::{ciede2000, srgb_to_lab};
use super::color_delta;
use super::masks::StructuralMasks;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PixelClass {
    /// Below the match budget — effectively identical.
    Match,
    /// Differing but on a shared structural edge within the AA budget — genuine
    /// cross-rasterizer glyph/edge anti-aliasing, never a defect.
    AaEdge,
    /// Both ink, aligned, colour differs beyond match with no 1px match — recolour.
    ColorErr,
    /// Both ink, differing, but the reference colour reappears within 1px in both
    /// directions — a boundary displaced ≤1px (counted, not zeroed).
    GeomShift,
    /// Reference paints, candidate is paper-white.
    Missing,
    /// Candidate paints, reference is paper-white.
    Extra,
}

/// A per-pixel class grid over the union-cropped frame (row-major).
pub(crate) struct ClassMap {
    pub(crate) w: u32,
    pub(crate) h: u32,
    pub(crate) px: Vec<PixelClass>,
}

/// Whether `target` colour reappears within `radius` of `(x,y)` in `img`, within
/// the match budget. Used for the ±1px `GeomShift` test (a real recolour or a
/// >1px shift has no such local match and is NOT forgiven).
fn color_present_near(img: &RgbaImage, target: &image::Rgba<u8>, x: u32, y: u32, radius: i32, budget: f64) -> bool {
    let (w, h) = img.dimensions();
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            if nx < 0 || ny < 0 || nx as u32 >= w || ny as u32 >= h {
                continue;
            }
            if color_delta(target, img.get_pixel(nx as u32, ny as u32), false).abs() <= budget {
                return true;
            }
        }
    }
    false
}

/// Classify every pixel in the aligned cand/ref frame (spec §1.7 precedence).
pub(crate) fn classify_pixels(
    cand: &RgbaImage,
    reference: &RgbaImage,
    mask_c: &Mask,
    mask_r: &Mask,
    masks: &StructuralMasks,
) -> ClassMap {
    let (w, h) = cand.dimensions();
    let mut px = Vec::with_capacity((w as usize) * (h as usize));
    let tm = t_match();
    let ta = t_aa();
    for y in 0..h {
        for x in 0..w {
            let c = cand.get_pixel(x, y);
            let r = reference.get_pixel(x, y);
            let d = color_delta(c, r, false).abs();
            let ink_c = mask_c.get(x, y);
            let ink_r = mask_r.get(x, y);

            // The YIQ `t_match` budget is COARSE: a perceptible recolour like
            // #cc0000 vs #dd0000 reads only ~46 YIQ (well under t_match ~352) yet
            // is ΔE2000 ~3.6 — above the JND. YIQ alone would launder it as Match,
            // defeating the whole point of the ΔE colour detector. So for two INK
            // pixels we additionally require the perceptual ΔE to be within the
            // JND (`COLOR_DE_PASS`) before calling it a match. (Paper-white vs
            // paper-white, or AA ramps, are handled by the YIQ budget and the
            // shared-band gate respectively — this only tightens ink-vs-ink.)
            let perceptual_match = if ink_c && ink_r {
                d <= tm
                    && ciede2000(
                        srgb_to_lab([r[0], r[1], r[2]]),
                        srgb_to_lab([c[0], c[1], c[2]]),
                    ) <= COLOR_DE_PASS
            } else {
                d <= tm
            };

            let class = if perceptual_match {
                // 1. Match (YIQ within budget AND, for ink pixels, ΔE within JND).
                PixelClass::Match
            } else if d <= tm {
                // YIQ matched but ΔE exceeded the JND on two ink pixels: a genuine
                // sub-YIQ recolour (not anti-aliasing — AA is intermediate values
                // on a contrast edge, which this is not). Score it as ColorErr.
                PixelClass::ColorErr
            } else if masks.in_shared_band(x, y) && d <= ta {
                // 2. AaEdge — only inside the shared edge band.
                PixelClass::AaEdge
            } else if ink_r && !ink_c {
                // 3. Missing — reference paints, candidate is paper-white.
                PixelClass::Missing
            } else if ink_c && !ink_r {
                // 4. Extra — candidate paints, reference is paper-white.
                PixelClass::Extra
            } else if ink_c
                && ink_r
                && color_present_near(cand, r, x, y, RESIDUAL_JITTER_PX, tm)
                && color_present_near(reference, c, x, y, RESIDUAL_JITTER_PX, tm)
            {
                // 5. GeomShift — boundary displaced ≤1px (both ink, mutual match).
                PixelClass::GeomShift
            } else {
                // 6. ColorErr — aligned recolour / wrong-value / colour-space.
                PixelClass::ColorErr
            };
            px.push(class);
        }
    }
    ClassMap { w, h, px }
}
