//! The comparator: the AA-aware per-pixel perceptual diff (Mapbox pixelmatch
//! port), its YIQ colour primitives, the sub-pixel-shift tolerance, and the
//! PASS/PARTIAL/FAIL classifier.
//!
//! Extracted verbatim from the former monolithic `mod.rs` (C1 mechanical split).

use image::{ImageBuffer, Rgba, RgbaImage};

use super::config::{PM_MAX_DELTA, PM_THRESHOLD};
use super::report::Status;

/// Structural diff over two ALREADY-cropped, same-size images using the
/// `image-compare` crate's SSIM **hybrid** comparison (MSSIM on luma + RMS on the
/// U/V chroma and alpha channels, combined per-pixel by minimum similarity). This
/// is a perceptual, windowed, anti-aliasing-robust structural metric: a 1px AA
/// ramp on a shared boundary barely perturbs the local SSIM window, while a border
/// present in only one image, a shifted line, or a recoloured region produce clear
/// structural/colour deviations. It supersedes the hand-rolled pixelmatch port.
///
/// SCORE DIRECTION (verified empirically against image-compare 0.5: identical =>
/// `score == 1.0`, all-black vs all-white => `score ~= 0.0`): `Similarity.score`
/// is a SIMILARITY in [0,1] where 1.0 = identical. We map it to a DISSIMILARITY
/// percentage `100 * (1 - score)` so that identical => 0% and maximally different
/// => ~100%, plugging into `classify()`/the thresholds unchanged in meaning
/// (lower is better). The denominator is the whole union-bbox region (the crate
/// averages over every pixel), preserving the union-bbox contract.
///
/// The overlay is SCORE-FAITHFUL: it is `Similarity.image.to_color_map()` — the
/// structural+colour difference map that PRODUCES the score, where all-black means
/// no difference and brighter pixels mark the structural (red) and chroma
/// (green/blue) deviations that drove the dissimilarity. It is returned as an
/// `RgbaImage` so the committed diff PNG visualises exactly what the score saw.
///
/// Perceptual image diff = fraction of pixels that genuinely differ, ignoring
/// anti-aliasing edges and sub-threshold noise (Mapbox pixelmatch algorithm).
///
/// Replaces global MSSIM, which had two opposite failure modes on this suite:
/// it was too LENIENT on small hard differences (an unrounded per-corner radius
/// is a tiny pixel fraction, so MSSIM diluted it into a PASS) and too HARSH on
/// large smooth differences (a visually-identical gradient scored ~10%). A
/// thresholded, AA-aware per-pixel diff is harsh on solid wrong regions and
/// lenient on smooth near-matches and glyph anti-aliasing — so a perfect render
/// scores ~0% and the noise floor can sit low enough to catch real defects.
pub(crate) fn diff_images(a: &RgbaImage, b: &RgbaImage) -> (f64, RgbaImage) {
    let (w, h) = a.dimensions();
    if w == 0 || h == 0 || b.dimensions() != (w, h) {
        return (100.0, ImageBuffer::from_pixel(1, 1, Rgba([255, 255, 255, 255])));
    }
    let max_delta = PM_MAX_DELTA * PM_THRESHOLD * PM_THRESHOLD;
    let mut overlay = ImageBuffer::from_pixel(w, h, Rgba([255u8, 255, 255, 255]));
    let mut diff_count: u64 = 0;
    for y in 0..h {
        for x in 0..w {
            let pa = a.get_pixel(x, y);
            let pb = b.get_pixel(x, y);
            let delta = color_delta(pa, pb, false);
            if delta.abs() > max_delta {
                // Significant difference — unless it is an anti-aliasing artifact
                // present in either image (glyph/shape edges differ sub-pixel
                // between rasterizers but are not real layout differences).
                if is_antialiased(a, x, y, w, h, b) || is_antialiased(b, x, y, w, h, a) {
                    overlay.put_pixel(x, y, Rgba([255, 224, 0, 255])); // AA edge: yellow
                } else if pixel_has_close_match(a, pb, x, y, w, h, max_delta)
                    && pixel_has_close_match(b, pa, x, y, w, h, max_delta)
                {
                    // SUB-PIXEL SHIFT TOLERANCE: the reference colour at (x,y)
                    // exists within 1px in the candidate AND vice-versa. That is
                    // a ≤1px local displacement — exactly the residual left by
                    // two different rasterizers placing the same glyph/edge a
                    // fraction of a pixel apart (the is_antialiased test misses
                    // the case where a boundary pixel is solid-ink in one image
                    // and solid-paper in the other). A genuine layout error
                    // (missing/extra content, recolour, or a shift larger than
                    // the global registration already cancels) has NO such local
                    // match and still counts. Bounded to radius 1 so it can only
                    // forgive sub-pixel noise, never a real ≥2px difference.
                    overlay.put_pixel(x, y, Rgba([0, 200, 255, 255])); // shift: cyan
                } else {
                    diff_count += 1;
                    overlay.put_pixel(x, y, Rgba([255, 40, 40, 255])); // real diff: red
                }
            } else {
                // Matched: faint grayscale of the reference so the overlay stays
                // readable (shows the shape behind the highlighted differences).
                let yv = rgb2y(pb);
                let g = (255.0 - (255.0 - yv) * 0.12).round().clamp(0.0, 255.0) as u8;
                overlay.put_pixel(x, y, Rgba([g, g, g, 255]));
            }
        }
    }
    let pct = (100.0 * diff_count as f64 / (w as f64 * h as f64)).clamp(0.0, 100.0);
    (pct, overlay)
}

