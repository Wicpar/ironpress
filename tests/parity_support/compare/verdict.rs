//! Multi-gate verdict (spec §1.10): PASS/PARTIAL/FAIL decided by per-class
//! severities, not a single `diff_pct`.
//!
//! PASS requires EVERY gate within its PASS bound. FAIL if ANY gate exceeds its
//! PARTIAL bound or hits a hard colour gate. Else PARTIAL. Crucially, the
//! geometry, coverage, and hard-colour gates are NOT overridable by manifest
//! thresholds — only `G_COLOR_PCT` and the derived total-area bound are — so no
//! per-fixture tuning can re-introduce a size/margin/recolour/missing false-pass
//! (amendment A6).

use super::super::config::{
    COLOR_DE_FAIL, COLOR_DE_PASS, G_COLOR_PCT, G_EDGE_CSS, G_EXTRA_PCT, G_MISSING_PCT, G_SHIFT_CSS,
};
use super::super::manifest::ManifestEntry;
use super::super::report::Status;
use super::classify::PixelClass;
use super::segment::DiffRegion;
use super::tally::ClassTally;

pub(crate) struct Verdict {
    pub(crate) status: Status,
    pub(crate) diff_pct: f64,
    pub(crate) dominant_class: PixelClass,
}

/// Apply the §1.10 gates. The returned `diff_pct` is a placeholder approximated
/// from the class percentages; `compare_v2` overwrites it with the exact
/// class-map-derived scalar (which needs the full class map, not just the tally).
pub(crate) fn verdict(t: &ClassTally, regions: &[DiffRegion], entry: &ManifestEntry) -> Verdict {
    // Manifest may RELAX only G_COLOR_PCT (and the derived total bound). Geometry,
    // coverage, and hard-colour gates are fixed.
    let color_pass = entry
        .pass_threshold_pct
        .map(|v| v.max(G_COLOR_PCT.0))
        .unwrap_or(G_COLOR_PCT.0);
    let color_partial = entry
        .partial_threshold_pct
        .map(|v| v.max(G_COLOR_PCT.1))
        .unwrap_or(G_COLOR_PCT.1);

    let dominant_class = elect_dominant(regions);

    // --- FAIL gates -------------------------------------------------------
    // (1) Whole feature absent: a Missing-dominant region covering >=50% of ref.
    let whole_missing = regions
        .iter()
        .any(|r| r.dominant == PixelClass::Missing)
        && t.missing_pct >= 50.0;

    // (2) Hard colour: a SOLID block dominated by a real colour error — a ColorErr-
    // DOMINANT region above the area floor with ΔE >= FAIL. We deliberately do NOT
    // fire on the aggregate (`color_de >= FAIL && color_pct >= floor`): scattered
    // glyph-edge ColorErr from cross-rasterizer text AA has a huge ΔE (black-on-white)
    // but is NOT a recolour — those pixels sit in GeomShift-DOMINANT regions, so the
    // dominant-class condition excludes them while a genuinely recoloured fill or
    // recoloured glyph (ColorErr-dominant region) still hard-fails here.
    let hard_color = regions.iter().any(|r| {
        r.dominant == PixelClass::ColorErr && r.area_pct >= G_COLOR_PCT.0 && r.delta_e >= COLOR_DE_FAIL
    });

    let any_fail = whole_missing
        || hard_color
        || t.color_pct > color_partial
        || t.missing_pct > G_MISSING_PCT.1
        || t.extra_pct > G_EXTRA_PCT.1
        || t.edge_max_css > G_EDGE_CSS.1
        || t.shift_max_css > G_SHIFT_CSS.1;

    let status = if any_fail {
        Status::Fail
    } else if t.color_pct <= color_pass
        && t.color_de <= COLOR_DE_PASS
        && t.missing_pct <= G_MISSING_PCT.0
        && t.extra_pct <= G_EXTRA_PCT.0
        && t.edge_max_css <= G_EDGE_CSS.0
        && t.shift_max_css <= G_SHIFT_CSS.0
    {
        Status::Pass
    } else {
        Status::Partial
    };

    // `diff_pct` placeholder (overwritten by compare_v2 with the exact value).
    let diff_pct = (t.color_pct + t.missing_pct + t.extra_pct).min(100.0);

    Verdict {
        status,
        diff_pct,
        dominant_class,
    }
}

/// The dominant class among real-diff regions: highest `area_pct`; ties broken by
/// severity (Missing > Extra > ColorErr > GeomShift).
fn elect_dominant(regions: &[DiffRegion]) -> PixelClass {
    let mut best: Option<&DiffRegion> = None;
    for r in regions {
        best = match best {
            None => Some(r),
            Some(b) => {
                if r.area_pct > b.area_pct + 1e-9
                    || ((r.area_pct - b.area_pct).abs() <= 1e-9 && severity(r.dominant) > severity(b.dominant))
                {
                    Some(r)
                } else {
                    Some(b)
                }
            }
        };
    }
    best.map(|r| r.dominant).unwrap_or(PixelClass::Match)
}

#[inline]
fn severity(c: PixelClass) -> u8 {
    match c {
        PixelClass::Missing => 5,
        PixelClass::Extra => 4,
        PixelClass::ColorErr => 3,
        PixelClass::GeomShift => 2,
        PixelClass::AaEdge => 1,
        PixelClass::Match => 0,
    }
}
