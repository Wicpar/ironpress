//! Combiner (spec §1.2/§1.3): fold per-verifier `SubVerdict`s into a single
//! `CombinedVerdict` by PER-CONCERN AUTHORITY, not by averaging or majority vote.
//!
//! For each concern the AUTHORITATIVE verifier's status decides that axis (the
//! authority table is §1.1); the combined status is WORST over the concerns using
//! the existing `Status::value()` severity order (Pass=1.0 > Partial=0.5 >
//! Fail=0.0; Unknown excluded). A non-authoritative verifier failing on an axis it
//! does NOT own can DOWNGRADE Pass→Partial (recorded as a `Disagreement`) but can
//! never force a Fail and never raises a status.
//!
//! PHASE 1: only `RasterVerifier` is present, and it owns all three concerns, so
//! there is exactly one verifier per axis, no challengers, no disagreements — and
//! WORST-of-its-three-axes == `verdict.rs`'s single status by construction (see
//! `raster.rs` for the equivalence argument, `goldens.rs` for the proof).

use super::super::report::Status;
use super::{Concern, Disagreement, SubVerdict, VerifierKind};

/// The per-axis breakdown carried alongside the combined status. ADDITIVE — used
/// by the report; never feeds the gate beyond `status`.
#[derive(Clone, Debug)]
pub(crate) struct PerConcern {
    pub(crate) concern: Concern,
    pub(crate) status: Status,
    pub(crate) authority: VerifierKind,
}

/// The combiner's output. `status` is what `process_entry` maps onto
/// `FixtureResult.status` (exactly where `outcome.status` was used before).
#[derive(Clone, Debug)]
pub(crate) struct CombinedVerdict {
    pub(crate) status: Status,
    #[allow(dead_code)]
    pub(crate) per_concern: Vec<PerConcern>,
    pub(crate) disagreements: Vec<Disagreement>,
}

/// All three concern axes, in a fixed order so the per-concern list is
/// deterministic.
const CONCERNS: [Concern; 3] = [Concern::Geometry, Concern::Appearance, Concern::Presence];

/// The verifier that holds AUTHORITY over a concern when it is present and
/// applies (§1.1 table):
///   * Geometry  -> PdfGeometry when it applies, else RasterDiff (Phase 1: always
///                  RasterDiff, since PdfGeometry is not implemented yet).
///   * Appearance-> RasterDiff (ΔE/AA/blend over real pixels).
///   * Presence  -> RasterDiff (missing/extra coverage is a whole-area signal).
///
/// Returns the kind that should decide `concern`, given the kinds that produced a
/// sub-verdict for it. The order encodes precedence.
fn authority_for(concern: Concern, present: &[VerifierKind]) -> Option<VerifierKind> {
    let prefer = |order: &[VerifierKind]| -> Option<VerifierKind> {
        order.iter().copied().find(|k| present.contains(k))
    };
    match concern {
        Concern::Geometry => prefer(&[VerifierKind::PdfGeometry, VerifierKind::RasterDiff]),
        Concern::Appearance => prefer(&[VerifierKind::RasterDiff]),
        Concern::Presence => prefer(&[VerifierKind::RasterDiff]),
    }
}

/// Severity rank for WORST (lower is worse). Unknown has no value (excluded).
fn rank(s: Status) -> Option<f64> {
    s.value()
}

/// Pick the worse of two statuses by `Status::value()`. Unknown is excluded: if
/// one side is Unknown the other wins; both Unknown stays Unknown.
fn worse(a: Status, b: Status) -> Status {
    match (rank(a), rank(b)) {
        (Some(va), Some(vb)) => {
            if vb < va {
                b
            } else {
                a
            }
        }
        (Some(_), None) => a,
        (None, Some(_)) => b,
        (None, None) => Status::Unknown,
    }
}

/// Combine sub-verdicts into the final per-fixture verdict.
pub(crate) fn combine(subs: &[SubVerdict]) -> CombinedVerdict {
    let present: Vec<VerifierKind> = {
        let mut v: Vec<VerifierKind> = Vec::new();
        for s in subs {
            if !v.contains(&s.verifier) {
                v.push(s.verifier);
            }
        }
        v
    };

    let mut per_concern: Vec<PerConcern> = Vec::new();
    let mut disagreements: Vec<Disagreement> = Vec::new();
    // Start from the worst *known* axis status; Unknown axes are excluded (mirrors
    // Status::value()==None scoring). If EVERY axis is Unknown the result stays
    // Unknown.
    let mut combined: Option<Status> = None;

    for &concern in &CONCERNS {
        let owner = match authority_for(concern, &present) {
            Some(k) => k,
            None => continue, // no verifier produced this axis — skip it.
        };

        // The authoritative status for this axis.
        let auth_status = subs
            .iter()
            .find(|s| s.concern == concern && s.verifier == owner)
            .map(|s| s.status)
            .unwrap_or(Status::Unknown);

        // Soft cross-signal downgrade (§1.3): a NON-authoritative verifier that
        // FAILs on this concern downgrades a PASS to PARTIAL and is recorded as a
        // disagreement. It can never force a Fail and never raises a status. With
        // only RasterDiff present (Phase 1) there are no challengers, so this loop
        // records nothing and `axis_status == auth_status`.
        let mut axis_status = auth_status;
        for s in subs {
            if s.concern != concern || s.verifier == owner {
                continue;
            }
            // A challenger opinion on an axis it does not own.
            let challenger_worse = matches!(
                (rank(s.status), rank(axis_status)),
                (Some(cs), Some(asx)) if cs < asx
            );
            if challenger_worse {
                disagreements.push(Disagreement {
                    concern,
                    authoritative: auth_status,
                    authoritative_by: owner,
                    challenger: s.status,
                    challenger_by: s.verifier,
                    note: s.headline.clone(),
                });
                // Downgrade Pass->Partial only; never below Partial, never raise.
                if axis_status == Status::Pass {
                    axis_status = Status::Partial;
                }
            }
        }

        per_concern.push(PerConcern {
            concern,
            status: axis_status,
            authority: owner,
        });
        combined = Some(match combined {
            None => axis_status,
            Some(c) => worse(c, axis_status),
        });
    }

    CombinedVerdict {
        status: combined.unwrap_or(Status::Unknown),
        per_concern,
        disagreements,
    }
}
