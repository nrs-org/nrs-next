//! Perf benchmark for the hot path: `nrs_core::score()` over a
//! synthetic `Snapshot` shaped like the real dataset (INFRA.md's
//! actual `nrs-impl` XML has ~1570 entries, ~3400 impacts, ~1800
//! relations after import) - kept synthetic rather than depending on
//! that external XML file so this benchmark is self-contained and
//! portable. `nrs-app`'s per-request cost is exactly this call (it
//! caches the parsed `Snapshot` and reruns `score()` live whenever the
//! `q` slider changes), so this is the number that matters for "does
//! the live-scoring web UI feel instant."

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use nrs_core::{
    AnchorId, Atom, Entry, EntryId, Homogeneity, Impact, ImpactId, Isotonic, KFeatMatrix,
    KSrcLevels, Palette, Params, RelKind, Relation, RelationScaling, Snapshot, SourceId, Taxonomy,
    TimeSupport, Transform,
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
    let mut taxonomy = Taxonomy::default();

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
            taxonomy.parent.insert(
                SourceId(entry_id.0.clone()),
                SourceId(container_id.0.clone()),
            );

            // 2-3 impacts per entry, each a moment atom at a rotating
            // anchor with a source shared by nearby entries (so
            // crowding has real work to do, not all-zero k_src).
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
        taxonomy,
    }
}

fn params() -> Params {
    Params {
        q: 0.6,
        k_src_levels: KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        },
        k_feat: KFeatMatrix::default(),
        homogeneity: Homogeneity::DegreeQ,
        calibration: Isotonic::default(),
        relation_scaling: RelationScaling::Fixed,
    }
}

fn bench_score(c: &mut Criterion) {
    let mut group = c.benchmark_group("score");
    let p = params();
    // 1600 matches the real dataset's rough entry count.
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
    let p = params();
    let target = snapshot.entries[snapshot.entries.len() / 2].id.clone();
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

criterion_group!(benches, bench_score, bench_explain);
criterion_main!(benches);
