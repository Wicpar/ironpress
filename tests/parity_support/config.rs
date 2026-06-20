//! All tunable constants for the parity engine.
//!
//! Extracted verbatim from the former monolithic `mod.rs` (constants block and
//! the comparator threshold pair). No values changed — this is a mechanical
//! split (C1).

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Rasterization DPI for both candidate and reference. High DPI so fine detail
/// (thin borders, small glyphs, gradient bands) is captured faithfully and any
/// anti-aliased edge is a smaller fraction of a region.
pub(crate) const DPI: u32 = 300;
/// Per-channel tolerance for the bbox white-detection (0..=255).
pub(crate) const WHITE_TOL: i32 = 10;
/// Maximum small-offset registration window (pixels at the rasterization DPI,
/// ~1.5 CSS px at 300 DPI). Before the SSIM compare we cancel a translation of
/// the candidate relative to the reference up to this magnitude, to neutralize a
/// UNIVERSAL sub-perceptual page-origin offset: ironpress anchors content at the
/// spec-correct 28.8pt = 120px@300dpi margin, while the Chrome reference sits a
/// few px in (~116px), producing an IDENTICAL ~+4px right/down shift on every
/// fixture. Clamping to this small window cancels that artifact while leaving any
/// GENUINE layout shift larger than the window unmasked (it still scores high).
pub(crate) const MAX_REG: i32 = 6;
/// Per-channel tolerance for the pixel diff (absorbs sub-pixel AA / gamma).
pub(crate) const CHANNEL_TOL: i32 = 20;
/// Overall-score regression epsilon (percentage points). Below this is noise.
pub(crate) const SCORE_EPSILON: f64 = 0.5;
/// Default thresholds when a manifest entry omits them.
pub(crate) const DEFAULT_PASS_PCT: f64 = 1.5;
pub(crate) const DEFAULT_PARTIAL_PCT: f64 = 12.0;
/// Inherent engine-vs-Chrome floor (percentage points) on the perceptual
/// pixel-diff scale (fraction of pixels that genuinely differ; see `diff_images`).
/// Because the metric ignores anti-aliasing edges and sub-threshold noise, a
/// PIXEL-CORRECT render scores ~0% — far lower than the old SSIM-hybrid baseline
/// (which inflated perfect renders to 4–6%). The perfect-render substrate probes,
/// measured on this scale at 300 DPI (full-suite run, this branch):
///   * probe-color-swatch : 0.00%
///   * probe-fill-box     : 0.00%
///   * probe-block-flow   : 0.32%
///   * probe-border-box   : 0.69%
///   * probe-image-render : 0.79%   <- highest perfect (non-text) probe
/// All perfect renders sit <= 0.8%. probe-text-baseline (20.6%) is the
/// cross-rasterizer text-AA ceiling — text is NOT pixel-identical across two
/// independent rasterizers even when feature-correct — and is deliberately
/// EXCLUDED from the floor (it is a known measurement ceiling, not a bug).
/// We set the PASS floor at 1.5% (clears every perfect probe by ~0.7pp, yet
/// below real small-area defects such as an unrounded per-corner radius at
/// 2.79%), and the PARTIAL floor at 12.0% (a recognizable-but-imperfect render
/// such as slightly-mis-sized flex boxes at 7.6% stays PARTIAL; substantially
/// wrong / missing content fails). Effective per-fixture thresholds are clamped
/// to sit ABOVE this floor so a correct render is never scored as a failure.
pub(crate) const NOISE_FLOOR_PASS_PCT: f64 = 1.5;
pub(crate) const NOISE_FLOOR_PARTIAL_PCT: f64 = 12.0;

/// Per-pixel perceptual threshold (pixelmatch `threshold`, 0..1). Differences
/// below this are treated as identical, so a visually-correct smooth gradient or
/// the sub-tone noise between two independent rasterizers scores ~0% instead of
/// being over-penalized the way global MSSIM is. Higher = more tolerant.
pub(crate) const PM_THRESHOLD: f64 = 0.12;
/// Maximum possible YIQ color delta (pixelmatch constant).
pub(crate) const PM_MAX_DELTA: f64 = 35215.0;
