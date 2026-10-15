//! Black-box property tests against the public `score`/`explain`
//! contract only (INFRA.md §4.1: "trustworthy ⇒ property-testable").
//! Each test pins one axiom of the scoring design.

use nrs_core::*;

const W_HYPE: f32 = 0.4;
const W_COZY: f32 = 0.5;

/// `hype` at exponent 0.4 and `cozy` at 0.5, pooling with each other at
/// 0.6 (one group); `music` stays additive.
fn k_feat() -> KFeatMatrix {
    let mut k = KFeatMatrix::default();
    for (a, b, v) in [
        ("hype", "hype", W_HYPE),
        ("cozy", "cozy", W_COZY),
        ("hype", "cozy", 0.6),
    ] {
        k.pairs.insert((AnchorId(a.into()), AnchorId(b.into())), v);
    }
    k
}

fn params() -> Params {
    Params {
        k_feat: k_feat(),
        time: Default::default(),
        calibration: Isotonic::default(), // identity, so S_e == V_e
        sign: SignMode::Split,
    }
}

fn palette() -> Palette {
    Palette {
        anchors: Vec::new(),
        k_feat: k_feat(),
    }
}

fn moment(anchor: &str, magnitude: f32) -> Atom {
    Atom {
        source: SourceId("s".into()),
        blend: vec![(AnchorId(anchor.into()), 1.0)],
        magnitude,
        time: TimeSupport::Moment,
    }
}

fn entry(id: &str) -> Entry {
    Entry {
        id: EntryId(id.into()),
        store: "test".into(),
        store_id: id.into(),
        title_cache: None,
    }
}

fn impact(id: &str, target: &str, atoms: Vec<Atom>) -> Impact {
    Impact {
        id: ImpactId(id.into()),
        target: EntryId(target.into()),
        atoms,
    }
}

fn rel(kind: RelKind, from: &str, to: &str, w: f32) -> Relation {
    Relation {
        kind,
        from: EntryId(from.into()),
        to: EntryId(to.into()),
        transform: Transform::Scalar(w),
    }
}

fn snapshot(entries: &[&str], impacts: Vec<Impact>, relations: Vec<Relation>) -> Snapshot {
    Snapshot {
        entries: entries.iter().map(|e| entry(e)).collect(),
        impacts,
        relations,
        palette: palette(),
    }
}

fn value_of(snap: &Snapshot, id: &str) -> f32 {
    score(snap, &params()).values[&EntryId(id.into())]
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-4 * a.abs().max(b.abs()).max(1.0)
}

/// An album scored through `Contains` over separate track entries equals
/// one entry holding the tracks' impacts directly.
#[test]
fn contains_flattens_to_the_same_score() {
    let grouped = snapshot(
        &["track1", "track2", "album"],
        vec![
            impact("i1", "track1", vec![moment("hype", 1.0)]),
            impact("i2", "track2", vec![moment("cozy", 1.5), moment("hype", 0.7)]),
        ],
        vec![
            rel(RelKind::Contains, "album", "track1", 1.0),
            rel(RelKind::Contains, "album", "track2", 1.0),
        ],
    );
    let flat = snapshot(
        &["flat"],
        vec![impact(
            "i",
            "flat",
            vec![moment("hype", 1.0), moment("cozy", 1.5), moment("hype", 0.7)],
        )],
        vec![],
    );
    let (album, flat_v) = (value_of(&grouped, "album"), value_of(&flat, "flat"));
    assert!(close(album, flat_v), "{album} vs {flat_v}");
}

/// Splitting invariance (A11): `a` contains `b`, `b` contains half of `x`,
/// and `a` contains the other half directly. `a` must score as if it
/// contained all of `x`.
#[test]
fn halves_reaching_by_two_paths_recombine() {
    let impacts = vec![impact("i", "x", vec![moment("hype", 2.0), moment("cozy", 1.0)])];
    let split = snapshot(
        &["x", "b", "a"],
        impacts.clone(),
        vec![
            rel(RelKind::Contains, "b", "x", 0.5),
            rel(RelKind::Contains, "a", "b", 1.0),
            rel(RelKind::Contains, "a", "x", 0.5),
        ],
    );
    let whole = snapshot(&["x", "a"], impacts, vec![rel(RelKind::Contains, "a", "x", 1.0)]);
    let (s, w) = (value_of(&split, "a"), value_of(&whole, "a"));
    assert!(close(s, w), "{s} vs {w}");
    assert!(close(w, value_of(&whole, "x")));
}

