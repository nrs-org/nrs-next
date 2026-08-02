//! Runs `nrs_core::audit_monotonicity` over a real nrsml dataset,
//! sweeping the redundancy exponent `q` and both `RelationScaling`
//! modes, to answer: how close is the live data to DegreeQ's
//! non-monotonicity harm zone
//! (`redesign-tex/monotonicity-vs-splitting-invariance.md` §1.4)?
//!
//! Usage: `cargo run -p nrs-app --example monotonicity_audit -- <path to NRSImplMain.xml>`

use nrs_core::{
    audit_monotonicity, Homogeneity, Isotonic, KFeatMatrix, KSrcLevels, Params, RelationScaling,
};

// The live app's k_src levels (crates/nrs-app/src/main.rs).
const LIVE_LEVELS: [f32; 5] = [0.95, 0.5, 0.3, 0.15, 0.0];

fn params(q: f32, scaling: RelationScaling) -> Params {
    params_k(q, scaling, LIVE_LEVELS)
}

fn params_k(q: f32, scaling: RelationScaling, levels: [f32; 5]) -> Params {
    Params {
        q,
        k_src_levels: KSrcLevels { levels },
        k_feat: KFeatMatrix::default(),
        homogeneity: Homogeneity::DegreeQ,
        calibration: Isotonic::default(),
        relation_scaling: scaling,
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: monotonicity_audit <path to NRSImplMain.xml>");
    let today = time::OffsetDateTime::now_utc().date();
    let doc = nrs_import::parse_file(std::path::Path::new(&path), today).expect("parse");
    let (snap, _warnings) = nrs_import::build_snapshot(&doc);
    eprintln!(
        "loaded {} entries, {} impacts, {} relations",
        snap.entries.len(),
        snap.impacts.len(),
        snap.relations.len()
    );

    println!(
        "{:<5} {:<6} {:>9} {:>9} {:>7} {:>10}",
        "q", "scale", "cands", "bridging", "viol", "worst_r"
    );
    println!("{}", "-".repeat(52));

    for scaling in [RelationScaling::Fixed, RelationScaling::PowQ] {
        for q in [0.1f32, 0.3, 0.6, 0.9] {
            let r = audit_monotonicity(&snap, &params(q, scaling), 12);
            let scale_name = match scaling {
                RelationScaling::Fixed => "fixed",
                RelationScaling::PowQ => "powq",
            };
            println!(
                "{:<5} {:<6} {:>9} {:>9} {:>7} {:>10.4}",
                q, scale_name, r.n_candidates, r.n_bridging, r.n_violations, r.worst_ratio
            );
        }
    }

    // How does safety depend on the *shape* of the k_src distance→level
    // curve? A bridge (ancestor) insertion can only erode leaves that
    // are less related to each other than to the bridge; with a shallow,
    // monotone curve that gap stays small. Sweep from the live curve to
    // progressively steeper ones (distance-2+ levels collapsing toward
    // 0) at q=0.1, fixed scaling, and watch the worst ratio.
    println!("\nk_src steepness sweep — q=0.1, fixed:");
    println!(
        "{:<26} {:>9} {:>9} {:>7} {:>10}",
        "levels (dist 1..5)", "cands", "bridging", "viol", "worst_r"
    );
    let curves: [[f32; 5]; 4] = [
        LIVE_LEVELS,                // live: shallow, monotone
        [0.95, 0.3, 0.1, 0.0, 0.0], // steeper
        [0.95, 0.1, 0.0, 0.0, 0.0], // steep: leaves nearly isolated
        [0.95, 0.0, 0.0, 0.0, 0.0], // cliff: leaves isolated, parent bridges
    ];
    for lv in curves {
        let r = audit_monotonicity(&snap, &params_k(0.1, RelationScaling::Fixed, lv), 1);
        println!(
            "{:<26} {:>9} {:>9} {:>7} {:>10.4}",
            format!("{lv:?}"),
            r.n_candidates,
            r.n_bridging,
            r.n_violations,
            r.worst_ratio
        );
    }

    // Detail on the single worst-hit setting: powq @ q=0.1 (the
    // combination the recommendation flagged, where PowQ's w^(1/q)
    // manufactures the tiny masses §1.4 says are most fragile).
    println!("\nworst candidates — powq @ q=0.1:");
    let r = audit_monotonicity(&snap, &params(0.1, RelationScaling::PowQ), 12);
    println!(
        "{:<24} {:<20} {:<14} {:>8} {:>8} {:>8} {:>10} {:>6} {:>12}",
        "entry", "insert source", "anchor", "gain", "loss", "ratio", "c_y", "nrel", "min_mass"
    );
    for m in &r.worst {
        println!(
            "{:<24} {:<20} {:<14} {:>8.3} {:>8.3} {:>8.4} {:>10.2e} {:>6} {:>12.2e}",
            trunc(&m.entry.0, 24),
            trunc(&m.source.0, 20),
            trunc(&m.anchor.0, 14),
            m.gain,
            m.loss,
            m.ratio,
            m.c_y,
            m.n_related,
            m.min_related_mass,
        );
    }
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}
