//! Phase 1 no-op proof (spec §4.5 item 2 + the CRITICAL acceptance): synthetic
//! `V2Outcome`s are fed through the `RasterVerifier` adapter + combiner, and the
//! combined status is asserted EQUAL to the raster `verdict.rs` status for every
//! case (all-pass, color-fail, edge-fail, missing-fail, mixed, plus boundary and
//! UNKNOWN cases). These run WITHOUT pdftoppm/Chrome (sub-second) and are the
//! standing guard that the multi-verifier seam does not move any verdict while
//! only `RasterVerifier` is present.

use image::{ImageBuffer, Rgba};

use super::super::compare::tally::ClassTally;
use super::super::compare::verdict::verdict;
use super::super::compare::{PixelClass, V2Outcome, Verdict};
use super::super::config::{
    COLOR_DE_FAIL, COLOR_DE_PASS, G_COLOR_PCT, G_EDGE_CSS, G_EXTRA_PCT, G_MISSING_PCT, G_SHIFT_CSS,
};
use super::super::manifest::ManifestEntry;
use super::super::report::Status;
use super::combine::combine;
use super::raster::RasterVerifier;
use super::{Verifier, VerifierKind, VerifyCtx};

// ----------------------------------------------------------------------------
// Builders
// ----------------------------------------------------------------------------

/// A zeroed tally; tests set only the fields they exercise (the rest default to a
/// PASS-clean value).
fn tally() -> ClassTally {
    ClassTally {
        color_pct: 0.0,
        missing_pct: 0.0,
        extra_pct: 0.0,
        edge_max_css: 0.0,
        edge_delta_css: [0.0; 4],
        shift_max_css: 0.0,
        aa_pct: 0.0,
        color_de: 0.0,
        interior_color_pct: 0.0,
        interior_color_de: 0.0,
        modal_drgb: [0, 0, 0],
        total_px: 1,
    }
}

/// A minimal free-geometry manifest entry (no threshold relaxation), so the
/// verdict + the adapter both read the fixed `config.rs` gates.
fn entry() -> ManifestEntry {
    ManifestEntry {
        id: "g".into(),
        category: "g".into(),
        feature: "g".into(),
        subfeature: String::new(),
        description: String::new(),
        file: "cases/g/g.html".into(),
        interaction_of: Vec::new(),
        base_ids: Vec::new(),
        weight: 1.0,
        pass_threshold_pct: None,
        partial_threshold_pct: None,
        sanitize: true,
        kind: "feature".into(),
        depends_on: Vec::new(),
        expected_support: "implemented".into(),
        geometry: "free".into(),
    }
}

/// Wrap a tally into a `V2Outcome` whose `status` is the REAL `verdict.rs` status
/// for that tally — exactly as `compare_v2` would have produced it. This exercises
/// the production `RasterVerifier::from_outcome` path, not a test shortcut.
fn outcome_for(t: ClassTally, e: &ManifestEntry) -> (V2Outcome, Status) {
    let v: Verdict = verdict(&t, &[], e);
    let status = v.status;
    let outcome = V2Outcome {
        status,
        diff_pct: 0.0,
        tally: t,
        regions: Vec::new(),
        verdict: v,
        overlay: ImageBuffer::from_pixel(1, 1, Rgba([255, 255, 255, 255])),
        diagnosis: Default::default(),
    };
    (outcome, status)
}

/// Assert that the combiner (with ONLY the RasterVerifier present) reproduces the
/// raster verdict status for this tally, and return the combined sub-verdict count
/// for the structural checks below.
fn assert_noop(t: ClassTally) -> (Status, usize) {
    let e = entry();
    let (outcome, verdict_status) = outcome_for(t, &e);
    let rv = RasterVerifier::from_outcome(&outcome, &e);

    // The Phase-1 ctx: only `entry` is read by RasterVerifier; the image/pdf
    // fields are placeholders for Phase 2.
    let px = ImageBuffer::from_pixel(1, 1, Rgba([255, 255, 255, 255]));
    let pdf: &[u8] = b"";
    let ctx = VerifyCtx {
        entry: &e,
        pdf,
        cand: &px,
        reference: &px,
    };

    assert!(rv.applies(&ctx), "RasterVerifier must always apply");
    let subs = rv.verify(&ctx);
    let combined = combine(&subs);

    assert_eq!(
        combined.status, verdict_status,
        "combined status must equal verdict.rs status (tally produced {verdict_status:?})"
    );
    // Phase 1: only RasterDiff present -> never any disagreements.
    assert!(
        combined.disagreements.is_empty(),
        "Phase 1 has a single verifier; no disagreements expected"
    );
    (combined.status, subs.len())
}

