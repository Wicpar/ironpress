//! ironpress feature-parity engine (core).
//!
//! This module is included by `tests/feature_parity.rs` via `#[path]`. It is the
//! implementation of the parity test driver. It is deliberately self-contained:
//! it shells out only to `pdftoppm` (poppler) at test time and never invokes
//! Chrome (references are pre-generated and committed by
//! `scripts/parity-gen-refs.sh`).
//!
//! Pipeline per fixture:
//!   render in-process (Letter + 28.8pt margins) -> validity check -> temp PDF
//!   -> `pdftoppm -r 150` -> decode candidate + committed reference (image crate)
//!   -> compute each side's content bbox in the SHARED page space, take the
//!      UNION, crop BOTH to that identical rectangle (preserves offsets) -> if
//!      exactly one side is blank, force FAIL -> per-pixel diff over the union
//!   -> classify PASS/PARTIAL/FAIL/UNKNOWN -> (on non-pass)
//!   write a diff overlay -> aggregate weighted scores.
//!
//! The engine ALWAYS writes `report.json` + `REPORT.md`, then enforces the
//! regression gate against the committed baseline `report.json` (loaded before
//! any write): it fails the test only on an overall-score regression beyond
//! EPSILON, or a named PASS->FAIL transition. Missing baseline => first run
//! (write baseline, pass). A missing reference or a missing `pdftoppm` yields
//! UNKNOWN and never fails CI. A single fixture error never aborts the run.
//!
//! The engine is split into single-responsibility submodules (C1 mechanical
//! split). This `mod.rs` is the thin orchestrator: it wires `run()`'s top-level
//! flow and the per-fixture pipeline; all algorithms live in the submodules.

mod calibrate;
mod compare;
mod config;
mod diagnose;
mod gate;
mod geom;
mod manifest;
mod rasterize;
mod render;
mod report;
mod util;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::{ImageBuffer, Rgba};
use rayon::prelude::*;