/// Splitting one impact into two same-anchor impacts in embedded units
/// (`e -> t e, (1 - t) e`) leaves the score unchanged.
#[test]
fn splitting_an_impact_in_embedded_units_is_invariant() {
    let m = 3.0f32;
    let t = 0.3f32;
    let whole = snapshot(&["e"], vec![impact("i", "e", vec![moment("hype", m)])], vec![]);
    let split = snapshot(
        &["e"],
        vec![
            impact("i1", "e", vec![moment("hype", t.powf(W_HYPE) * m)]),
            impact("i2", "e", vec![moment("hype", (1.0 - t).powf(W_HYPE) * m)]),
        ],
        vec![],
    );
    assert!(close(value_of(&whole, "e"), value_of(&split, "e")));
}

/// Repeats on one anchor pool as `l_(1/w)`: `n` equal hits give `a n^w`.
#[test]
fn repeats_grow_like_n_to_the_w() {
    let n = 9;
    let impacts = (0..n)
        .map(|i| impact(&format!("i{i}"), "e", vec![moment("hype", 1.0)]))
        .collect();
    let v = value_of(&snapshot(&["e"], impacts, vec![]), "e");
    assert!(close(v, (n as f32).powf(W_HYPE)), "{v}");
}

/// A franchise that contains an album and also, directly, the album's
/// track holds the track once.
#[test]
fn diamond_containment_counts_once() {
    let impacts = vec![impact("i", "track", vec![moment("hype", 2.0)])];
    let diamond = snapshot(
        &["track", "album", "franchise"],
        impacts.clone(),
        vec![
            rel(RelKind::Contains, "album", "track", 1.0),
            rel(RelKind::Contains, "franchise", "album", 1.0),
            rel(RelKind::Contains, "franchise", "track", 1.0),
        ],
    );
    let (f, t) = (value_of(&diamond, "franchise"), value_of(&diamond, "track"));
    assert!(close(f, t), "{f} vs {t}");
}

/// A mutual remix: each entry borrows back at most what it lends, and a
/// container of both scores as their flattened own mass.
#[test]
fn mutual_remix_has_no_double_count() {
    let r = 0.2f32;
    let snap = snapshot(
        &["a", "b", "c"],
        vec![
            impact("ia", "a", vec![moment("music", 1.0)]),
            impact("ib", "b", vec![moment("music", 1.0)]),
        ],
        vec![
            rel(RelKind::Remix, "a", "b", r),
            rel(RelKind::Remix, "b", "a", r),
            rel(RelKind::Contains, "c", "a", 1.0),
            rel(RelKind::Contains, "c", "b", 1.0),
        ],
    );
    // `music` is additive (w = 1): a = own 1 + r of b's own; its own mass
    // coming back through b is capped at all of it.
    assert!(close(value_of(&snap, "a"), 1.0 + r));
    assert!(close(value_of(&snap, "c"), 2.0));
}

/// An entry borrowing only through a relation gets the weight on its
/// embedded mass.
#[test]
fn borrow_only_entry_scales_embedded_mass() {
    let w = 0.2f32;
    let m = 3.0f32;
    let snap = snapshot(
        &["src", "dst"],
        vec![impact("i", "src", vec![moment("hype", m)])],
        vec![rel(RelKind::FeatureMusic, "dst", "src", w)],
    );
    let expect = (w * m.powf(1.0 / W_HYPE)).powf(W_HYPE);
    assert!(close(value_of(&snap, "dst"), expect));
}

