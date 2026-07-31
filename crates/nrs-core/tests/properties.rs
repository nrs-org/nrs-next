//! Black-box property tests against the public `score`/`explain`
//! contract only (INFRA.md §4.1: "trustworthy ⇒ property-testable").
//! Each test encodes one proof from the redesign notes.

use nrs_core::*;

fn params(q: f32) -> Params {
    Params {
        q,
        k_src_levels: KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        },
        k_feat: KFeatMatrix::default(),
        homogeneity: Homogeneity::DegreeQ,
        calibration: Isotonic::default(), // identity, so S_e == V_e
        relation_scaling: RelationScaling::Fixed,
    }
}

fn palette() -> Palette {
    Palette {
        anchors: Vec::new(),
        k_feat: KFeatMatrix::default(),
    }
}

fn moment(source: &str, anchor: &str, magnitude: f32) -> Atom {
    Atom {
        source: SourceId(source.into()),
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

/// Notes, "splittability => additivity in time": chopping a tail's
/// time-support at any point and re-scoring must agree with the whole.
#[test]
fn splitting_a_tail_leaves_the_score_unchanged() {
    let whole_atom = Atom {
        source: SourceId("s".into()),
        blend: vec![(AnchorId("a".into()), 1.0)],
        magnitude: 1.0,
        time: TimeSupport::Tail {
            peak: 2.0,
            lambda: 0.5,
            cutoff: 4.0,
        },
    };
    let split_at = 1.7f32;
    let remainder_peak = 2.0 * (-0.5 * split_at).exp();
    let first_half = Atom {
        time: TimeSupport::Tail {
            peak: 2.0,
            lambda: 0.5,
            cutoff: split_at,
        },
        ..whole_atom.clone()
    };
    let second_half = Atom {
        time: TimeSupport::Tail {
            peak: remainder_peak,
            lambda: 0.5,
            cutoff: 4.0 - split_at,
        },
        ..whole_atom.clone()
    };

    let snap = |atoms: Vec<Atom>| Snapshot {
        entries: vec![entry("e")],
        impacts: vec![Impact {
            id: ImpactId("i".into()),
            target: EntryId("e".into()),
            atoms,
        }],
        relations: vec![],
        palette: palette(),
        taxonomy: Taxonomy::default(),
    };

    let p = params(0.7);
    let whole_score = score(&snap(vec![whole_atom]), &p);
    let split_score = score(&snap(vec![first_half, second_half]), &p);

    let e = EntryId("e".into());
    let a = whole_score.values[&e];
    let b = split_score.values[&e];
    assert!((a - b).abs() < 1e-4, "{a} vs {b}");
}

/// Notes, "Contains, and why flattening now works": an album's score,
/// computed via `Contains` relations over separate track entries, must
/// equal the score of one entry holding the tracks' impacts directly.
#[test]
fn contains_flattens_to_the_same_score() {
    let grouped = Snapshot {
        entries: vec![entry("track1"), entry("track2"), entry("album")],
        impacts: vec![
            Impact {
                id: ImpactId("i1".into()),
                target: EntryId("track1".into()),
                atoms: vec![moment("char1", "gut-punch", 1.0)],
            },
            Impact {
                id: ImpactId("i2".into()),
                target: EntryId("track2".into()),
                atoms: vec![moment("char2", "cozy-warmth", 1.5)],
            },
        ],
        relations: vec![
            Relation {
                kind: RelKind::Contains,
                from: EntryId("album".into()),
                to: EntryId("track1".into()),
                transform: Transform::Scalar(1.0),
            },
            Relation {
                kind: RelKind::Contains,
                from: EntryId("album".into()),
                to: EntryId("track2".into()),
                transform: Transform::Scalar(1.0),
            },
        ],
        palette: palette(),
        taxonomy: Taxonomy::default(),
    };
    let flat = Snapshot {
        entries: vec![entry("flat")],
        impacts: vec![Impact {
            id: ImpactId("i".into()),
            target: EntryId("flat".into()),
            atoms: vec![
                moment("char1", "gut-punch", 1.0),
                moment("char2", "cozy-warmth", 1.5),
            ],
        }],
        relations: vec![],
        palette: palette(),
        taxonomy: Taxonomy::default(),
    };

    let p = params(0.6);
    let grouped_scores = score(&grouped, &p);
    let flat_scores = score(&flat, &p);

    let album = grouped_scores.values[&EntryId("album".into())];
    let flat_v = flat_scores.values[&EntryId("flat".into())];
    assert!((album - flat_v).abs() < 1e-4, "{album} vs {flat_v}");
}

/// A mutual remix (fraction `r`) of `a` and `b`, both pooled into
/// container `c` via `Contains` - the exact setup Prop. "the
/// double-count is identified and vanishes with saturation" analyzes.
fn mutual_remix_snapshot(r: f32) -> Snapshot {
    Snapshot {
        entries: vec![entry("a"), entry("b"), entry("c")],
        impacts: vec![
            Impact {
                id: ImpactId("ia".into()),
                target: EntryId("a".into()),
                atoms: vec![moment("src-a", "anchor-a", 1.0)],
            },
            Impact {
                id: ImpactId("ib".into()),
                target: EntryId("b".into()),
                atoms: vec![moment("src-b", "anchor-b", 1.0)],
            },
        ],
        relations: vec![
            Relation {
                kind: RelKind::Remix,
                from: EntryId("a".into()),
                to: EntryId("b".into()),
                transform: Transform::Scalar(r),
            },
            Relation {
                kind: RelKind::Remix,
                from: EntryId("b".into()),
                to: EntryId("a".into()),
                transform: Transform::Scalar(r),
            },
            Relation {
                kind: RelKind::Contains,
                from: EntryId("c".into()),
                to: EntryId("a".into()),
                transform: Transform::Scalar(1.0),
            },
            Relation {
                kind: RelKind::Contains,
                from: EntryId("c".into()),
                to: EntryId("b".into()),
                transform: Transform::Scalar(1.0),
            },
        ],
        palette: palette(),
        taxonomy: Taxonomy::default(),
    }
}

/// Notes Prop. "the double-count is identified and vanishes with
/// saturation": a mutual remix (fraction `r`) inside a container scores
/// `V(C) = 2(1-r)^-q`, strictly between `2` (flattened, `q -> 0`) and
/// `2/(1-r)` (the redundancy-blind `q = 1` corner, the old system).
#[test]
fn mutual_remix_container_matches_double_count_formula() {
    let r = 0.2f32;
    let snap = mutual_remix_snapshot(r);

    let c = EntryId("c".into());
    let mut previous = None;
    for &q in &[0.01f32, 0.25, 0.5, 0.75, 1.0] {
        let v = score(&snap, &params(q)).values[&c];
        let expected = 2.0 * (1.0 - r).powf(-q);
        assert!((v - expected).abs() < 1e-3, "q={q}: {v} vs {expected}");
        if let Some(prev) = previous {
            assert!(v > prev, "V(C) should increase with q: {prev} then {v}");
        }
        previous = Some(v);
    }

    // As q -> 0+, V(C) -> 2 (the flattened, saturation-max limit).
    let near_zero = score(&snap, &params(1e-4)).values[&c];
    assert!((near_zero - 2.0).abs() < 1e-2, "{near_zero}");

    // At q = 1 (redundancy-blind), V(C) is the old system's 2/(1-r).
    let at_one = score(&snap, &params(1.0)).values[&c];
    assert!((at_one - 2.0 / (1.0 - r)).abs() < 1e-3, "{at_one}");
}

/// `RelationScaling::PowQ`'s closed form on the same setup:
/// `V(C) = 2(1 - r^(1/q))^-q`. Confirms it matches `Fixed` exactly at
/// `q=1` (r^(1/1)=r, no change from the old system), matches the same
/// `q->0` flattening limit, and - the point of the reparametrization -
/// is `<= Fixed`'s value for every `q` in `(0, 1]` (proved as a
/// separate theorem in redesign-tex's notes, right after Prop. "the
/// double-count is identified and vanishes with saturation").
#[test]
fn powq_relation_scaling_matches_its_closed_form_and_never_exceeds_fixed() {
    let r = 0.2f32;
    let snap = mutual_remix_snapshot(r);
    let c = EntryId("c".into());

    for &q in &[0.05f32, 0.25, 0.5, 0.75, 1.0] {
        let mut p = params(q);
        p.relation_scaling = RelationScaling::PowQ;
        let v = score(&snap, &p).values[&c];
        let r_prime = r.powf(1.0 / q);
        let expected = 2.0 * (1.0 - r_prime).powf(-q);
        assert!((v - expected).abs() < 1e-2, "q={q}: {v} vs {expected}");

        let mut fixed_p = params(q);
        fixed_p.relation_scaling = RelationScaling::Fixed;
        let v_fixed = score(&snap, &fixed_p).values[&c];
        assert!(
            v <= v_fixed + 1e-4,
            "q={q}: PowQ {v} should be <= Fixed {v_fixed}"
        );
    }

    // Exact match at q=1 (r^(1/1) = r, identical to Fixed).
    let mut p1 = params(1.0);
    p1.relation_scaling = RelationScaling::PowQ;
    let v1 = score(&snap, &p1).values[&c];
    let mut fixed_p1 = params(1.0);
    fixed_p1.relation_scaling = RelationScaling::Fixed;
    let v1_fixed = score(&snap, &fixed_p1).values[&c];
    assert!((v1 - v1_fixed).abs() < 1e-3, "{v1} vs {v1_fixed}");

    // Same q -> 0 flattening limit as Fixed.
    let mut p0 = params(1e-4);
    p0.relation_scaling = RelationScaling::PowQ;
    let near_zero = score(&snap, &p0).values[&c];
    assert!((near_zero - 2.0).abs() < 1e-2, "{near_zero}");
}

/// `Contains`'s weight (1.0) must be unaffected by `PowQ` at any `q`
/// (`1^x = 1`) - only genuinely-diluted relations are meant to be
/// touched, confirmed directly against `contains_flattens_to_the_same_score`'s
/// setup with PowQ turned on.
#[test]
fn powq_leaves_full_weight_contains_unaffected() {
    let grouped = Snapshot {
        entries: vec![entry("track1"), entry("track2"), entry("album")],
        impacts: vec![
            Impact {
                id: ImpactId("i1".into()),
                target: EntryId("track1".into()),
                atoms: vec![moment("char1", "gut-punch", 1.0)],
            },
            Impact {
                id: ImpactId("i2".into()),
                target: EntryId("track2".into()),
                atoms: vec![moment("char2", "cozy-warmth", 1.5)],
            },
        ],
        relations: vec![
            Relation {
                kind: RelKind::Contains,
                from: EntryId("album".into()),
                to: EntryId("track1".into()),
                transform: Transform::Scalar(1.0),
            },
            Relation {
                kind: RelKind::Contains,
                from: EntryId("album".into()),
                to: EntryId("track2".into()),
                transform: Transform::Scalar(1.0),
            },
        ],
        palette: palette(),
        taxonomy: Taxonomy::default(),
    };

    let album = EntryId("album".into());
    for &q in &[0.05f32, 0.3, 0.6, 1.0, 1.5] {
        let mut fixed_p = params(q);
        fixed_p.relation_scaling = RelationScaling::Fixed;
        let mut powq_p = params(q);
        powq_p.relation_scaling = RelationScaling::PowQ;

        let v_fixed = score(&grouped, &fixed_p).values[&album];
        let v_powq = score(&grouped, &powq_p).values[&album];
        assert!(
            (v_fixed - v_powq).abs() < 1e-4,
            "q={q}: {v_fixed} vs {v_powq}"
        );
    }
}

/// Same claim as `contains_flattens_to_the_same_score`, two hops deep:
/// a best-of containing two albums, each containing two tracks, must
/// score exactly like one entry holding all four tracks' impacts
/// directly. Flattening has to hold transitively, not just one level
/// of `Contains` at a time.
#[test]
fn nested_contains_flattens_transitively() {
    let track_impacts = [
        ("track1", "char1", "gut-punch", 1.0),
        ("track2", "char2", "cozy-warmth", 1.5),
        ("track3", "char3", "clever-premise", 0.8),
        ("track4", "char4", "gorgeous-visuals", 1.2),
    ];

    let mut entries = vec![entry("albumA"), entry("albumB"), entry("bestof")];
    let mut impacts = Vec::new();
    let mut relations = vec![
        Relation {
            kind: RelKind::Contains,
            from: EntryId("bestof".into()),
            to: EntryId("albumA".into()),
            transform: Transform::Scalar(1.0),
        },
        Relation {
            kind: RelKind::Contains,
            from: EntryId("bestof".into()),
            to: EntryId("albumB".into()),
            transform: Transform::Scalar(1.0),
        },
    ];
    for (i, &(track, character, anchor, mag)) in track_impacts.iter().enumerate() {
        entries.push(entry(track));
        impacts.push(Impact {
            id: ImpactId(format!("i{i}")),
            target: EntryId(track.into()),
            atoms: vec![moment(character, anchor, mag)],
        });
        let album = if i < 2 { "albumA" } else { "albumB" };
        relations.push(Relation {
            kind: RelKind::Contains,
            from: EntryId(album.into()),
            to: EntryId(track.into()),
            transform: Transform::Scalar(1.0),
        });
    }
    let grouped = Snapshot {
        entries,
        impacts,
        relations,
        palette: palette(),
        taxonomy: Taxonomy::default(),
    };

    let flat = Snapshot {
        entries: vec![entry("flat")],
        impacts: vec![Impact {
            id: ImpactId("i".into()),
            target: EntryId("flat".into()),
            atoms: track_impacts
                .iter()
                .map(|&(_, character, anchor, mag)| moment(character, anchor, mag))
                .collect(),
        }],
        relations: vec![],
        palette: palette(),
        taxonomy: Taxonomy::default(),
    };

    let p = params(0.5);
    let bestof = score(&grouped, &p).values[&EntryId("bestof".into())];
    let flat_v = score(&flat, &p).values[&EntryId("flat".into())];
    assert!((bestof - flat_v).abs() < 1e-4, "{bestof} vs {flat_v}");
}

/// `explain` and `score` must agree (they share the same phase 3
/// computation), and a `Contribution` breakdown must literally sum to
/// the entry's value — `explain` is a view onto `score`'s arithmetic,
/// not a separate computation.
#[test]
fn explain_agrees_with_score_and_contributions_sum_to_value() {
    let e = EntryId("e".into());
    // Two related sources (same parent) sharing an anchor, so there is
    // real crowding to break down, not just a single isolated cell.
    let mut taxonomy = Taxonomy::default();
    taxonomy
        .parent
        .insert(SourceId("char1".into()), SourceId("parent".into()));
    taxonomy
        .parent
        .insert(SourceId("char2".into()), SourceId("parent".into()));

    let snap = Snapshot {
        entries: vec![entry("e")],
        impacts: vec![Impact {
            id: ImpactId("i".into()),
            target: e.clone(),
            atoms: vec![
                moment("char1", "gut-punch", 2.0),
                moment("char2", "gut-punch", 3.0),
            ],
        }],
        relations: vec![],
        palette: palette(),
        taxonomy,
    };

    let p = params(0.5);
    let explanation = explain(&snap, &p, e.clone());
    assert_eq!(explanation.entry, e);
    assert_eq!(explanation.contributions.len(), 2);

    let summed: f32 = explanation
        .contributions
        .iter()
        .map(|c| c.contribution)
        .sum();
    assert!(
        (summed - explanation.value).abs() < 1e-5,
        "{summed} vs {}",
        explanation.value
    );
    assert_eq!(explanation.score, p.calibration.apply(explanation.value));

    let scored = score(&snap, &p).values[&e];
    assert!(
        (scored - explanation.value).abs() < 1e-5,
        "{scored} vs {}",
        explanation.value
    );
}

/// Coincident opposite-signed atoms merge to net zero mass (they're
/// literally the same cell), which must drop out of both `explain`'s
/// breakdown and `score`'s output cleanly — no NaNs or leftover
/// zero-mass cells from raising zero to a fractional power.
#[test]
fn zero_net_measure_entry_has_no_contributions_and_scores_zero() {
    let e = EntryId("e".into());
    let snap = Snapshot {
        entries: vec![entry("e"), entry("untouched")],
        impacts: vec![Impact {
            id: ImpactId("i".into()),
            target: e.clone(),
            atoms: vec![
                moment("char", "gut-punch", 2.0),
                moment("char", "gut-punch", -2.0),
            ],
        }],
        relations: vec![],
        palette: palette(),
        taxonomy: Taxonomy::default(),
    };

    let p = params(0.5);
    let explanation = explain(&snap, &p, e.clone());
    assert!(explanation.contributions.is_empty());
    assert_eq!(explanation.value, 0.0);

    let scores = score(&snap, &p);
    assert_eq!(scores.values[&e], 0.0);
    // An entry with no impacts at all must still show up, at zero.
    assert_eq!(scores.values[&EntryId("untouched".into())], 0.0);
}

/// An entry that remixes *itself* is a genuine 1-node cycle: the
/// borrowing solve must still converge to the geometric fixed point
/// `f = m / (1 - r)` rather than looping or panicking.
#[test]
fn self_remix_reaches_the_geometric_fixed_point() {
    let r = 0.3f32;
    let snap = Snapshot {
        entries: vec![entry("a")],
        impacts: vec![Impact {
            id: ImpactId("i".into()),
            target: EntryId("a".into()),
            atoms: vec![moment("s", "x", 1.0)],
        }],
        relations: vec![Relation {
            kind: RelKind::Remix,
            from: EntryId("a".into()),
            to: EntryId("a".into()),
            transform: Transform::Scalar(r),
        }],
        palette: palette(),
        taxonomy: Taxonomy::default(),
    };

    // q = 1 isolates the relation solve from the redundancy functional
    // (V = sum of mass there), since that's what this test targets.
    let v = score(&snap, &params(1.0)).values[&EntryId("a".into())];
    let expected = 1.0 / (1.0 - r);
    assert!((v - expected).abs() < 1e-4, "{v} vs {expected}");
}

/// A relation cycle elsewhere in the graph must not leak into an
/// entirely disconnected entry's score.
#[test]
fn unconnected_entry_is_unaffected_by_a_cycle_elsewhere() {
    let r = 0.2f32;
    let snap = Snapshot {
        entries: vec![entry("a"), entry("b"), entry("solo")],
        impacts: vec![
            Impact {
                id: ImpactId("ia".into()),
                target: EntryId("a".into()),
                atoms: vec![moment("src-a", "anchor-a", 1.0)],
            },
            Impact {
                id: ImpactId("ib".into()),
                target: EntryId("b".into()),
                atoms: vec![moment("src-b", "anchor-b", 1.0)],
            },
            Impact {
                id: ImpactId("isolo".into()),
                target: EntryId("solo".into()),
                atoms: vec![moment("src-solo", "anchor-solo", 5.0)],
            },
        ],
        relations: vec![
            Relation {
                kind: RelKind::Remix,
                from: EntryId("a".into()),
                to: EntryId("b".into()),
                transform: Transform::Scalar(r),
            },
            Relation {
                kind: RelKind::Remix,
                from: EntryId("b".into()),
                to: EntryId("a".into()),
                transform: Transform::Scalar(r),
            },
        ],
        palette: palette(),
        taxonomy: Taxonomy::default(),
    };

    let solo = score(&snap, &params(1.0)).values[&EntryId("solo".into())];
    assert!((solo - 5.0).abs() < 1e-5, "{solo}");
}

/// An entry with no impacts of its own, only a borrow, must resolve to
/// exactly the scaled source value — pure pass-through.
#[test]
fn borrow_only_entry_gets_exactly_the_scaled_source_value() {
    let snap = Snapshot {
        entries: vec![entry("source_entry"), entry("receiver")],
        impacts: vec![Impact {
            id: ImpactId("i".into()),
            target: EntryId("source_entry".into()),
            atoms: vec![moment("s", "x", 4.0)],
        }],
        relations: vec![Relation {
            kind: RelKind::Remix,
            from: EntryId("receiver".into()),
            to: EntryId("source_entry".into()),
            transform: Transform::Scalar(0.5),
        }],
        palette: palette(),
        taxonomy: Taxonomy::default(),
    };

    let receiver = score(&snap, &params(1.0)).values[&EntryId("receiver".into())];
    assert!((receiver - 2.0).abs() < 1e-5, "{receiver}");
}

/// redesign-tex Theorem "dilution survives to the breadth limit": with
/// `K` owned, mutually non-redundant atoms and `N` atoms borrowed at a
/// common dilution `w` (also mutually non-redundant, from `N`
/// distinct one-atom sources), `V -> K + N` under `Fixed` and
/// `V -> K + w*N` under `PowQ` as `q -> 0+`.
#[test]
fn breadth_limit_matches_k_plus_n_fixed_and_k_plus_wn_powq() {
    let k = 2;
    let n = 5;
    let w = 0.1f32;

    let mut entries = vec![entry("owner"), entry("borrower")];
    let mut impacts = vec![Impact {
        id: ImpactId("io1".into()),
        target: EntryId("owner".into()),
        atoms: vec![moment("owner-src", "owner-anchor-1", 3.0)],
    }];
    // A second owned atom at a distinct source/anchor (k=2 total).
    impacts.push(Impact {
        id: ImpactId("io2".into()),
        target: EntryId("owner".into()),
        atoms: vec![moment("owner-src2", "owner-anchor-2", 5.0)],
    });
    assert_eq!(k, 2);

    let mut relations = Vec::new();
    for i in 0..n {
        let src_id = format!("src{i}");
        entries.push(entry(&src_id));
        impacts.push(Impact {
            id: ImpactId(format!("is{i}")),
            target: EntryId(src_id.clone()),
            atoms: vec![moment(
                &format!("src-src-{i}"),
                &format!("src-anchor-{i}"),
                (i + 1) as f32,
            )],
        });
        relations.push(Relation {
            kind: RelKind::KilledBy,
            from: EntryId("borrower".into()),
            to: EntryId(src_id),
            transform: Transform::Scalar(w),
        });
    }

    let snap = Snapshot {
        entries,
        impacts,
        relations,
        palette: palette(),
        taxonomy: Taxonomy::default(),
    };

    let owner = EntryId("owner".into());
    let borrower = EntryId("borrower".into());
    // Not q -> 0 literally: PowQ's own reparametrized weight w^(1/q)
    // is an f32 intermediate, and for q much below this it underflows
    // to exact 0.0 before ever reaching the redundancy functional
    // (e.g. at q=1e-4, 0.1^(1/1e-4) = 0.1^10000, far past f32's ~1e-45
    // floor) - silently losing the small-but-nonzero true limit rather
    // than approximating it. q=0.1 is comfortably inside the range
    // already validated against the real dataset (nrs-app) and still
    // small enough to show clear convergence toward the theorem's
    // predicted limits.
    let q = 0.1;

    let mut fixed_p = params(q);
    fixed_p.relation_scaling = RelationScaling::Fixed;
    let v_owner = score(&snap, &fixed_p).values[&owner];
    let v_borrower_fixed = score(&snap, &fixed_p).values[&borrower];

    let mut powq_p = params(q);
    powq_p.relation_scaling = RelationScaling::PowQ;
    let v_borrower_powq = score(&snap, &powq_p).values[&borrower];

    // Not yet at the q->0 limit, but trending the right way: Fixed's
    // borrower is already closer to N=5 than to 0, PowQ's borrower is
    // already closer to w*N=0.5 than to N=5.
    assert!(
        v_borrower_fixed > 3.0,
        "borrower/fixed should trend toward N=5: {v_borrower_fixed}"
    );
    assert!(
        v_borrower_powq < 1.0,
        "borrower/powq should trend toward w*N=0.5: {v_borrower_powq}"
    );

    // The rank-swap corollary: N=5 > K=2, so Fixed's dilution-blind
    // breadth limit lets the borrower (each connection only 10%
    // strength) overtake the owner - the exact instability the PowQ
    // knob was added to fix. w*N=0.5 < K=2, so PowQ preserves the
    // sensible ordering.
    assert!(
        v_borrower_fixed > v_owner,
        "Fixed should let breadth overtake depth here: {v_borrower_fixed} vs {v_owner}"
    );
    assert!(
        v_borrower_powq < v_owner,
        "PowQ should preserve the sensible ordering: {v_borrower_powq} vs {v_owner}"
    );
}
