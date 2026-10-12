//! Load a real `nrsml` tree, score it with the default (legacy) `K`,
//! and time the hot path on that actual data:
//!   cargo run --release -p nrs-import --example score_bench --features scripting \
//!     -- /path/to/NRSImplMain.xml
//! Reports non-finite counts (a correctness smoke test), the median
//! score() time, and the top entries by value.

use std::path::PathBuf;
use std::time::Instant;

use nrs_core::{Isotonic, Params, Snapshot};

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

    let p = Params {
        k_feat: snap.palette.k_feat.clone(),
        time: Default::default(),
        calibration: Isotonic::default(),
    };

    let title = |id: &nrs_core::EntryId| -> String {
        snap.entries
            .iter()
            .find(|e| &e.id == id)
            .and_then(|e| e.title_cache.clone())
            .unwrap_or_else(|| id.0.clone())
    };

    let scores = nrs_core::score(&snap, &p);
    let non_finite = scores.values.values().filter(|v| !v.is_finite()).count();
    let negative = scores.values.values().filter(|&&v| v < 0.0).count();
    let median = time_median(&snap, &p, 3, 25);

    let mut ranked: Vec<(&nrs_core::EntryId, f32)> =
        scores.values.iter().map(|(k, &v)| (k, v)).collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

    println!(
        "scored {}  non-finite {}  negative {}  median score() {:.2} ms",
        scores.values.len(),
        non_finite,
        negative,
        median
    );
    println!("top 10 by value:");
    for (id, v) in ranked.iter().take(10) {
        println!("  {v:>10.3}  {}", title(id));
    }
}