/// Under `SignMode::Signed`, signed-pow keeps grouping consistent: a
/// sub-entry whose impacts cancel contributes nothing to its container.
#[test]
fn signed_cancelling_sub_entry_contributes_nothing() {
    let snap = snapshot(
        &["b", "a"],
        vec![
            impact("p", "b", vec![moment("hype", 1.0)]),
            impact("n", "b", vec![moment("hype", -1.0)]),
            impact("own", "a", vec![moment("hype", 1.0)]),
        ],
        vec![rel(RelKind::Contains, "a", "b", 1.0)],
    );
    let signed = Params {
        sign: SignMode::Signed,
        ..params()
    };
    let value = |id: &str| score(&snap, &signed).values[&EntryId(id.into())];
    assert!(close(value("b"), 0.0));
    assert!(close(value("a"), 1.0));
}

/// Under `SignMode::Split` a negative is an independent bad experience:
/// it costs its standalone value beside any amount of positive mass, on
/// its own anchor or another.
#[test]
fn split_negative_costs_its_standalone_value() {
    for (pos, neg) in [("hype", "hype"), ("hype", "cozy"), ("hype", "music")] {
        let snap = snapshot(
            &["e"],
            vec![
                impact("p", "e", vec![moment(pos, 10.0)]),
                impact("n", "e", vec![moment(neg, -3.0)]),
            ],
            vec![],
        );
        let v = value_of(&snap, "e");
        assert!(close(v, 7.0), "{pos}/{neg}: {v}");
    }
}

/// Positives pool among themselves and negatives subtract afterwards, so
/// a negative does not undo the positive of its size: `3 (+) 7 - 3 < 7`.
#[test]
fn split_pools_positives_before_subtracting() {
    let snap = snapshot(
        &["e"],
        vec![
            impact("a", "e", vec![moment("hype", 3.0)]),
            impact("b", "e", vec![moment("hype", 7.0)]),
            impact("n", "e", vec![moment("hype", -3.0)]),
        ],
        vec![],
    );
    let p = 1.0 / W_HYPE;
    let expect = (3.0f32.powf(p) + 7.0f32.powf(p)).powf(W_HYPE) - 3.0;
    assert!(close(value_of(&snap, "e"), expect));
}

/// A container borrows a sub-entry's negatives into its own negative
/// measure: they pool with its other negatives, not against its positives.
#[test]
fn split_borrowed_negatives_pool_with_negatives() {
    let snap = snapshot(
        &["b", "a"],
        vec![
            impact("p", "b", vec![moment("hype", 1.0)]),
            impact("n", "b", vec![moment("hype", -1.0)]),
            impact("own", "a", vec![moment("hype", 1.0)]),
        ],
        vec![rel(RelKind::Contains, "a", "b", 1.0)],
    );
    assert!(close(value_of(&snap, "b"), 0.0));
    assert!(close(value_of(&snap, "a"), 2.0f32.powf(W_HYPE) - 1.0));
}

/// Anchors in one group pool at the group exponent; additive anchors add.
#[test]
fn group_pooling_matches_the_nested_norm() {
    let snap = snapshot(
        &["e"],
        vec![impact(
            "i",
            "e",
            vec![moment("hype", 2.0), moment("cozy", 1.5), moment("music", 0.5)],
        )],
        vec![],
    );
    let p = 1.0 / 0.6f32;
    let expect = (2.0f32.powf(p) + 1.5f32.powf(p)).powf(0.6) + 0.5;
    assert!(close(value_of(&snap, "e"), expect));
}

/// A cycle elsewhere leaves an unconnected entry alone, and `explain`
/// agrees with `score` with shares that sum to the value.
#[test]
fn explain_agrees_with_score() {
    let snap = snapshot(
        &["a", "b", "solo"],
        vec![
            impact("ia", "a", vec![moment("hype", 1.0), moment("cozy", 2.0)]),
            impact("ib", "b", vec![moment("cozy", 1.0)]),
            impact("is", "solo", vec![moment("hype", 2.0)]),
        ],
        vec![rel(RelKind::Remix, "a", "b", 0.2), rel(RelKind::Remix, "b", "a", 0.2)],
    );
    assert!(close(value_of(&snap, "solo"), 2.0));
    let ex = explain(&snap, &params(), EntryId("a".into()));
    assert!(close(ex.value, value_of(&snap, "a")));
    let sum: f32 = ex.contributions.iter().map(|c| c.contribution).sum();
    assert!(close(sum, ex.value), "{sum} vs {}", ex.value);
}