use calibrate::{assert_calibration, calibrate};
use compare::{classify, compare_v2, diff_images};
use diagnose::compute_attribution;
use gate::{
    build_report, check_refs_freshness, collect_suspect_unsupported_pass, compute_coverage,
    compute_fix_first, enforce_gate,
};
use geom::{content_bbox, crop_rect, shift_bbox, shift_image, union_bbox};
use manifest::{find_ref_mismatches, load_manifests, ManifestEntry};
use render::{check_pdf_valid, load_bundled_fonts, render_pdf, SharedFonts};
use report::{
    fixture_fail, fixture_unknown, write_html_reports, write_report_json, write_report_md,
    FixtureResult, Report, Status,
};
use util::{sha256_hex, which};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run() -> Result<(), String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let parity_dir = root.join("tests").join("parity");
    let manifest_dir = parity_dir.join("manifest");
    let cases_dir = parity_dir.join("cases");
    let refs_dir = parity_dir.join("refs");
    let diffs_dir = parity_dir.join("diffs");
    let out_dir = parity_dir.join("out");
    let reports_dir = parity_dir.join("reports");
    let tmp_dir = root.join("target").join("parity-tmp");
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("cannot create temp dir {}: {e}", tmp_dir.display()))?;

    // Load committed baseline BEFORE writing anything.
    let baseline_path = parity_dir.join("report.json");
    let baseline: Option<Report> = std::fs::read_to_string(&baseline_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());

    let pdftoppm_available = which("pdftoppm");
    if !pdftoppm_available {
        eprintln!(
            "parity: WARNING pdftoppm not found on PATH; all fixtures will be UNKNOWN (not gating)."
        );
    }

    // Discover + parse manifests.
    let mut entries = match load_manifests(&manifest_dir, &parity_dir) {
        Ok(e) => e,
        Err(e) => return Err(e),
    };
    if entries.is_empty() {
        eprintln!(
            "parity: no manifest entries found under {} (nothing to do).",
            manifest_dir.display()
        );
    }

    // Verdict mode: default LEGACY; V2 multi-gate comparator only under
    // `PARITY_VERDICT=v2`. Read once so the per-fixture path is consistent.
    let v2 = std::env::var("PARITY_VERDICT")
        .map(|v| v.eq_ignore_ascii_case("v2"))
        .unwrap_or(false);
    if v2 {
        eprintln!("parity: PARITY_VERDICT=v2 — using the V2 multi-gate comparator.");
    }

    // Fast dev loop (amendment A3): `PARITY_ONLY` is a comma list of substrings;
    // when set, process only fixtures whose `<category>/<id>` contains any
    // substring, and SKIP the regression gate (these are partial runs and must
    // never inform the baseline). Empty/unset => normal full run.
    let only_filter: Vec<String> = std::env::var("PARITY_ONLY")
        .ok()
        .map(|s| {
            s.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let filtered_run = !only_filter.is_empty();
    if filtered_run {
        entries.retain(|e| {
            let key = format!("{}/{}", e.category, e.id);
            only_filter.iter().any(|f| key.contains(f.as_str()))
        });
        eprintln!(
            "parity: PARITY_ONLY={:?} -> {} fixture(s); regression gate SKIPPED (dev run).",
            only_filter,
            entries.len()
        );
    }

    // Load the bundled font bytes ONCE into shared immutable data so the heavy
    // per-fixture work can run in parallel without re-reading the faces from disk
    // per render and without sharing any mutable converter across threads.
    let shared_fonts: SharedFonts = Arc::new(load_bundled_fonts());

    // V2 calibration audit: render the rigid probes once and verify the page-origin
    // offset is the expected fixed translation, BEFORE scoring any fixture. Drift
    // aborts the run loudly. Skipped when pdftoppm is unavailable (nothing renders)
    // or on a filtered dev run (probes may not be selected).
    let calibration = if v2 && pdftoppm_available && !filtered_run {
        match assert_calibration(&entries, &parity_dir, &refs_dir, &tmp_dir, &shared_fonts) {
            Ok(c) => Some(c),
            Err(e) => return Err(e),
        }
    } else {
        None
    };

    // Heavy per-fixture work (ironpress render -> pdftoppm raster -> image decode
    // -> bbox/diff -> classify) is embarrassingly parallel: each fixture builds
    // its OWN HtmlConverter and shells pdftoppm to a per-fixture-UNIQUE temp path
    // (keyed on `entry.id`), so jobs never collide. We size the rayon pool to
    // min(nproc-2, 8) to leave headroom, keep `catch_unwind` per fixture, then
    // SORT the collected results by (category, id) so report.json / REPORT.md are
    // byte-identical regardless of thread scheduling. All scoring / attribution /
    // guard / gate logic downstream is unchanged.
    let pool_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .saturating_sub(2)
        .clamp(1, 8);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(pool_threads)
        .build()
        .map_err(|e| format!("cannot build rayon pool: {e}"))?;

    let mut results: Vec<FixtureResult> = pool.install(|| {
        entries
            .par_iter()
            .map(|entry| {
                let fonts = Arc::clone(&shared_fonts);
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    process_entry(
                        entry,
                        &parity_dir,
                        &cases_dir,
                        &refs_dir,
                        &diffs_dir,
                        &out_dir,
                        &reports_dir,
                        &tmp_dir,
                        pdftoppm_available,
                        &fonts,
                        v2,
                    )
                }))
                .unwrap_or_else(|_| {
                    fixture_fail(entry, 100.0, "panic during processing".to_string())
                });
                eprintln!(
                    "parity: {:8} {:>7.4}%  {}/{}  {}",
                    res.status.as_str(),
                    res.diff_pct,
                    res.category,
                    res.id,
                    res.note
                );
                res
            })
            .collect()
    });

    // Determinism: fix a stable order independent of thread scheduling before any
    // scoring / reporting. `build_report` re-sorts too, but attribution / fix_first
    // / guards iterate `results` directly, so normalize here first.
    results.sort_by(|a, b| {
        (a.category.as_str(), a.id.as_str()).cmp(&(b.category.as_str(), b.id.as_str()))
    });

    // Substrate-probe attribution: name the root cause of each non-PASS fixture.
    compute_attribution(&mut results);
    let fix_first = compute_fix_first(&results);

    // Guards (surfaced, non-gating): id!=ref-filename mismatches and
    // unsupported-but-PASS suspects.
    let ref_mismatches = find_ref_mismatches(&entries, &refs_dir);
    let suspect_unsupported_pass = collect_suspect_unsupported_pass(&results);

    // refs.lock freshness check (reads the lock written by gen-refs; we only
    // READ + verify). Non-gating here — surfaced in report.json + REPORT.md + a
    // loud WARNING line; CI enforces the hard fail.
    let (stale_refs, refs_lock_present) = check_refs_freshness(&parity_dir, &results);

    let mut report = build_report(results, pdftoppm_available);
    report.coverage = compute_coverage(&report);
    report.fix_first = fix_first;
    report.ref_mismatches = ref_mismatches;
    report.suspect_unsupported_pass = suspect_unsupported_pass;
    report.stale_refs = stale_refs;
    report.refs_lock_present = refs_lock_present;
    report.calibration = calibration;

    // A filtered dev run (`PARITY_ONLY`) scores only a subset, so it must NOT
    // overwrite the committed baseline `report.json` / `REPORT.md` (that would
    // corrupt the baseline) and must NOT enforce the gate. Print the summary and
    // return early.
    if filtered_run {
        if let Err(e) = write_html_reports(&reports_dir, &report) {
            eprintln!("parity: WARNING could not write HTML reports: {e}");
        }
        println!(
            "parity (PARTIAL/dev): {}P/{}p/{}F/{}U over {} filtered fixture(s) — baseline NOT written, gate SKIPPED.",
            report.overall.pass,
            report.overall.partial,
            report.overall.fail,
            report.overall.unknown,
            report.overall.total
        );
        return Ok(());
    }

    // ALWAYS write report.json + REPORT.md.
    write_report_json(&baseline_path, &report)?;
    write_report_md(&parity_dir.join("REPORT.md"), &report)?;

    // Generate the in-repo per-theme visual HTML reports (triptych galleries).
    if let Err(e) = write_html_reports(&reports_dir, &report) {
        eprintln!("parity: WARNING could not write HTML reports: {e}");
    }

    println!(
        "parity: {:.2}% ({}P/{}p/{}F/{}U) · scored {:.2}% · report at {}",
        report.overall.score_pct,
        report.overall.pass,
        report.overall.partial,
        report.overall.fail,
        report.overall.unknown,
        report.overall.scored_ratio_pct,
        parity_dir.join("REPORT.md").display()
    );

    if !report.ref_mismatches.is_empty() {
        eprintln!(
            "parity: WARNING {} ref lookup mismatch(es) (id != ref-filename) — see REPORT.md.",
            report.ref_mismatches.len()
        );
    }
    if !report.suspect_unsupported_pass.is_empty() {
        eprintln!(
            "parity: WARNING {} unsupported-but-PASS suspect(s): {} — see REPORT.md.",
            report.suspect_unsupported_pass.len(),
            report.suspect_unsupported_pass.join(", ")
        );
    }
    if !report.refs_lock_present {
        eprintln!(
            "parity: WARNING no refs.lock committed — reference freshness is UNVERIFIED. \
             Run scripts/parity-gen-refs.sh to write the lock."
        );
    } else if !report.stale_refs.is_empty() {
        let ids: Vec<&str> = report.stale_refs.iter().map(|s| s.id.as_str()).collect();
        eprintln!(
            "parity: WARNING {} STALE reference(s) (fixture changed since ref was generated) — \
             regenerate with scripts/parity-gen-refs.sh: {}",
            report.stale_refs.len(),
            ids.join(", ")
        );
    }

    // Regression gate.
    enforce_gate(baseline.as_ref(), &report)
}

