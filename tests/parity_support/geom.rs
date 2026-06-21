//! Raster geometry: content detection, bounding boxes, union/crop, the clamped
//! small-offset registration search, and image translation.
//!
//! Extracted verbatim from the former monolithic `mod.rs` (C1 mechanical split).

use image::{ImageBuffer, Rgba, RgbaImage};

use super::config::{MAX_REG, WHITE_TOL};

pub(crate) fn is_content(px: &Rgba<u8>) -> bool {
    let [r, g, b, _] = px.0;
    let dr = (r as i32 - 255).abs();
    let dg = (g as i32 - 255).abs();
    let db = (b as i32 - 255).abs();
    dr.max(dg).max(db) > WHITE_TOL
}

/// Inclusive content bounding box `(min_x, min_y, max_x, max_y)` in the image's
/// own (== shared page) pixel coordinates, or `None` if the image is entirely
/// white (no content pixels). Coordinates are NOT re-anchored, so a box at the
/// same page position in two images yields the same numbers -> positional
/// offsets survive into the union/diff.
pub(crate) type BBox = (u32, u32, u32, u32);

pub(crate) fn content_bbox(img: &RgbaImage) -> Option<BBox> {
    let (w, h) = img.dimensions();
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (u32::MAX, u32::MAX, 0u32, 0u32);
    let mut found = false;
    for y in 0..h {
        for x in 0..w {
            if is_content(img.get_pixel(x, y)) {
                found = true;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    if found {
        Some((min_x, min_y, max_x, max_y))
    } else {
        None
    }
}

/// Pick the integer translation `(dx, dy)` within `±MAX_REG` that best aligns the
/// candidate onto the reference, by minimizing a cheap per-pixel colour
/// difference over the reference's content region. This cancels the universal
/// page-origin offset (and 1–2px cross-rasterizer edge rounding) far more
/// accurately than the single bbox-corner estimate, which systematically left a
/// thin mismatch frame around content that was actually identical.
///
/// Safety: the search is bounded by `±MAX_REG`, so a genuine layout shift larger
/// than that window still leaves a residual and is NOT masked (same guarantee as
/// the corner-based offset). Sampled on a stride for speed — the chosen offset
/// then drives the full perceptual diff. `dx, dy` follow `shift_image`'s
/// convention (candidate pixel `(x, y)` moves to `(x + dx, y + dy)`), so the
/// candidate pixel landing at reference position `(x, y)` is `(x - dx, y - dy)`.
pub(crate) fn best_registration_offset(cand: &RgbaImage, reference: &RgbaImage, rb: BBox) -> (i32, i32) {
    const STRIDE: u32 = 3;
    const COLOR_DELTA: i32 = 60;
    let (cw, ch) = cand.dimensions();
    let (min_x, min_y, max_x, max_y) = rb;
    let mut best_cost = i64::MAX;
    let mut best = (0, 0);
    let mut best_mag = i32::MAX;
    for dy in -MAX_REG..=MAX_REG {
        for dx in -MAX_REG..=MAX_REG {
            let mut cost: i64 = 0;
            let mut y = min_y;
            while y <= max_y {
                let mut x = min_x;
                while x <= max_x {
                    let rp = reference.get_pixel(x, y).0;
                    let sx = x as i32 - dx;
                    let sy = y as i32 - dy;
                    let cp = if sx >= 0 && sy >= 0 && (sx as u32) < cw && (sy as u32) < ch {
                        cand.get_pixel(sx as u32, sy as u32).0
                    } else {
                        [255, 255, 255, 255]
                    };
                    let d = (rp[0] as i32 - cp[0] as i32).abs()
                        + (rp[1] as i32 - cp[1] as i32).abs()
                        + (rp[2] as i32 - cp[2] as i32).abs();
                    if d > COLOR_DELTA {
                        cost += 1;
                    }
                    x += STRIDE;
                }
                y += STRIDE;
            }
            // On ties prefer the smallest-magnitude offset so equal-cost
            // alignments don't introduce a spurious shift.
            let mag = dx * dx + dy * dy;
            if cost < best_cost || (cost == best_cost && mag < best_mag) {
                best_cost = cost;
                best_mag = mag;
                best = (dx, dy);
            }
        }
    }
    best
}

/// Translate `img` by `(dx, dy)` pixels on a white background (same dimensions),
/// so registered content lands at the reference's page position before cropping.
/// Out-of-frame source pixels become white; this is only ever called with the
/// small clamped registration offset, so at most `MAX_REG` px is lost per edge.
pub(crate) fn shift_image(img: &RgbaImage, dx: i32, dy: i32) -> RgbaImage {
    if dx == 0 && dy == 0 {
        return img.clone();
    }
    let (w, h) = img.dimensions();
    let mut out: RgbaImage = ImageBuffer::from_pixel(w, h, Rgba([255, 255, 255, 255]));
    for y in 0..h {
        for x in 0..w {
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            if nx >= 0 && ny >= 0 && (nx as u32) < w && (ny as u32) < h {
                out.put_pixel(nx as u32, ny as u32, *img.get_pixel(x, y));
            }
        }
    }
    out
}

/// Translate an inclusive bbox by `(dx, dy)`, clamping to the image bounds so the
/// shifted box stays valid for `union_bbox`/`crop_rect`. Matches `shift_image`.
pub(crate) fn shift_bbox(bb: BBox, dx: i32, dy: i32, dims: (u32, u32)) -> BBox {
    let (w, h) = dims;
    let clamp = |v: i32, hi: u32| v.clamp(0, hi.saturating_sub(1) as i32) as u32;
    (
        clamp(bb.0 as i32 + dx, w),
        clamp(bb.1 as i32 + dy, h),
        clamp(bb.2 as i32 + dx, w),
        clamp(bb.3 as i32 + dy, h),
    )
}

/// Union of two inclusive bboxes (min of mins, max of maxes).
pub(crate) fn union_bbox(a: BBox, b: BBox) -> BBox {
    (
        a.0.min(b.0),
        a.1.min(b.1),
        a.2.max(b.2),
        a.3.max(b.3),
    )
}

// ---------------------------------------------------------------------------
// Content mask (V2; spec §1.4)
// ---------------------------------------------------------------------------

/// A 1-bit-per-pixel content mask in row-major order: bit set iff the pixel is
/// ink (`is_content`). Packed into `u64` words so the per-pixel classifier and
/// the structural-edge dilation can test membership in O(1) without re-running
/// `is_content`. Used only by the V2 comparator path.
pub(crate) struct Mask {
    pub(crate) w: u32,
    pub(crate) h: u32,
    bits: Vec<u64>,
}

impl Mask {
    #[inline]
    fn idx(&self, x: u32, y: u32) -> usize {
        (y as usize) * (self.w as usize) + (x as usize)
    }
    /// Whether the pixel at `(x, y)` is ink. Out-of-bounds reads as `false`.
    #[inline]
    pub(crate) fn get(&self, x: u32, y: u32) -> bool {
        if x >= self.w || y >= self.h {
            return false;
        }
        let i = self.idx(x, y);
        (self.bits[i >> 6] >> (i & 63)) & 1 == 1
    }
    #[inline]
    fn set(&mut self, x: u32, y: u32) {
        let i = self.idx(x, y);
        self.bits[i >> 6] |= 1u64 << (i & 63);
    }
}

/// Build the content mask of `img`: one set bit per ink pixel (`is_content`).
pub(crate) fn content_mask(img: &RgbaImage) -> Mask {
    let (w, h) = img.dimensions();
    let words = ((w as usize * h as usize) + 63) / 64;
    let mut m = Mask {
        w,
        h,
        bits: vec![0u64; words.max(1)],
    };
    for y in 0..h {
        for x in 0..w {
            if is_content(img.get_pixel(x, y)) {
                m.set(x, y);
            }
        }
    }
    m
}

/// Crop `img` to the inclusive rectangle `bb` in `img`'s OWN coordinate space,
/// padding with white where the rectangle extends past the image bounds. Both
/// ref and candidate are cropped to the SAME rectangle, so output dims match and
/// every pixel compares like-for-like at the same page position.
pub(crate) fn crop_rect(img: &RgbaImage, bb: BBox) -> RgbaImage {
    let (min_x, min_y, max_x, max_y) = bb;
    let w = max_x - min_x + 1;
    let h = max_y - min_y + 1;
    let mut out: RgbaImage = ImageBuffer::from_pixel(w, h, Rgba([255, 255, 255, 255]));
    for oy in 0..h {
        for ox in 0..w {
            let sx = min_x + ox;
            let sy = min_y + oy;
            if sx < img.width() && sy < img.height() {
                out.put_pixel(ox, oy, *img.get_pixel(sx, sy));
            }
        }
    }
    out
}