/// Sources split each anchor's mass and contribution across the impacts
/// reaching it, own and borrowed, and sum back to both.
#[test]
fn explain_sources_sum_to_each_anchor() {
    let snap = snapshot(
        &["a", "b", "c"],
        vec![
            impact("ia1", "a", vec![moment("hype", 1.0), moment("cozy", 2.0)]),
            impact("ia2", "a", vec![moment("hype", 3.0)]),
            impact("ib", "b", vec![moment("cozy", 1.0), moment("music", 2.0)]),
            impact("ic", "c", vec![moment("music", 5.0)]),
        ],
        vec![
            rel(RelKind::Contains, "a", "b", 0.5),
            rel(RelKind::Remix, "b", "a", 0.2),
        ],
    );
    let ex = explain(&snap, &params(), EntryId("a".into()));
    for c in &ex.contributions {
        let of_anchor: Vec<_> = ex.sources.iter().filter(|s| s.anchor == c.anchor).collect();
        let embedded: f32 = of_anchor.iter().map(|s| s.embedded).sum();
        let contribution: f32 = of_anchor.iter().map(|s| s.contribution).sum();
        assert!(close(embedded, c.embedded), "{:?}: {embedded} vs {}", c.anchor, c.embedded);
        assert!(close(contribution, c.contribution), "{:?}", c.anchor);
    }
    let from = |impact: &str| -> Vec<&Source> {
        ex.sources.iter().filter(|s| s.impact.0 == impact).collect()
    };
    assert_eq!(from("ia2").len(), 1);
    assert!(from("ib").iter().all(|s| s.entry.0 == "b"));
    assert_eq!(from("ib").len(), 2, "b lends cozy and music");
    assert!(from("ic").is_empty(), "c is unrelated");
    let total: f32 = ex.sources.iter().map(|s| s.contribution).sum();
    assert!(close(total, ex.value));
}

/// Under `SignMode::Split`, `explain` nets each anchor's and impact's
/// positive part against its negative one, and everything still sums to
/// the value.
#[test]
fn split_explain_nets_signs_and_sums() {
    let snap = snapshot(
        &["a", "b"],
        vec![
            impact("mixed", "a", vec![moment("hype", 4.0), moment("hype", -1.0)]),
            impact("bad", "a", vec![moment("cozy", -2.0)]),
            impact("good", "a", vec![moment("cozy", 3.0), moment("music", 1.0)]),
            impact("ib", "b", vec![moment("hype", -2.0), moment("music", 2.0)]),
        ],
        vec![rel(RelKind::Contains, "a", "b", 0.5)],
    );
    let ex = explain(&snap, &params(), EntryId("a".into()));
    assert!(close(ex.value, value_of(&snap, "a")));
    let sum: f32 = ex.contributions.iter().map(|c| c.contribution).sum();
    assert!(close(sum, ex.value), "{sum} vs {}", ex.value);
    for c in &ex.contributions {
        let of_anchor: Vec<_> = ex.sources.iter().filter(|s| s.anchor == c.anchor).collect();
        let embedded: f32 = of_anchor.iter().map(|s| s.embedded).sum();
        let contribution: f32 = of_anchor.iter().map(|s| s.contribution).sum();
        assert!(close(embedded, c.embedded), "{:?}: {embedded} vs {}", c.anchor, c.embedded);
        assert!(close(contribution, c.contribution), "{:?}", c.anchor);
    }
    let rows = |impact: &str| ex.sources.iter().filter(|s| s.impact.0 == impact).count();
    assert_eq!(rows("mixed"), 1, "one netted hype row");
    assert!(ex.sources.iter().filter(|s| s.impact.0 == "bad").all(|s| s.contribution < 0.0));
}

/// An entry with no mass scores zero and has no contributions.
#[test]
fn empty_entry_scores_zero() {
    let snap = snapshot(&["e"], vec![], vec![]);
    assert_eq!(value_of(&snap, "e"), 0.0);
    assert!(explain(&snap, &params(), EntryId("e".into())).contributions.is_empty());
}