// ----------------------------------------------------------------------------
// Tests — synthetic V2Outcomes, the §4.5 cases
// ----------------------------------------------------------------------------

#[test]
fn all_pass_is_noop() {
    // Everything clean -> PASS.
    let (status, n) = assert_noop(tally());
    assert_eq!(status, Status::Pass);
    assert_eq!(n, 3, "RasterVerifier emits one sub-verdict per concern");
}

#[test]
fn color_fail_is_noop() {
    // color_pct over the PARTIAL bound -> Appearance FAIL -> verdict FAIL.
    let mut t = tally();
    t.color_pct = G_COLOR_PCT.1 + 1.0;
    let (status, _) = assert_noop(t);
    assert_eq!(status, Status::Fail);
}

#[test]
fn hard_color_fail_is_noop() {
    // Interior recolour (hard-colour gate) -> Appearance FAIL even at small area.
    let mut t = tally();
    t.interior_color_de = COLOR_DE_FAIL + 1.0;
    t.interior_color_pct = G_COLOR_PCT.0 + 0.1;
    t.color_pct = 0.0; // under the area gate; hard-colour alone must FAIL.
    let (status, _) = assert_noop(t);
    assert_eq!(status, Status::Fail);
}

#[test]
fn edge_fail_is_noop() {
    // edge_max_css over the PARTIAL bound -> Geometry FAIL -> verdict FAIL.
    let mut t = tally();
    t.edge_max_css = G_EDGE_CSS.1 + 1.0;
    let (status, _) = assert_noop(t);
    assert_eq!(status, Status::Fail);
}

#[test]
fn shift_fail_is_noop() {
    let mut t = tally();
    t.shift_max_css = G_SHIFT_CSS.1 + 1.0;
    let (status, _) = assert_noop(t);
    assert_eq!(status, Status::Fail);
}

#[test]
fn missing_fail_is_noop() {
    // missing_pct over the PARTIAL bound -> Presence FAIL -> verdict FAIL.
    let mut t = tally();
    t.missing_pct = G_MISSING_PCT.1 + 1.0;
    let (status, _) = assert_noop(t);
    assert_eq!(status, Status::Fail);
}

#[test]
fn extra_fail_is_noop() {
    let mut t = tally();
    t.extra_pct = G_EXTRA_PCT.1 + 1.0;
    let (status, _) = assert_noop(t);
    assert_eq!(status, Status::Fail);
}

#[test]
fn partial_band_is_noop() {
    // A single gate in the PARTIAL band (over PASS, under FAIL) -> PARTIAL.
    let mut t = tally();
    t.edge_max_css = (G_EDGE_CSS.0 + G_EDGE_CSS.1) / 2.0;
    let (status, _) = assert_noop(t);
    assert_eq!(status, Status::Partial);
}

#[test]
fn color_partial_band_is_noop() {
    // color_pct between PASS and PARTIAL bound -> PARTIAL.
    let mut t = tally();
    t.color_pct = (G_COLOR_PCT.0 + G_COLOR_PCT.1) / 2.0;
    let (status, _) = assert_noop(t);
    assert_eq!(status, Status::Partial);
}

#[test]
fn interior_de_partial_is_noop() {
    // Interior ΔE between PASS and FAIL bounds, area clean -> PARTIAL (denied PASS
    // by the interior-ΔE PASS condition, but below the hard-colour FAIL gate).
    let mut t = tally();
    t.interior_color_de = (COLOR_DE_PASS + COLOR_DE_FAIL) / 2.0;
    t.interior_color_pct = G_COLOR_PCT.0 + 0.1;
    let (status, _) = assert_noop(t);
    assert_eq!(status, Status::Partial);
}

#[test]
fn mixed_partial_and_fail_is_noop() {
    // One axis PARTIAL, another FAIL -> WORST == FAIL == verdict.
    let mut t = tally();
    t.edge_max_css = (G_EDGE_CSS.0 + G_EDGE_CSS.1) / 2.0; // Geometry PARTIAL
    t.missing_pct = G_MISSING_PCT.1 + 1.0; // Presence FAIL
    let (status, _) = assert_noop(t);
    assert_eq!(status, Status::Fail);
}