// ---------------------------------------------------------------------------
// Per-fixture processing
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn process_entry(
    entry: &ManifestEntry,
    parity_dir: &Path,
    _cases_dir: &Path,
    refs_dir: &Path,
    diffs_dir: &Path,
    out_dir: &Path,
    reports_dir: &Path,
    tmp_dir: &Path,
    pdftoppm_available: bool,
    fonts: &[(&'static str, Vec<u8>)],
    v2: bool,
) -> FixtureResult {
    let fixture = parity_dir.join(&entry.file);
    let html = match std::fs::read_to_string(&fixture) {
        Ok(h) => h,
        Err(e) => return fixture_fail(entry, 100.0, format!("cannot read fixture: {e}")),
    };
    // SHA-256 of the fixture HTML for the refs.lock freshness check. Computed
    // once here so every result (even UNKNOWN/error paths via the `with_sha`
    // closure below) carries it.
    let html_sha = sha256_hex(html.as_bytes());
    // Helper: stamp the sha onto any result we return from this function.
    let with_sha = |mut r: FixtureResult| -> FixtureResult {
        r.html_sha256 = html_sha.clone();
        r
    };

    // In-process render at Chrome-matching geometry.
    let pdf = match render_pdf(&html, entry.sanitize, fonts) {
        Ok(p) => p,
        Err(e) => return with_sha(fixture_fail(entry, 100.0, format!("render error: {e}"))),
    };

    // PDF validity guard (mirror pdf_smoke_tests).
    if let Err(e) = check_pdf_valid(&pdf) {
        return with_sha(fixture_fail(entry, 100.0, format!("malformed PDF: {e}")));
    }

    let pdf_path = tmp_dir.join(format!("{}.pdf", entry.id));
    if let Err(e) = std::fs::write(&pdf_path, &pdf) {
        return with_sha(fixture_fail(entry, 100.0, format!("cannot write temp pdf: {e}")));
    }

    // The committed candidate raster path (LFS). Written below whenever we
    // successfully rasterize, so the in-repo visual reports always have an
    // ironpress image to show even for non-scored (UNKNOWN-ref) fixtures.
    let out_png = out_dir.join(&entry.category).join(format!("{}.png", entry.id));

    if !pdftoppm_available {
        return with_sha(fixture_unknown(entry, "pdftoppm unavailable".to_string()));
    }

    // Rasterize candidate (independent of whether a reference exists), then
    // persist it to the committed `out/` tree.
    let cand_png = tmp_dir.join(format!("{}.png", entry.id));
    if let Err(e) = rasterize::rasterize(&pdf_path, &cand_png, tmp_dir, &entry.id) {
        return with_sha(fixture_fail(entry, 100.0, format!("pdftoppm failed: {e}")));
    }

    // Decode candidate, then persist a committed copy to `out/<cat>/<id>.png`.
    let cand = match image::open(&cand_png) {
        Ok(i) => i.to_rgba8(),
        Err(e) => return with_sha(fixture_fail(entry, 100.0, format!("decode candidate failed: {e}"))),
    };
    if let Some(parent) = out_png.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = cand.save(&out_png);

    // Reference lookup. Absent => UNKNOWN (never gates). Candidate already
    // committed above so the report still shows the ironpress render.
    let ref_path = refs_dir.join(&entry.category).join(format!("{}.png", entry.id));
    if !ref_path.is_file() {
        return with_sha(fixture_unknown(
            entry,
            "no reference (run scripts/parity-gen-refs.sh)".to_string(),
        ));
    }

    let reference = match image::open(&ref_path) {
        Ok(i) => i.to_rgba8(),
        // A corrupt/truncated reference (e.g. a gen-refs run killed mid-rasterize)
        // must NOT be scored as a 100% FAIL — that would gate CI and pollute the
        // baseline on a tooling glitch. Treat it as UNKNOWN (non-gating);
        // re-running gen-refs regenerates it.
        Err(e) => return with_sha(fixture_unknown(entry, format!("reference unreadable (regenerate): {e}"))),
    };

    // V2 PATH (`PARITY_VERDICT=v2`): apply the fixed page-origin calibration, then
    // run the §1.2 multi-detector pipeline. The verdict's status/diff_pct replace
    // the legacy scoring; the classed overlay replaces the legacy diff image. The
    // legacy block below is left entirely untouched for the default path.
    if v2 {
        let cand_cal = calibrate(&cand);
        let outcome = compare_v2(&cand_cal, &reference, entry);
        let diff_pct = util::round4(outcome.diff_pct);

        // Per-class breakdown for tuning (set PARITY_DEBUG_TALLY=1). Non-gating.
        if std::env::var("PARITY_DEBUG_TALLY").is_ok() {
            let t = &outcome.tally;
            eprintln!(
                "tally {}/{}: color={:.2}% (ΔE {:.2}) missing={:.2}% extra={:.2}% edge_max={:.2}css shift_max={:.2}css aa={:.2}% dom={:?}",
                entry.category, entry.id, t.color_pct, t.color_de, t.missing_pct, t.extra_pct,
                t.edge_max_css, t.shift_max_css, t.aa_pct, outcome.verdict.dominant_class
            );
            eprintln!(
                "DIAG {}/{}: [{}] {}  (conf {:.2})",
                entry.category, entry.id, outcome.diagnosis.primary_class,
                outcome.diagnosis.headline, outcome.diagnosis.confidence
            );
        }

        let reports_diff = reports_dir
            .join(&entry.category)
            .join(format!("{}.diff.png", entry.id));
        if let Some(parent) = reports_diff.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = outcome.overlay.save(&reports_diff);
        if outcome.status != Status::Pass {
            let out = diffs_dir.join(&entry.category).join(format!("{}.png", entry.id));
            if let Some(parent) = out.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = outcome.overlay.save(&out);
        }
        // ADDITIVE: attach the V2 diagnosis (spec §2). The attribution prefix
        // (`via {dep}: …` for confounded fixtures) is applied later in `run()` by
        // `compute_attribution`, once every fixture's status is known.
        let mut result = report::fixture_base(entry, outcome.status, diff_pct, String::new());
        result.diagnosis = Some(outcome.diagnosis);
        return with_sha(result);
    }

    // Compute each side's content bbox in the SHARED page coordinate space, then
    // take the UNION (min/max across both). Crop BOTH images to that identical
    // rectangle and diff over the union area. This preserves positional offsets
    // (no per-image re-anchoring) and removes the full-page denominator that let
    // a blank candidate score a tiny diff_pct.
    let cand_bb = content_bbox(&cand);
    let ref_bb = content_bbox(&reference);

    // Safety guard: exactly one side blank while the other has non-trivial
    // content => a genuine all-or-nothing miss. Force FAIL (100%) regardless of
    // any tolerance/dilation that might otherwise mask it.
    let (diff_pct, diff_img) = match (cand_bb, ref_bb) {
        (Some(cb), Some(rb)) => {
            // SMALL-OFFSET REGISTRATION: cancel the UNIVERSAL ~+4px page-origin
            // offset (see MAX_REG) before the diff. The single bbox-corner delta
            // mis-registers by 1–2px whenever the two rasterizers round an edge
            // differently, leaving a red frame around otherwise-identical content.
            // So also search every integer offset within ±MAX_REG for the one that
            // minimizes a cheap proxy cost. Both candidates are CLAMPED to ±MAX_REG
            // so a real layout shift larger than the window is NOT masked.
            //
            // The proxy minimum is not guaranteed to minimize the perceptual diff
            // (especially on already-wrong renders, where it can mis-align), so we
            // evaluate the ACTUAL diff at BOTH the corner delta and the searched
            // offset and keep the lower. This is monotonic: the result is never
            // worse than the corner-only registration that preceded it.
            let corner = (
                (rb.0 as i32 - cb.0 as i32).clamp(-config::MAX_REG, config::MAX_REG),
                (rb.1 as i32 - cb.1 as i32).clamp(-config::MAX_REG, config::MAX_REG),
            );
            let searched = geom::best_registration_offset(&cand, &reference, rb);
            let eval = |dx: i32, dy: i32| {
                let cand_reg = shift_image(&cand, dx, dy);
                let cb_reg = shift_bbox(cb, dx, dy, cand.dimensions());
                let union = union_bbox(cb_reg, rb);
                diff_images(&crop_rect(&cand_reg, union), &crop_rect(&reference, union))
            };
            let corner_res = eval(corner.0, corner.1);
            if searched == corner {
                corner_res
            } else {
                let searched_res = eval(searched.0, searched.1);
                if searched_res.0 < corner_res.0 {
                    searched_res
                } else {
                    corner_res
                }
            }
        }
        (None, None) => {
            // Both blank: pixel-identical empties => perfect parity.
            (0.0, ImageBuffer::from_pixel(1, 1, Rgba([255, 255, 255, 255])))
        }
        (None, Some(rb)) | (Some(rb), None) => {
            // Exactly one side blank, the other has non-trivial content. Diff
            // over the content side's bbox so the overlay shows the missing /
            // extra region, then FORCE 100% (FAIL) regardless of tolerance.
            let cand_a = crop_rect(&cand, rb);
            let ref_a = crop_rect(&reference, rb);
            let (_, overlay) = diff_images(&cand_a, &ref_a);
            (100.0, overlay)
        }
    };
    let diff_pct = util::round4(diff_pct);

    let status = classify(diff_pct, entry.pass_threshold(), entry.partial_threshold());

    // COMMITTED diff map: write the score-faithful SSIM overlay to
    // `reports/<cat>/<id>.diff.png` for EVERY scored fixture (PASS included) so
    // the in-repo visual reports always have all three triptych images.
    let reports_diff = reports_dir
        .join(&entry.category)
        .join(format!("{}.diff.png", entry.id));
    if let Some(parent) = reports_diff.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = diff_img.save(&reports_diff);

    // Also keep the legacy scratch overlay under diffs/ on non-pass (gitignored).
    if status != Status::Pass {
        let out = diffs_dir.join(&entry.category).join(format!("{}.png", entry.id));
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = diff_img.save(&out);
    }

    with_sha(report::fixture_base(entry, status, diff_pct, String::new()))
}

// ---------------------------------------------------------------------------
// Adversarial unit tests for the image-compare SSIM-hybrid metric.
//
// These prove the metric is ROBUST and DISCRIMINATING: pure anti-aliasing on a
// SHARED boundary perturbs the windowed structural score only negligibly (≈0%,
// far under the noise floor), while STRUCTURAL real errors (a border in only one
// image, a several-px shifted line, a recoloured solid sub-region) produce a
// clearly larger dissimilarity. SSIM is windowed and perceptual, so exact
// magnitudes differ from a per-pixel metric; the assertions therefore check the
// ORDERING (AA << real structural error) and that AA is near-zero, NOT specific
// magnitudes. The synthetic images are built in-memory and fully deterministic.
//
// NOTE on chroma-only hairline recolours: a 2px-wide pure-colour change barely
// moves a windowed structural score (it is below the pure-AA reading here), so
// that case asserts only "counted, not exactly zero" — it deliberately does NOT
// claim to exceed the AA case (SSIM cannot, and pretending otherwise would be a
// false assertion). The discriminating cases are the structural ones.
//
// Run ONLY these (not the 300-DPI suite) with, e.g.:
//   cargo test --test feature_parity aa_ -- --nocapture
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::config::NOISE_FLOOR_PASS_PCT;
    use super::geom::{best_registration_offset, content_bbox, crop_rect, shift_bbox, shift_image, union_bbox};
    use super::*;
    use image::{ImageBuffer, Rgba, RgbaImage};

    use compare::diff_images;

    const W: u32 = 80;
    const H: u32 = 60;
    const BLACK: Rgba<u8> = Rgba([0, 0, 0, 255]);
    const WHITE: Rgba<u8> = Rgba([255, 255, 255, 255]);

    fn white_canvas() -> RgbaImage {
        ImageBuffer::from_pixel(W, H, WHITE)
    }

    /// Fill an inclusive rectangle [x0,x1] x [y0,y1] with `c`.
    fn fill_rect(img: &mut RgbaImage, x0: u32, y0: u32, x1: u32, y1: u32, c: Rgba<u8>) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                img.put_pixel(x, y, c);
            }
        }
    }

    /// diff_pct for two same-size synthetic images.
    fn pct(a: &RgbaImage, b: &RgbaImage) -> f64 {
        diff_images(a, b).0
    }

    /// A solid filled square used as the common substrate in several cases.
    fn solid_square() -> RgbaImage {
        let mut img = white_canvas();
        fill_rect(&mut img, 20, 15, 60, 45, BLACK);
        img
    }

    #[test]
    fn aa_identical_is_zero() {
        let a = solid_square();
        let b = solid_square();
        let p = pct(&a, &b);
        eprintln!("aa_identical_is_zero diff_pct = {p:.4}");
        assert!(p < 1e-9, "identical images must diff ~0%, got {p:.6}%");
    }

    #[test]
    fn aa_pure_antialiasing_near_zero() {
        // Both images share the SAME rectangle at the SAME position. A: hard
        // edge. B: a 1px greyscale AA ramp on the left boundary (the AA pixels
        // sit on the shared edge, with solid black inside and solid white
        // outside in BOTH images). Pure anti-aliasing must be excluded.
        let a = solid_square();
        let mut b = solid_square();
        // Replace column 20 (the left boundary) with a mid-grey AA ramp.
        for y in 16..=44 {
            b.put_pixel(20, y, Rgba([128, 128, 128, 255]));
        }
        let p = pct(&a, &b);
        eprintln!("aa_pure_antialiasing_near_zero diff_pct = {p:.4}");
        assert!(
            p < NOISE_FLOOR_PASS_PCT,
            "pure AA on a shared boundary must be near zero, got {p:.4}%"
        );
    }

    #[test]
    fn aa_real_border_only_in_one_image_counted() {
        // A: rectangle WITH a 3px black border frame. B: the same interior but
        // NO border. The border exists in only one image -> must be COUNTED,
        // never masked as AA.
        let mut a = white_canvas();
        // 3px frame around [20..60] x [15..45].
        fill_rect(&mut a, 20, 15, 60, 17, BLACK); // top
        fill_rect(&mut a, 20, 43, 60, 45, BLACK); // bottom
        fill_rect(&mut a, 20, 15, 22, 45, BLACK); // left
        fill_rect(&mut a, 58, 15, 60, 45, BLACK); // right
        let b = white_canvas(); // no border at all
        let p = pct(&a, &b);
        eprintln!("aa_real_border_only_in_one_image_counted diff_pct = {p:.4}");
        assert!(
            p > NOISE_FLOOR_PASS_PCT,
            "a border in only one image must be counted, got {p:.4}%"
        );
    }

    /// The residual a real error must clear to prove the windowed structural
    /// metric registered it at all (a metric that masked an error would score
    /// ~0). SSIM responds strongly to STRUCTURAL change (geometry / large solid
    /// recolours) and only weakly to thin chroma-only shifts, so this bar is set
    /// conservatively above the pure-AA baseline for the structural cases.
    const COUNTED_FLOOR_PCT: f64 = 0.05;

    #[test]
    fn aa_real_shifted_line_counted() {
        // A: a thin 2px vertical line at columns 10-11. B: same line shifted to
        // columns 16-17 (a 6px shift, no overlap). A several-px shift is a real
        // STRUCTURAL error, not AA — the windowed SSIM must register it clearly,
        // well above the pure-AA baseline.
        let mut a = white_canvas();
        fill_rect(&mut a, 10, 10, 11, 49, BLACK);
        let mut b = white_canvas();
        fill_rect(&mut b, 16, 10, 17, 49, BLACK);
        let p = pct(&a, &b);
        eprintln!("aa_real_shifted_line_counted diff_pct = {p:.4}");
        assert!(
            p > COUNTED_FLOOR_PCT,
            "a shifted line must be counted, got {p:.4}% (masked errors score ~0%)"
        );
    }

    #[test]
    fn aa_real_hairline_recolour_counted() {
        // Both: a solid rectangle. B recolours a 2px-wide strip at the left
        // boundary from black to a saturated red. A chroma-only hairline barely
        // perturbs a windowed STRUCTURAL score (it can read BELOW the pure-AA
        // baseline — SSIM is structure-dominant), so we only assert it is counted
        // at all (non-zero), NOT that it exceeds the AA case.
        let a = solid_square();
        let mut b = solid_square();
        fill_rect(&mut b, 20, 15, 21, 45, Rgba([220, 0, 0, 255]));
        let p = pct(&a, &b);
        eprintln!("aa_real_hairline_recolour_counted diff_pct = {p:.4}");
        assert!(
            p > 0.0,
            "a hairline recolour must register some difference, got {p:.4}%"
        );
    }

    #[test]
    fn aa_real_region_recolour_counted_proportional() {
        // Both: a solid rectangle. B recolours a solid sub-rectangle to blue.
        // A large solid recoloured block is a strong structural+colour change and
        // must be counted clearly above the noise floor.
        let a = solid_square();
        let mut b = solid_square();
        // 20x16 sub-rect = 320 px out of 80*60 = 4800 => ~6.67% of the image.
        fill_rect(&mut b, 30, 22, 49, 37, Rgba([0, 0, 220, 255]));
        let p = pct(&a, &b);
        eprintln!("aa_real_region_recolour_counted_proportional diff_pct = {p:.4}");
        assert!(
            p > NOISE_FLOOR_PASS_PCT,
            "a recoloured solid sub-region must be counted, got {p:.4}%"
        );
    }

    #[test]
    fn aa_discriminates_aa_from_real_errors() {
        // The headline assertion: pure AA is near-zero (well under the noise
        // floor) while every STRUCTURAL real-error case is clearly larger. SSIM
        // is windowed so we assert ORDERING (AA << each real error) plus AA being
        // near-zero, not specific magnitudes.
        let aa = {
            let a = solid_square();
            let mut b = solid_square();
            for y in 16..=44 {
                b.put_pixel(20, y, Rgba([128, 128, 128, 255]));
            }
            pct(&a, &b)
        };
        let border = {
            let mut a = white_canvas();
            fill_rect(&mut a, 20, 15, 60, 17, BLACK);
            fill_rect(&mut a, 20, 43, 60, 45, BLACK);
            fill_rect(&mut a, 20, 15, 22, 45, BLACK);
            fill_rect(&mut a, 58, 15, 60, 45, BLACK);
            pct(&a, &white_canvas())
        };
        let shifted = {
            let mut a = white_canvas();
            fill_rect(&mut a, 10, 10, 11, 49, BLACK);
            let mut b = white_canvas();
            fill_rect(&mut b, 16, 10, 17, 49, BLACK);
            pct(&a, &b)
        };
        let region = {
            let a = solid_square();
            let mut b = solid_square();
            fill_rect(&mut b, 30, 22, 49, 37, Rgba([0, 0, 220, 255]));
            pct(&a, &b)
        };
        eprintln!(
            "aa_discriminates: aa={aa:.4}%  border={border:.4}%  shifted={shifted:.4}%  region={region:.4}%"
        );
        // AA must be near-zero (well under the pass floor).
        assert!(
            aa < NOISE_FLOOR_PASS_PCT,
            "pure AA must be near-zero, got {aa:.4}%"
        );
        // Every structural real error must clear the floor AND dominate AA.
        for (name, val) in [("border", border), ("shifted", shifted), ("region", region)] {
            assert!(
                val > NOISE_FLOOR_PASS_PCT,
                "structural error '{name}' must clear the noise floor, got {val:.4}%"
            );
            assert!(
                val > aa * 10.0,
                "structural error '{name}' ({val:.4}%) must dominate AA ({aa:.4}%)"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Clamped small-offset registration (neutralizes the universal page-origin
    // offset without masking genuine layout shifts).
    // -----------------------------------------------------------------------

    /// Score a candidate vs a reference through the SAME registration + union-crop
    /// + SSIM path the real comparator uses, so these tests exercise the actual
    /// behaviour rather than re-deriving it.
    fn registered_pct(cand: &RgbaImage, reference: &RgbaImage) -> f64 {
        match (content_bbox(cand), content_bbox(reference)) {
            (Some(cb), Some(rb)) => {
                let (dx, dy) = best_registration_offset(cand, reference, rb);
                let cand_reg = shift_image(cand, dx, dy);
                let cb_reg = shift_bbox(cb, dx, dy, cand.dimensions());
                let union = union_bbox(cb_reg, rb);
                diff_images(&crop_rect(&cand_reg, union), &crop_rect(reference, union)).0
            }
            (None, None) => 0.0,
            // Exactly one side blank: mirror the comparator's forced-FAIL guard.
            _ => 100.0,
        }
    }

    #[test]
    fn registration_cancels_universal_small_offset() {
        // (a) Two IDENTICAL shapes, the candidate translated by exactly (4,4) —
        // the universal sub-perceptual page-origin offset. Registration (clamped
        // at ±6) cancels it, so the registered diff is ~0, while comparing the
        // SAME pair WITHOUT registration scores clearly higher (proving the offset
        // really was being penalized before).
        let reference = solid_square();
        let cand = shift_image(&reference, 4, 4);

        let raw = pct(&cand, &reference); // no registration, page coords
        let reg = registered_pct(&cand, &reference);
        eprintln!("registration_cancels_universal_small_offset raw={raw:.4}% reg={reg:.4}%");
        assert!(
            reg < NOISE_FLOOR_PASS_PCT,
            "a (4,4) offset must register to ~0%, got {reg:.4}%"
        );
        assert!(
            raw > reg + 1.0,
            "unregistered (4,4) offset ({raw:.4}%) must be visibly penalized vs registered ({reg:.4}%)"
        );
    }

    #[test]
    fn registration_clamps_large_shift_not_masked() {
        // (b) A shape shifted by (20,20) — a GENUINE layout shift far beyond the
        // ±6 window. Registration clamps at 6, leaving a 14px residual, so the
        // pair still scores HIGH (clearly above the noise floor): not masked.
        let reference = solid_square();
        let cand = shift_image(&reference, 20, 20);
        let reg = registered_pct(&cand, &reference);
        eprintln!("registration_clamps_large_shift_not_masked reg={reg:.4}%");
        assert!(
            reg > NOISE_FLOOR_PASS_PCT,
            "a 20px shift must NOT be masked by the ±6 clamp, got {reg:.4}%"
        );
    }

    #[test]
    fn best_registration_recovers_asymmetric_offset() {
        // A candidate translated by an asymmetric (5, 2) — the best-shift search
        // must recover exactly the inverse offset (-5, -2) and register to ~0%,
        // where the naive top-left-corner estimate would also work here but the
        // search is what makes it robust to 1–2px cross-rasterizer edge rounding.
        let reference = solid_square();
        let cand = shift_image(&reference, 5, 2);
        let rb = content_bbox(&reference).unwrap();
        let (dx, dy) = best_registration_offset(&cand, &reference, rb);
        assert_eq!((dx, dy), (-5, -2), "best-shift must recover the inverse offset");
        let reg = registered_pct(&cand, &reference);
        assert!(reg < NOISE_FLOOR_PASS_PCT, "registered diff must be ~0%, got {reg:.4}%");
    }

    #[test]
    fn registration_blank_candidate_still_fails() {
        // (c) A blank (all-white) candidate vs a real reference must still score
        // ~100%: registration must never rescue an all-or-nothing miss.
        let reference = solid_square();
        let blank = white_canvas();
        let reg = registered_pct(&blank, &reference);
        eprintln!("registration_blank_candidate_still_fails reg={reg:.4}%");
        assert!(
            reg >= 100.0 - 1e-9,
            "a blank candidate must still score ~100%, got {reg:.4}%"
        );
    }
}