#[inline]
pub(crate) fn rgb2y(p: &Rgba<u8>) -> f64 {
    p[0] as f64 * 0.298_895_31 + p[1] as f64 * 0.586_622_47 + p[2] as f64 * 0.114_482_23
}
#[inline]
pub(crate) fn rgb2i(p: &Rgba<u8>) -> f64 {
    p[0] as f64 * 0.595_977_99 - p[1] as f64 * 0.274_176_10 - p[2] as f64 * 0.321_801_89
}
#[inline]
pub(crate) fn rgb2q(p: &Rgba<u8>) -> f64 {
    p[0] as f64 * 0.211_470_17 - p[1] as f64 * 0.522_617_11 + p[2] as f64 * 0.311_146_94
}

/// Signed YIQ perceptual delta between two pixels (pixelmatch `colorDelta`).
/// `y_only` returns just the brightness delta (used for AA edge detection).
/// Sign encodes which pixel is brighter; callers use the magnitude for the
/// threshold and the sign for anti-aliasing classification.
pub(crate) fn color_delta(a: &Rgba<u8>, b: &Rgba<u8>, y_only: bool) -> f64 {
    if a == b {
        return 0.0;
    }
    let y1 = rgb2y(a);
    let y2 = rgb2y(b);
    if y_only {
        return y1 - y2;
    }
    let dy = y1 - y2;
    let di = rgb2i(a) - rgb2i(b);
    let dq = rgb2q(a) - rgb2q(b);
    let delta = 0.5053 * dy * dy + 0.299 * di * di + 0.1957 * dq * dq;
    if y1 > y2 { -delta } else { delta }
}

/// Whether the pixel at (x1,y1) in `img` looks like anti-aliasing rather than a
/// real difference (pixelmatch `antialiased`): it sits on a high-contrast edge
/// whose extreme neighbor also has many equal siblings in both images.
pub(crate) fn is_antialiased(img: &RgbaImage, x1: u32, y1: u32, w: u32, h: u32, img2: &RgbaImage) -> bool {
    let x0 = x1.saturating_sub(1);
    let y0 = y1.saturating_sub(1);
    let x2 = (x1 + 1).min(w - 1);
    let y2 = (y1 + 1).min(h - 1);
    let mut zeroes = u32::from(x1 == x0 || x1 == x2 || y1 == y0 || y1 == y2);
    let center = img.get_pixel(x1, y1);
    let mut min = 0.0f64;
    let mut max = 0.0f64;
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (0u32, 0u32, 0u32, 0u32);
    for y in y0..=y2 {
        for x in x0..=x2 {
            if x == x1 && y == y1 {
                continue;
            }
            let delta = color_delta(center, img.get_pixel(x, y), true);
            if delta == 0.0 {
                zeroes += 1;
                if zeroes > 2 {
                    return false;
                }
            } else if delta < min {
                min = delta;
                min_x = x;
                min_y = y;
            } else if delta > max {
                max = delta;
                max_x = x;
                max_y = y;
            }
        }
    }
    if min == 0.0 || max == 0.0 {
        return false;
    }
    (has_many_siblings(img, min_x, min_y, w, h) && has_many_siblings(img2, min_x, min_y, w, h))
        || (has_many_siblings(img, max_x, max_y, w, h) && has_many_siblings(img2, max_x, max_y, w, h))
}

/// Whether `target` colour appears (within `max_delta`) anywhere in the 3x3
/// neighbourhood of `img` centred at (x,y). Used for the sub-pixel-shift
/// tolerance: a differing pixel is forgiven only when each side's colour has a
/// near-match within 1px on the other side (a ≤1px local displacement, i.e.
/// rasterizer glyph/edge noise — not a real layout difference).
pub(crate) fn pixel_has_close_match(
    img: &RgbaImage,
    target: &Rgba<u8>,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    max_delta: f64,
) -> bool {
    let x0 = x.saturating_sub(1);
    let y0 = y.saturating_sub(1);
    let x2 = (x + 1).min(w - 1);
    let y2 = (y + 1).min(h - 1);
    for yy in y0..=y2 {
        for xx in x0..=x2 {
            if color_delta(target, img.get_pixel(xx, yy), false).abs() <= max_delta {
                return true;
            }
        }
    }
    false
}

/// Whether a pixel has 3+ equal adjacent neighbors (pixelmatch `hasManySiblings`).
pub(crate) fn has_many_siblings(img: &RgbaImage, x1: u32, y1: u32, w: u32, h: u32) -> bool {
    let x0 = x1.saturating_sub(1);
    let y0 = y1.saturating_sub(1);
    let x2 = (x1 + 1).min(w - 1);
    let y2 = (y1 + 1).min(h - 1);
    let mut zeroes = u32::from(x1 == x0 || x1 == x2 || y1 == y0 || y1 == y2);
    let center = img.get_pixel(x1, y1);
    for y in y0..=y2 {
        for x in x0..=x2 {
            if x == x1 && y == y1 {
                continue;
            }
            if img.get_pixel(x, y) == center {
                zeroes += 1;
                if zeroes > 2 {
                    return true;
                }
            }
        }
    }
    false
}

pub(crate) fn classify(diff_pct: f64, pass: f64, partial: f64) -> Status {
    if diff_pct <= pass {
        Status::Pass
    } else if diff_pct <= partial {
        Status::Partial
    } else {
        Status::Fail
    }
}
