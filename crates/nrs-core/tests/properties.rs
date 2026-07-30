//! Black-box property tests against the public `score`/`explain`
//! contract only (INFRA.md §4.1: "trustworthy ⇒ property-testable").
//! Each test encodes one proof from the redesign notes.

use nrs_core::*;

fn params(q: f32) -> Params {
    Params {
        q,
        sigma: 1.0,
        k_src_levels: KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        },
        k_feat: KFeatMatrix::default(),
        homogeneity: Homogeneity::DegreeQ,
        calibration: Isotonic::default(), // identity, so S_e == V_e
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

/// Notes Prop. "the double-count is identified and vanishes with
/// saturation": a mutual remix (fraction `r`) inside a container scores
/// `V(C) = 2(1-r)^-q`, strictly between `2` (flattened, `q -> 0`) and
/// `2/(1-r)` (the redundancy-blind `q = 1` corner, the old system).
#[test]
fn mutual_remix_container_matches_double_count_formula() {
    let r = 0.2f32;
    let snap = Snapshot {
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
    };

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
