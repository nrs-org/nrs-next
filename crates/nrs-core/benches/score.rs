//! Perf benchmark for the hot path: `nrs_core::score()` over a
//! synthetic `Snapshot` shaped like the real dataset (INFRA.md's
//! actual `nrs-impl` XML has ~1570 entries, ~3400 impacts, ~1800
//! relations after import) - kept synthetic rather than depending on
//! that external XML file so this benchmark is self-contained and
//! portable. `nrs-app`'s per-request cost is exactly this call (it
//! caches the parsed `Snapshot` and reruns `score()` live whenever a
//! knob changes), so this is the number that matters for "does the
//! live-scoring web UI feel instant."

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use nrs_core::{
    AnchorId, Atom, Entry, EntryId, Impact, ImpactId, Isotonic, KFeatMatrix, Palette, Params,
    RelKind, Relation, Snapshot, SourceId, TimeSupport, Transform,
};

const ANCHORS: &[&str] = &[
    "gut-punch",
    "cozy-warmth",
    "gorgeous-visuals",
    "clever-premise",
    "bittersweet",
    "hype",
    "nostalgia",
    "awe",
];

/// A container ("franchise") every `GROUP_SIZE` entries, each holding
/// a few impacts at varying anchors/sources, plus a light scattering
/// of Remix/KilledBy relations to exercise the SCC solver on
/// non-trivial (cyclic) components, not just a pure tree.
const GROUP_SIZE: usize = 8;

fn generate_snapshot(n_leaf_entries: usize) -> Snapshot {
    let mut entries = Vec::new();
    let mut impacts = Vec::new();
    let mut relations = Vec::new();

    let n_groups = n_leaf_entries.div_ceil(GROUP_SIZE);
    for g in 0..n_groups {
        let container_id = EntryId(format!("F{g}"));
        entries.push(Entry {
            id: container_id.clone(),
            store: "bench".into(),
            store_id: format!("F{g}"),
            title_cache: Some(format!("Franchise {g}")),
        });

        let leaves_in_group = GROUP_SIZE.min(n_leaf_entries - g * GROUP_SIZE);
        for l in 0..leaves_in_group {
            let idx = g * GROUP_SIZE + l;
            let entry_id = EntryId(format!("E{idx}"));
            entries.push(Entry {
                id: entry_id.clone(),
                store: "bench".into(),
                store_id: format!("E{idx}"),
                title_cache: Some(format!("Entry {idx}")),
            });

            relations.push(Relation {
                kind: RelKind::Contains,
                from: container_id.clone(),
                to: entry_id.clone(),
                transform: Transform::Scalar(1.0),
            });

            // 2-3 impacts per entry, each a moment atom at a rotating
            // anchor.
            for k in 0..(2 + idx % 2) {
                let anchor = AnchorId(ANCHORS[(idx + k) % ANCHORS.len()].to_string());
                let source = SourceId(format!("char-{}", idx / 3));
                impacts.push(Impact {
                    id: ImpactId(format!("I{idx}-{k}")),
                    target: entry_id.clone(),
                    atoms: vec![Atom {
                        source,
                        blend: vec![(anchor, 1.0)],
                        magnitude: 1.0 + (k as f32) * 0.5,
                        time: TimeSupport::Moment,
                    }],
                });
            }

            // Every 5th leaf remixes the previous one (a light cyclic
            // scattering, not just a pure Contains tree).
            if l > 0 && l % 5 == 0 {
                let prev_id = EntryId(format!("E{}", idx - 1));
                relations.push(Relation {
                    kind: RelKind::Remix,
                    from: entry_id.clone(),
                    to: prev_id.clone(),
                    transform: Transform::Scalar(0.2),
                });
                relations.push(Relation {
                    kind: RelKind::Remix,
                    from: prev_id,
                    to: entry_id,
                    transform: Transform::Scalar(0.2),
                });
            }
        }
    }

    Snapshot {
        entries,
        impacts,
        relations,
        palette: Palette {
            anchors: Vec::new(),
            k_feat: KFeatMatrix::default(),
        },
    }
}

/// The first half of `ANCHORS` pool as one group at 0.6 over box
/// exponents of 0.4; the rest add.
fn k_feat() -> KFeatMatrix {
    let mut k = KFeatMatrix::default();
    let group = &ANCHORS[..ANCHORS.len() / 2];
    for (i, a) in group.iter().enumerate() {
        k.pairs.insert((AnchorId((*a).into()), AnchorId((*a).into())), 0.4);
        for b in &group[i + 1..] {
            k.pairs.insert((AnchorId((*a).into()), AnchorId((*b).into())), 0.6);
        }
    }
    k
}

fn params(k_feat: KFeatMatrix) -> Params {
    Params {
        k_feat,
        time: Default::default(),
        calibration: Isotonic::default(),
        sign: Default::default(),
    }
}

fn bench_score(c: &mut Criterion) {
    let mut group = c.benchmark_group("score");
    // 1600 matches the real dataset's rough entry count.
    let p = params(k_feat());
    for &n in &[100usize, 400, 1600] {
        let snapshot = generate_snapshot(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &snapshot, |b, snap| {
            b.iter(|| nrs_core::score(std::hint::black_box(snap), std::hint::black_box(&p)));
        });
    }
    group.finish();
}

fn bench_explain(c: &mut Criterion) {
    let snapshot = generate_snapshot(1600);
    let target = snapshot.entries[snapshot.entries.len() / 2].id.clone();
    let p = params(k_feat());
    c.bench_function("explain/1600", |b| {
        b.iter(|| {
            nrs_core::explain(
                std::hint::black_box(&snapshot),
                std::hint::black_box(&p),
                target.clone(),
            )
        });
    });
}

/// One entry with `n` anchors, every pair pooling below 1, so the
/// single-linkage pass sees all `n^2 / 2` pairs.
fn dense_snapshot(n: usize) -> (Snapshot, KFeatMatrix) {
    let entry = EntryId("dense".into());
    let source = SourceId("same-source".into());
    let anchors: Vec<_> = (0..n).map(|i| AnchorId(format!("a{i}"))).collect();
    let impacts = anchors
        .iter()
        .enumerate()
        .map(|(i, anchor)| Impact {
            id: ImpactId(format!("i{i}")),
            target: entry.clone(),
            atoms: vec![Atom {
                source: source.clone(),
                blend: vec![(anchor.clone(), 1.0)],
                magnitude: 1.0,
                time: TimeSupport::Moment,
            }],
        })
        .collect();
    let mut k_feat = KFeatMatrix::default();
    for i in 0..n {
        for j in (i + 1)..n {
            k_feat
                .pairs
                .insert((anchors[i].clone(), anchors[j].clone()), 0.5 + 0.4 * (i * j % 7) as f32 / 7.0);
        }
    }
    let snapshot = Snapshot {
        entries: vec![Entry {
            id: entry,
            store: "bench".into(),
            store_id: "dense".into(),
            title_cache: None,
        }],
        impacts,
        relations: Vec::new(),
        palette: Palette {
            anchors: Vec::new(),
            k_feat: KFeatMatrix::default(),
        },
    };
    (snapshot, k_feat)
}

fn bench_dense(c: &mut Criterion) {
    let mut group = c.benchmark_group("dense");
    for &n in &[8usize, 32, 128] {
        let (snapshot, k_feat) = dense_snapshot(n);
        let p = params(k_feat);
        group.bench_with_input(BenchmarkId::from_parameter(n), &snapshot, |b, snap| {
            b.iter(|| nrs_core::score(std::hint::black_box(snap), std::hint::black_box(&p)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_score, bench_explain, bench_dense);
criterion_main!(benches);