#[test]
fn mixed_two_partials_is_noop() {
    // Two axes PARTIAL, none FAIL -> WORST == PARTIAL == verdict.
    let mut t = tally();
    t.edge_max_css = (G_EDGE_CSS.0 + G_EDGE_CSS.1) / 2.0; // Geometry PARTIAL
    t.color_pct = (G_COLOR_PCT.0 + G_COLOR_PCT.1) / 2.0; // Appearance PARTIAL
    let (status, _) = assert_noop(t);
    assert_eq!(status, Status::Partial);
}

#[test]
fn pass_boundary_is_noop() {
    // Every gate exactly at its PASS bound -> still PASS (verdict uses `<=`).
    let mut t = tally();
    t.edge_max_css = G_EDGE_CSS.0;
    t.shift_max_css = G_SHIFT_CSS.0;
    t.color_pct = G_COLOR_PCT.0;
    t.missing_pct = G_MISSING_PCT.0;
    t.extra_pct = G_EXTRA_PCT.0;
    t.interior_color_de = COLOR_DE_PASS;
    let (status, _) = assert_noop(t);
    assert_eq!(status, Status::Pass);
}

#[test]
fn fail_boundary_just_over_is_noop() {
    // Just over a PARTIAL bound -> FAIL (verdict uses `>`); at the bound -> PARTIAL.
    let mut t = tally();
    t.edge_max_css = G_EDGE_CSS.1; // exactly at PARTIAL bound -> PARTIAL
    assert_eq!(assert_noop(t).0, Status::Partial);
    let mut t2 = tally();
    t2.edge_max_css = G_EDGE_CSS.1 + f64::EPSILON.max(0.001);
    assert_eq!(assert_noop(t2).0, Status::Fail);
}

#[test]
fn unknown_outcome_is_noop() {
    // An UNKNOWN outcome (unscoreable pair) maps every concern to Unknown; the
    // combiner reproduces UNKNOWN. Build it directly (verdict() never returns
    // Unknown for a tally — only the dimension guard does).
    let e = entry();
    let outcome = V2Outcome {
        status: Status::Unknown,
        diff_pct: 0.0,
        tally: tally(),
        regions: Vec::new(),
        verdict: Verdict {
            status: Status::Unknown,
            diff_pct: 0.0,
            dominant_class: PixelClass::Match,
        },
        overlay: ImageBuffer::from_pixel(1, 1, Rgba([255, 255, 255, 255])),
        diagnosis: Default::default(),
    };
    let rv = RasterVerifier::from_outcome(&outcome, &e);
    let px = ImageBuffer::from_pixel(1, 1, Rgba([255, 255, 255, 255]));
    let pdf: &[u8] = b"";
    let ctx = VerifyCtx { entry: &e, pdf, cand: &px, reference: &px };
    let subs = rv.verify(&ctx);
    assert!(subs.iter().all(|s| s.status == Status::Unknown));
    assert_eq!(combine(&subs).status, Status::Unknown);
}

#[test]
fn all_subverdicts_are_raster_in_phase1() {
    // Structural: in Phase 1 every sub-verdict is RasterDiff and the combiner
    // assigns RasterDiff authority to all three concerns.
    let e = entry();
    let (outcome, _) = outcome_for(tally(), &e);
    let rv = RasterVerifier::from_outcome(&outcome, &e);
    let px = ImageBuffer::from_pixel(1, 1, Rgba([255, 255, 255, 255]));
    let pdf: &[u8] = b"";
    let ctx = VerifyCtx { entry: &e, pdf, cand: &px, reference: &px };
    assert_eq!(rv.kind(), VerifierKind::RasterDiff);
    let subs = rv.verify(&ctx);
    assert!(subs.iter().all(|s| s.verifier == VerifierKind::RasterDiff));
    let combined = combine(&subs);
    assert_eq!(combined.per_concern.len(), 3);
    assert!(combined
        .per_concern
        .iter()
        .all(|p| p.authority == VerifierKind::RasterDiff));
    // Each axis status equals the corresponding raster sub-verdict (no challenger
    // can move it in Phase 1).
    for p in &combined.per_concern {
        let sub = subs.iter().find(|s| s.concern == p.concern).unwrap();
        assert_eq!(p.status, sub.status, "axis {:?} must mirror its sub-verdict", p.concern);
    }
}
