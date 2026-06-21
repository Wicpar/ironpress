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

// ===========================================================================
// V2 COMPARATOR CONSTANTS (spec §1.1)
//
// These drive the multi-gate V2 verdict path (behind `PARITY_VERDICT=v2`). They
// live ALONGSIDE the legacy constants above — nothing legacy is tightened or
// removed here (that is C6). The V2 path uses its OWN `t_match()` (PM_THRESHOLD
// 0.10, tighter than the legacy 0.12) and its own gates; the legacy `diff_images`
// continues to read the legacy `PM_THRESHOLD`.
// ===========================================================================

/// Device px per CSS px @ 300 DPI (96 CSS px/in -> 300/96 = 3.125).
pub(crate) const CSS_PX: f64 = 3.125;
/// Fixed page-origin correction (device px): ironpress content sits +4,+4 vs the
/// Chrome reference because Chrome's `--print-to-pdf` rounds the printable margin.
/// We shift the candidate by `-GLOBAL_OFFSET` once, uniformly, and audit it — we
/// do NOT search per-fixture (that masked real layout bugs). See spec §0.1/§1.3.
pub(crate) const GLOBAL_OFFSET: (i32, i32) = (4, 4);
/// Allowed raw-probe deviation from `GLOBAL_OFFSET` during calibration audit.
pub(crate) const PROBE_JITTER_PX: i32 = 1;
/// Post-calibration sub-pixel rounding band: a residual displacement within this
/// radius is classed `GeomShift` (counted, never zeroed), not `ColorErr`.
pub(crate) const RESIDUAL_JITTER_PX: i32 = 1;
/// Cross-rasterizer edge-jitter radius (device px). A `Missing`/`Extra` pixel whose
/// SAME-COLOUR ink reappears within this radius in the other image is a displaced
/// glyph/border edge (two rasterizers place the same stroke a px or two apart), not
/// real missing/extra content — it is forgiven as `AaEdge`. This is ΔE-gated (a
/// recoloured displaced edge is NOT forgiven) and CANNOT mask a consistent shift or
/// size change: those are caught independently by the bbox-extent gate
/// (`G_EDGE_CSS`, from `edge_delta_css`), which does not depend on this forgiveness.
/// ~2 device px = ~0.64 CSS px, below the 1.0 CSS-px edge PASS bound. Kept
/// conservative: widening to 3 only flipped one fixture (monospace FAIL->PARTIAL,
/// likely a real font-mapping difference) and did NOT reduce the residual ColorErr
/// on correctly-rendered text (that residual is genuine minor glyph-weight/border
/// difference, not forgivable AA — so it is honestly reported as PARTIAL, not masked).
/// (Distinct from the rejected global best-shift: this is a LOCAL, colour-gated,
/// bidirectional same-ink test that cannot mask a whole-element shift — that is the
/// bbox-extent gate's job.)
pub(crate) const EDGE_JITTER_PX: i32 = 2;

/// V2 per-pixel match threshold (pixelmatch `threshold`, 0..1), TIGHTER than the
/// legacy 0.12. Only used by the V2 path's `t_match()`.
pub(crate) const PM_THRESHOLD_V2: f64 = 0.10;
/// V2 "match" YIQ delta budget (~352). At/below this, a pixel is `Match`.
pub(crate) fn t_match() -> f64 {
    PM_MAX_DELTA * PM_THRESHOLD_V2 * PM_THRESHOLD_V2
}
/// Wider AA tolerance (0..1) — legal ONLY inside the shared edge band.
pub(crate) const AA_THRESHOLD: f64 = 0.18;
/// V2 anti-aliasing YIQ delta budget (~1141). A differing pixel inside the shared
/// edge band and within this budget is `AaEdge` (cross-rasterizer glyph AA).
pub(crate) fn t_aa() -> f64 {
    PM_MAX_DELTA * AA_THRESHOLD * AA_THRESHOLD
}

/// Per-channel 4-neighbour gradient threshold (0..255) for structural edges. A
/// pixel is an edge iff the max per-channel |Δ| to any 4-neighbour exceeds this.
pub(crate) const EDGE_GRAD: i32 = 24;

/// Drop diff regions smaller than this (ignore <3x3 device-px specks).
pub(crate) const REGION_MIN_AREA_PX: u32 = 9;

// --- verdict gates: (PASS bound, PARTIAL bound). FAIL if > PARTIAL bound. ---
/// % of union content pixels classed `ColorErr`.
pub(crate) const G_COLOR_PCT: (f64, f64) = (0.5, 8.0);
/// % of REF content area classed `Missing`.
pub(crate) const G_MISSING_PCT: (f64, f64) = (0.5, 6.0);
/// % of CAND content area classed `Extra`.
pub(crate) const G_EXTRA_PCT: (f64, f64) = (0.5, 6.0);
/// Max per-side content-extent delta, CSS px (the box-size signal).
pub(crate) const G_EDGE_CSS: (f64, f64) = (1.0, 3.0);
/// Residual translation beyond calibration, CSS px.
pub(crate) const G_SHIFT_CSS: (f64, f64) = (1.0, 4.0);
/// ΔE2000: at/below this a colour difference is not a defect even if pixels differ.
pub(crate) const COLOR_DE_PASS: f64 = 2.5;
/// ΔE2000: at/above this is a hard colour failure regardless of area.
pub(crate) const COLOR_DE_FAIL: f64 = 6.0;