fn dated(anchor: &str, magnitude: f32, from: i32, to: i32) -> Atom {
    Atom {
        time: TimeSupport::Interval { from, to },
        ..moment(anchor, magnitude)
    }
}

fn value_with(snap: &Snapshot, id: &str, time: TimeParams) -> f32 {
    let p = Params { time, ..params() };
    score(snap, &p).values[&EntryId(id.into())]
}

/// A dated period split into two adjacent periods, its embedded mass
/// split by length, scores the same at any time setting.
#[test]
fn splitting_a_period_in_embedded_units_is_invariant() {
    let m = 3.0f32;
    let whole = snapshot(&["e"], vec![impact("i", "e", vec![dated("hype", m, 0, 100)])], vec![]);
    let split = snapshot(
        &["e"],
        vec![
            impact("i1", "e", vec![dated("hype", 0.3f32.powf(W_HYPE) * m, 0, 30)]),
            impact("i2", "e", vec![dated("hype", 0.7f32.powf(W_HYPE) * m, 30, 100)]),
        ],
        vec![],
    );
    for time in [TimeParams::default(), TimeParams { shared: 0.0, length_days: 20.0 }] {
        let (a, b) = (value_with(&whole, "e", time), value_with(&split, "e", time));
        assert!(close(a, b), "{a} vs {b}");
    }
}

/// With `shared = 1` time is ignored: dated mass scores as if undated.
#[test]
fn fully_shared_time_matches_undated_scoring() {
    let atoms = |d: bool| {
        vec![
            impact("a", "e", vec![if d { dated("hype", 1.0, 0, 30) } else { moment("hype", 1.0) }]),
            impact("b", "e", vec![if d { dated("hype", 2.0, 900, 990) } else { moment("hype", 2.0) }]),
        ]
    };
    let time = TimeParams { shared: 1.0, length_days: 30.0 };
    let (with_dates, without) = (
        value_with(&snapshot(&["e"], atoms(true), vec![]), "e", time),
        value_with(&snapshot(&["e"], atoms(false), vec![]), "e", time),
    );
    assert!(close(with_dates, without), "{with_dates} vs {without}");
}

/// Repeats far apart in time score higher than repeats at one moment,
/// and less than independent mass.
#[test]
fn distant_repeats_count_partly_new() {
    let snap = |gap: i32| {
        snapshot(
            &["e"],
            vec![
                impact("a", "e", vec![dated("hype", 1.0, 0, 1)]),
                impact("b", "e", vec![dated("hype", 1.0, gap, gap + 1)]),
            ],
            vec![],
        )
    };
    let time = TimeParams { shared: 0.7, length_days: 30.0 };
    let (near, far) = (value_with(&snap(0), "e", time), value_with(&snap(2000), "e", time));
    // Two hits on one day are one hit with their embedded masses added.
    let one = snapshot(
        &["e"],
        vec![impact("a", "e", vec![dated("hype", 2f32.powf(W_HYPE), 0, 1)])],
        vec![],
    );
    assert!(close(near, value_with(&one, "e", time)), "{near}");
    assert!(far > near && far < 2.0, "{far}");
}

/// Splitting invariance through relations holds for dated mass too.
#[test]
fn dated_halves_reaching_by_two_paths_recombine() {
    let impacts = vec![impact("i", "x", vec![dated("hype", 2.0, 0, 200)])];
    let split = snapshot(
        &["x", "b", "a"],
        impacts.clone(),
        vec![
            rel(RelKind::Contains, "b", "x", 0.5),
            rel(RelKind::Contains, "a", "b", 1.0),
            rel(RelKind::Contains, "a", "x", 0.5),
        ],
    );
    let whole = snapshot(&["x", "a"], impacts, vec![rel(RelKind::Contains, "a", "x", 1.0)]);
    let time = TimeParams { shared: 0.5, length_days: 30.0 };
    let (s, w) = (value_with(&split, "a", time), value_with(&whole, "a", time));
    assert!(close(s, w), "{s} vs {w}");
}
