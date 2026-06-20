//! Substrate-probe failure attribution: distinguishes a REAL feature failure
//! from one CONFOUNDED by a non-PASS substrate it renders through.
//!
//! Extracted verbatim from the former monolithic `mod.rs` (C1 mechanical split).

use std::collections::BTreeMap;

use super::report::{FixtureResult, Status};

/// For every non-PASS fixture, set `attribution`:
///   CONFOUNDED: <probe feature>  -> a depended substrate id is itself non-PASS
///   REAL                          -> all deps PASS (the target feature is wrong)
/// PASS fixtures get "" (no attribution).
pub(crate) fn compute_attribution(results: &mut [FixtureResult]) {
    // id -> (status, feature) snapshot before mutation.
    let mut snap: BTreeMap<String, (Status, String)> = BTreeMap::new();
    for r in results.iter() {
        snap.insert(r.id.clone(), (r.status, r.feature.clone()));
    }
    for r in results.iter_mut() {
        if r.status == Status::Pass {
            r.attribution.clear();
            continue;
        }
        // Find the first non-PASS dependency (probe or base).
        let mut culprit: Option<String> = None;
        for d in r.depends_on.iter().chain(r.base_ids.iter()) {
            if let Some((st, feat)) = snap.get(d) {
                if *st != Status::Pass {
                    culprit = Some(format!("{feat} (`{d}`)"));
                    break;
                }
            }
        }
        r.attribution = match culprit {
            Some(c) => format!("CONFOUNDED: {c}"),
            None => "REAL".to_string(),
        };
    }
}
