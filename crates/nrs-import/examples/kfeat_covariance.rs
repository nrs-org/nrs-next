//! Empirical `k_feat` seed from the legacy corpus: correlate summed
//! per-anchor mass across `nrs-impl` entries and clamp to `[0,1]`.
//!
//! This is a *prior*, not a fit: `INFRA.md` §8 has `nrs-fit` derive
//! `k_feat` from indifference-bracket elicitation, "leaving
//! *unidentified* parameters at their prior" otherwise. Correlation
//! across a hand-scored corpus conflates real anchor overlap with
//! halo effects (loving a franchise inflates every dimension at once)
//! and missing-not-at-random coverage (an anchor is only ever scored
//! when the author bothered), so treat this as a cheap seed to
//! override once real elicitation data exists, not ground truth.
//!
//! Deliberately does *not* run `nrs_core::score`/`explain`: those
//! resolve relations first, pooling many children's atoms into one
//! franchise entry's measure, which would smear each child's own
//! anchor mix into an artificial (ecological) correlation. Reading
//! `snap.impacts` directly instead correlates what a human actually
//! co-assigned on one act of scoring.
//!
//!   cargo run -p nrs-import --example kfeat_covariance -- \
//!     /path/to/NRSImplMain.xml
//!
//! `--json` prints the full anchor x anchor correlation/support
//! matrices instead of the text report, for feeding the heatmap/tweak
//! webapp (`kfeat-tweak.html`) rather than eyeballing a terminal table.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use nrs_core::{AnchorId, EntryId};

/// Below this many entries with nonzero mass in *both* anchors, a
/// correlation is more likely sampling noise than signal - the
/// AL/AV/AM/Boredom/Additional anchors in particular are only scored
/// on a minority of entries (see the example's own report for exact
/// support counts on this corpus).
const MIN_SUPPORT: usize = 15;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .map(PathBuf::from)
        .expect("usage: kfeat_covariance <path to NRSImplMain.xml> [--json]");
    let json_mode = args.next().as_deref() == Some("--json");
    let today = time::OffsetDateTime::now_utc().date();

    let doc = nrs_import::xml::parse_file(&path, today).expect("parse");
    let (snap, _warnings) = nrs_import::build_snapshot(&doc);

    let mut per_entry: BTreeMap<EntryId, BTreeMap<AnchorId, f64>> = BTreeMap::new();
    for impact in &snap.impacts {
        let bucket = per_entry.entry(impact.target.clone()).or_default();
        for atom in &impact.atoms {
            for (anchor, weight) in &atom.blend {
                *bucket.entry(anchor.clone()).or_insert(0.0) +=
                    f64::from(atom.magnitude.abs()) * f64::from(weight.abs());
            }
        }
    }

    let anchors: Vec<AnchorId> = per_entry
        .values()
        .flat_map(|m| m.keys().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let rows: Vec<Vec<f64>> = per_entry
        .values()
        .map(|m| {
            anchors
                .iter()
                .map(|a| m.get(a).copied().unwrap_or(0.0))
                .collect()
        })
        .collect();

    let n = rows.len() as f64;
    let means: Vec<f64> = (0..anchors.len())
        .map(|j| rows.iter().map(|r| r[j]).sum::<f64>() / n)
        .collect();
    let stddevs: Vec<f64> = (0..anchors.len())
        .map(|j| (rows.iter().map(|r| (r[j] - means[j]).powi(2)).sum::<f64>() / n).sqrt())
        .collect();

    let na = anchors.len();
    let mut corr_matrix = vec![vec![0.0f64; na]; na];
    let mut support_matrix = vec![vec![0usize; na]; na];
    for i in 0..na {
        support_matrix[i][i] = rows.iter().filter(|r| r[i] > 0.0).count();
        corr_matrix[i][i] = 1.0;
        for j in (i + 1)..na {
            let support = rows.iter().filter(|r| r[i] > 0.0 && r[j] > 0.0).count();
            let corr = if stddevs[i] == 0.0 || stddevs[j] == 0.0 {
                0.0
            } else {
                let cov = rows
                    .iter()
                    .map(|r| (r[i] - means[i]) * (r[j] - means[j]))
                    .sum::<f64>()
                    / n;
                cov / (stddevs[i] * stddevs[j])
            };
            corr_matrix[i][j] = corr;
            corr_matrix[j][i] = corr;
            support_matrix[i][j] = support;
            support_matrix[j][i] = support;
        }
    }

    if json_mode {
        print_json(&anchors, rows.len(), &corr_matrix, &support_matrix);
        return;
    }

    println!(
        "{} entries with scored impacts, {} anchors: {}",
        rows.len(),
        anchors.len(),
        anchors
            .iter()
            .map(|a| a.0.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    struct Pair {
        i: usize,
        j: usize,
        corr: f64,
        support: usize,
    }
    let mut pairs = Vec::new();
    for i in 0..na {
        for j in (i + 1)..na {
            pairs.push(Pair {
                i,
                j,
                corr: corr_matrix[i][j],
                support: support_matrix[i][j],
            });
        }
    }
    pairs.sort_by(|a, b| b.corr.abs().partial_cmp(&a.corr.abs()).unwrap());

    println!(
        "\n{:<12}{:<12}{:>8}{:>10}{:>10}",
        "anchor a", "anchor b", "corr", "support", "k_feat"
    );
    let mut suggested = Vec::new();
    for p in &pairs {
        let k = p.corr.max(0.0) as f32;
        let confident = p.support >= MIN_SUPPORT;
        println!(
            "{:<12}{:<12}{:>8.3}{:>10}{:>10}",
            anchors[p.i].0,
            anchors[p.j].0,
            p.corr,
            p.support,
            if confident {
                format!("{k:.3}")
            } else {
                "-".into()
            }
        );
        if confident && k > 0.0 {
            suggested.push((&anchors[p.i], &anchors[p.j], k));
        }
    }

    println!(
        "\n-- suggested KFeatMatrix seed (support >= {MIN_SUPPORT}, corr clamped to [0,1]) --"
    );
    for (a, b, k) in &suggested {
        println!(
            "k_feat.pairs.insert((AnchorId({:?}.into()), AnchorId({:?}.into())), {k:.3});",
            a.0, b.0
        );
    }
}

/// Hand-rolled rather than pulling in `serde_json` (only a dev
/// dependency here behind the `scripting` feature): the payload is
/// just anchor names plus two square `f64`/`usize` matrices, so a
/// dependency would buy nothing over a few `write!` calls.
fn print_json(anchors: &[AnchorId], n_entries: usize, corr: &[Vec<f64>], support: &[Vec<usize>]) {
    let names = anchors
        .iter()
        .map(|a| format!("{:?}", a.0))
        .collect::<Vec<_>>()
        .join(",");
    let corr_rows = corr
        .iter()
        .map(|row| {
            format!(
                "[{}]",
                row.iter()
                    .map(|v| format!("{v:.6}"))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let support_rows = support
        .iter()
        .map(|row| {
            format!(
                "[{}]",
                row.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    println!(
        "{{\"anchors\":[{names}],\"nEntries\":{n_entries},\"corr\":[{corr_rows}],\"support\":[{support_rows}]}}"
    );
}
