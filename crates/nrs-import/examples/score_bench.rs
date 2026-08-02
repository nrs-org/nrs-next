//! Load a real `nrsml` tree, score it under each `Homogeneity` mode,
//! and time the hot path on that actual data:
//!   cargo run --release -p nrs-import --example score_bench --features scripting \
//!     -- /path/to/NRSImplMain.xml
//! Reports non-finite counts (a correctness smoke test), median score()
//! time per mode, and the top entries by value so the two modes can be
//! eyeballed against each other.
//!
//! Also crosses each mode with `k_feat = default (empty)` vs `k_feat =
//! kfeat_covariance`'s suggested seed (support >= 15, hardcoded below):
//! `all_crowdings` (redundancy.rs) only walks the O(cells^2) cross-anchor
//! pairwise fallback when `k_feat.pairs` is non-empty — the real corpus
//! has been on the always-empty fast path so far, so this is the first
//! measurement of what a populated `k_feat` actually costs `nrs-app`'s
//! per-request `score()` call.

use std::path::PathBuf;
use std::time::Instant;

use nrs_core::{
    AnchorId, Homogeneity, Isotonic, KFeatMatrix, KSrcLevels, Params, RelationScaling, Snapshot,
};

fn k_feat_seed() -> KFeatMatrix {
    let mut k_feat = KFeatMatrix::default();
    let pairs: &[(&str, &str, f32)] = &[
        ("AL", "Boredom", 0.832),
        ("AV", "Boredom", 0.757),
        ("AL", "AV", 0.731),
        ("AL", "AP", 0.442),
        ("AP", "Boredom", 0.441),
        ("CU", "MP", 0.426),
        ("CP", "MP", 0.414),
        ("AL", "CU", 0.378),
        ("AP", "AV", 0.341),
        ("AP", "MP", 0.330),
        ("Boredom", "CU", 0.314),
        ("AV", "CU", 0.291),
        ("AL", "MP", 0.285),
        ("AP", "AU", 0.272),
        ("AV", "MP", 0.268),
        ("AP", "CU", 0.251),
        ("AL", "CP", 0.247),
        ("Boredom", "MP", 0.241),
        ("AP", "MU", 0.228),
        ("AU", "MU", 0.213),
        ("AL", "MU", 0.205),
        ("CP", "CU", 0.201),
        ("Additional", "Boredom", 0.191),
        ("AV", "CP", 0.188),
        ("Boredom", "CP", 0.178),
        ("AL", "AU", 0.163),
        ("AU", "AV", 0.151),
        ("AU", "Boredom", 0.142),
        ("AP", "CP", 0.137),
        ("Boredom", "MU", 0.087),
        ("AV", "MU", 0.062),
    ];
    for &(a, b, k) in pairs {
        k_feat
            .pairs
            .insert((AnchorId(a.into()), AnchorId(b.into())), k);
    }
    k_feat
}

fn params(homogeneity: Homogeneity, k_feat: KFeatMatrix) -> Params {
    Params {
        q: 0.6,
        k_src_levels: KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        },
        k_feat,
        homogeneity,
        calibration: Isotonic::default(),
        relation_scaling: RelationScaling::Fixed,
    }
}

fn time_median(snap: &Snapshot, p: &Params, warmup: usize, iters: usize) -> f64 {
    for _ in 0..warmup {
        std::hint::black_box(nrs_core::score(snap, p));
    }
    let mut samples: Vec<f64> = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        std::hint::black_box(nrs_core::score(
            std::hint::black_box(snap),
            std::hint::black_box(p),
        ));
        samples.push(t.elapsed().as_secs_f64() * 1e3);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    samples[samples.len() / 2]
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: score_bench <path to .xml>");
    let today = time::OffsetDateTime::now_utc().date();

    let doc = nrs_import::xml::parse_file(&path, today).expect("parse");
    let (snap, _extras, _warnings) = nrs_import::build_snapshot(&doc);
    println!(
        "snapshot: {} entries, {} impacts, {} relations\n",
        snap.entries.len(),
        snap.impacts.len(),
        snap.relations.len()
    );

    let modes = [
        ("degreeq", Homogeneity::DegreeQ),
        ("spectral(b=2)", Homogeneity::Spectral { beta: 2.0 }),
    ];
    let k_feat_variants = [
        ("k_feat=empty", KFeatMatrix::default()),
        ("k_feat=seed(31 pairs)", k_feat_seed()),
    ];

    let title = |id: &nrs_core::EntryId| -> String {
        snap.entries
            .iter()
            .find(|e| &e.id == id)
            .and_then(|e| e.title_cache.clone())
            .unwrap_or_else(|| id.0.clone())
    };

    let mut medians: Vec<(&str, &str, f64)> = Vec::new();
    for (kf_name, k_feat) in &k_feat_variants {
        for (name, homogeneity) in modes {
            let p = params(homogeneity, k_feat.clone());
            let scores = nrs_core::score(&snap, &p);
            let non_finite = scores.values.values().filter(|v| !v.is_finite()).count();
            let negative = scores.values.values().filter(|&&v| v < 0.0).count();
            let median = time_median(&snap, &p, 3, 25);
            medians.push((kf_name, name, median));

            let mut ranked: Vec<(&nrs_core::EntryId, f32)> =
                scores.values.iter().map(|(k, &v)| (k, v)).collect();
            ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

            println!("=== {kf_name} / {name} ===");
            println!(
                "  scored {}  non-finite {}  negative {}  median score() {:.2} ms",
                scores.values.len(),
                non_finite,
                negative,
                median
            );
            println!("  top 5 by value:");
            for (id, v) in ranked.iter().take(5) {
                println!("    {v:>10.3}  {}", title(id));
            }
            println!();
        }
    }

    println!("=== summary ===");
    for (name, _) in modes {
        let empty = medians
            .iter()
            .find(|(kf, m, _)| *kf == "k_feat=empty" && *m == name)
            .unwrap()
            .2;
        let seeded = medians
            .iter()
            .find(|(kf, m, _)| *kf == "k_feat=seed(31 pairs)" && *m == name)
            .unwrap()
            .2;
        println!(
            "  {name:<14}  empty {empty:>7.2} ms  seeded {seeded:>7.2} ms  ({:+.1}%)",
            (seeded - empty) / empty * 100.0
        );
    }
}
