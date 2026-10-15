//! End-to-end: parse an nrsml-shaped document, build a `Snapshot`, and
//! confirm `nrs-core` can actually score it without panicking - the
//! real point of having a parser at all.

use nrs_core::*;
use nrs_import::{build_snapshot, write_snapshot, xml::parse_str};
use nrs_store::Store;
use time::macros::date;

fn params() -> Params {
    Params {
        k_feat: nrs_import::anchors::legacy_k_feat(),
        time: Default::default(),
        calibration: Isotonic::default(),
        sign: Default::default(),
    }
}

/// A trimmed excerpt shaped like the real `Clannad.xml`: a franchise
/// containing two works, one with a multi-emotion `nei`, the other
/// with a `killedBy` relation (referencing the first).
const DOC: &str = r#"<document>
    <entry id="F-VGMDB-1018" title="CLANNAD">
        <entry id="A-MAL-2167" title="Clannad">
            <source><mal id="2167" /></source>
            <bestGirl name="Nagisa Furukawa" />
            <animeConsumedProgress status="Completed" boredom="0.8" episodes="23" />
            <visual type="animated" base="0.35" unique="0.75" />
            <nei base="0.3" emotions="CU-0.5:CP-0.5" />
            <writing character="0.5" story="0.5" pacing="0.8" originality="0.8" />
        </entry>
        <entry id="A-MAL-4181" title="Clannad: After Story">
            <killedBy id="A-MAL-2167" potential="0.3" effect="1.0" />
            <animeConsumedProgress status="Completed" boredom="0.95" episodes="24" />
            <cry emotions="MU" />
        </entry>
        <nei base="0.6" emotions="AP">
            <contributor id="A-MAL-2167" factor="0.7" />
            <contributor id="A-MAL-4181" factor="0.3" />
        </nei>
    </entry>
</document>"#;

#[test]
fn parses_and_scores_a_franchise_shaped_document() {
    let doc = parse_str(DOC, date!(2026 - 07 - 31)).expect("parse");
    assert_eq!(doc.entries.len(), 3);

    let (snapshot, _extras, conversion_warnings) = build_snapshot(&doc);
    assert!(conversion_warnings.is_empty(), "{conversion_warnings:?}");
    assert_eq!(snapshot.entries.len(), 3);

    // Franchise -> both works via nested-entry Contains.
    let franchise = EntryId("F-VGMDB-1018".into());
    let contains_count = snapshot
        .relations
        .iter()
        .filter(|r| r.kind == RelKind::Contains && r.from == franchise)
        .count();
    assert_eq!(contains_count, 2);

    // killedBy on the second work referencing the first.
    assert!(snapshot
        .relations
        .iter()
        .any(|r| r.kind == RelKind::KilledBy
            && r.from == EntryId("A-MAL-4181".into())
            && r.to == EntryId("A-MAL-2167".into())));

    // The whole thing must actually score without panicking, and every
    // entry (including the franchise, which has no impacts of its own)
    // must appear.
    let scores = score(&snapshot, &params());
    assert_eq!(scores.values.len(), 3);
    for (entry, value) in &scores.values {
        assert!(value.is_finite(), "{entry:?} scored {value}");
    }

    // The franchise borrows both works' measures via Contains, so its
    // score must be nonzero (it has no direct impacts of its own).
    assert!(scores.values[&franchise] != 0.0);
}

/// Roadmap phase 3's "-> nrs.db" half (INFRA.md §12): the same franchise
/// fixture, written into a store and reloaded, must score identically to
/// scoring the freshly-built in-memory `Snapshot` directly.
#[test]
fn write_snapshot_then_load_snapshot_scores_identically() {
    let doc = parse_str(DOC, date!(2026 - 07 - 31)).expect("parse");
    let (snapshot, extras, conversion_warnings) = build_snapshot(&doc);
    assert!(conversion_warnings.is_empty(), "{conversion_warnings:?}");

    let mut store = Store::open_in_memory().expect("open store");
    write_snapshot(&mut store, &snapshot, &extras).expect("write snapshot");

    let (reloaded, _params) = store.load_snapshot().expect("load snapshot");
    assert_eq!(reloaded.entries.len(), snapshot.entries.len());
    assert_eq!(reloaded.impacts.len(), snapshot.impacts.len());
    assert_eq!(reloaded.relations.len(), snapshot.relations.len());

    let original_scores = score(&snapshot, &params());
    let reloaded_scores = score(&reloaded, &params());
    assert_eq!(original_scores, reloaded_scores);
}

#[test]
fn include_resolves_relative_to_the_including_file() {
    let dir = std::env::temp_dir().join(format!("nrs-import-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.xml"),
        r#"<document><include src="child.xml" /></document>"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("child.xml"),
        r#"<document><entry id="A-1" title="A"><cry emotions="MU" /></entry></document>"#,
    )
    .unwrap();

    let doc =
        nrs_import::xml::parse_file(&dir.join("main.xml"), date!(2026 - 07 - 31)).expect("parse");
    assert_eq!(doc.entries.len(), 1);
    assert_eq!(doc.entries[0].id, "A-1");

    std::fs::remove_dir_all(&dir).ok();
}

/// Shaped like the real `Cue.xml`: a track crediting an artist with
/// `vocal+image`, both nested inside an album/franchise. The artist
/// must end up borrowing a role-weighted (not full) share of the
/// track via an auto-generated Credit relation (not Contains -- role
/// credits aren't structural containment, see `RelKind`'s doc comment),
/// and the whole thing must still score finitely.
#[test]
fn role_credit_generates_a_weighted_credit_relation() {
    let doc = parse_str(
        r#"<document>
            <entry id="M-ARTIST" title="AiRBLUE" />
            <entry id="M-ALBUM" title="Album">
                <entry id="$-1" title="Track">
                    <music base="0.7" />
                    <role id="M-ARTIST" roles="vocal+image" />
                </entry>
            </entry>
        </document>"#,
        date!(2026 - 07 - 31),
    )
    .expect("parse");
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);

    let (snapshot, _extras, warnings) = build_snapshot(&doc);
    assert!(warnings.is_empty(), "{warnings:?}");

    let artist = EntryId("M-ARTIST".into());
    // "$-1" is a positional placeholder, unique only within its
    // containing entry - transformId replaces "$" with the enclosing
    // entry's id ("M-ALBUM"), giving "M-ALBUM-1".
    let track = EntryId("M-ALBUM-1".into());
    assert!(
        snapshot.entries.iter().any(|e| e.id == track),
        "track id should be resolved to M-ALBUM-1"
    );
    let role_relation = snapshot
        .relations
        .iter()
        .find(|r| r.kind == RelKind::Credit && r.from == artist && r.to == track)
        .expect("role-generated relation");

    // vocal+image is not a full (Scalar(1.0)) borrow - it must be a
    // genuine, non-trivial per-anchor weighting.
    match &role_relation.transform {
        Transform::Regional(map) => assert!(!map.is_empty()),
        Transform::Scalar(v) => assert!(
            (*v - 1.0).abs() > 1e-6,
            "expected a partial credit, got full 1.0"
        ),
        Transform::Transport(_) => panic!("role weights should never produce a Transport"),
    }

    let scores = score(&snapshot, &params());
    for (entry, value) in &scores.values {
        assert!(value.is_finite(), "{entry:?} scored {value}");
    }
    assert!(
        scores.values[&artist] != 0.0,
        "artist should have borrowed a nonzero share"
    );
}

/// End-to-end through build_snapshot: the exact Koikake.xml pattern
/// (a <script> inside an <aei> splitting the impact's contribution
/// between the entry itself and a fixed external id) must produce two
/// real Impacts, one per contributor, both scoring finitely.
#[cfg(feature = "scripting")]
#[test]
fn script_split_contributors_reach_the_snapshot() {
    let doc = parse_str(
        r#"<document>
            <entry id="A-MAL-1" title="A">
                <aei base="0.5" emotions="MP">
                    <script>
                        this.value[0].contributors.set(this.parent.entry.value.id, this.root.context.api.newScalarMatrix(0.7));
                        this.value[0].contributors.set("V-VNDB-1", this.root.context.api.newScalarMatrix(0.3));
                    </script>
                </aei>
            </entry>
            <entry id="V-VNDB-1" title="V" />
        </document>"#,
        date!(2026 - 07 - 31),
    )
    .expect("parse");
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);

    let (snapshot, _extras, warnings) = build_snapshot(&doc);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(snapshot.impacts.len(), 2);

    let a = EntryId("A-MAL-1".into());
    let v = EntryId("V-VNDB-1".into());
    assert!(snapshot.impacts.iter().any(|i| i.target == a));
    assert!(snapshot.impacts.iter().any(|i| i.target == v));

    let scores = score(&snapshot, &params());
    for (entry, value) in &scores.values {
        assert!(value.is_finite(), "{entry:?} scored {value}");
    }
    assert!(scores.values[&a] != 0.0);
    assert!(scores.values[&v] != 0.0);
}

/// The exact real-data bug: `$-N` positional track ids are reused
/// across every album that has an N-th track (e.g. "$-1" alone
/// appears 28 times in one real franchise file's worth of albums).
/// Two different albums' `$-1` tracks must resolve to two distinct
/// entries, not collide into one merged measure.
#[test]
fn dollar_placeholder_ids_are_scoped_to_their_containing_entry() {
    let doc = parse_str(
        r#"<document>
            <entry id="M-ALBUM-A" title="Album A">
                <entry id="$-1" title="Track A1">
                    <music base="0.9" />
                </entry>
            </entry>
            <entry id="M-ALBUM-B" title="Album B">
                <entry id="$-1" title="Track B1">
                    <music base="0.1" />
                </entry>
            </entry>
        </document>"#,
        date!(2026 - 07 - 31),
    )
    .expect("parse");
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);

    let track_a = EntryId("M-ALBUM-A-1".into());
    let track_b = EntryId("M-ALBUM-B-1".into());
    assert!(
        doc.entries.iter().any(|e| e.id == track_a.0),
        "M-ALBUM-A's $-1 should resolve to M-ALBUM-A-1"
    );
    assert!(
        doc.entries.iter().any(|e| e.id == track_b.0),
        "M-ALBUM-B's $-1 should resolve to M-ALBUM-B-1"
    );

    let (snapshot, _extras, warnings) = build_snapshot(&doc);
    assert!(warnings.is_empty(), "{warnings:?}");
    // Two entries, two impacts, two distinct scores - not merged into
    // one shared "$-1" measure.
    assert_eq!(
        snapshot
            .entries
            .iter()
            .filter(|e| e.id == track_a || e.id == track_b)
            .count(),
        2
    );
    assert_eq!(snapshot.impacts.len(), 2);

    let scores = score(&snapshot, &params());
    assert!(scores.values[&track_a] > scores.values[&track_b]);
}

/// End-to-end: a comment immediately before an impact tag reaches
/// `BuildExtras.source_comments`, and an `AP`-coded impact (an
/// overloaded box under the legacy palette) is flagged for review,
/// while an `AU`-coded one (unambiguous) is not.
#[test]
fn build_snapshot_captures_comments_and_flags_ambiguous_anchors() {
    let doc = parse_str(
        r#"<document>
            <entry id="A-1" title="A">
                <!-- comedy was weak ngl -->
                <nei base="0.5" emotions="AP" />
                <!-- suicidal undertones -->
                <aei base="0.5" emotions="AU" />
            </entry>
        </document>"#,
        date!(2026 - 07 - 31),
    )
    .expect("parse");
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);

    let (snapshot, extras, warnings) = build_snapshot(&doc);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(snapshot.impacts.len(), 2);

    let ap_impact = snapshot
        .impacts
        .iter()
        .find(|i| extras.needs_review.contains(&i.id))
        .expect("the AP-coded impact should be flagged");
    assert_eq!(
        extras
            .source_comments
            .get(&ap_impact.id)
            .map(String::as_str),
        Some("comedy was weak ngl")
    );

    let au_impact = snapshot
        .impacts
        .iter()
        .find(|i| !extras.needs_review.contains(&i.id))
        .expect("the AU-coded impact should not be flagged");
    assert_eq!(
        extras
            .source_comments
            .get(&au_impact.id)
            .map(String::as_str),
        Some("suicidal undertones")
    );
}

/// The migration to the new palette (`ANCHOR_PALETTE_PROPOSAL.md`) is
/// meant to be a pure relabeling with `k_feat` still all-default -- the
/// same claim the plan calls "byte-identical, not just eyeballed".
/// Rather than depending on prior git history for a literal
/// old-palette snapshot, this tests the actual invariant directly:
/// renaming every `AnchorId` in a scored `Snapshot` back to its legacy
/// 2-letter box code (the reverse of `snapshot::anchor_for_code`/
/// `legacy.rs`'s new targets) must not change any score, since
/// `score()` only ever reads anchor identity for same-cell grouping
/// and `k_feat` lookups, both of which are relabeled consistently.
#[test]
fn anchor_relabeling_does_not_change_scores() {
    let doc = parse_str(DOC, date!(2026 - 07 - 31)).expect("parse");
    let (snapshot, _extras, warnings) = build_snapshot(&doc);
    assert!(warnings.is_empty(), "{warnings:?}");

    let reverse: std::collections::BTreeMap<AnchorId, AnchorId> = [
        ("comedy", "AP"),
        ("tragedy", "CU"),
        ("cozy", "CP"),
        ("heartwarming", "MP"),
        ("hate", "AU"),
        ("discomfort", "MU"),
        ("writing", "AL"),
        ("visual", "AV"),
        ("music", "AM"),
    ]
    .into_iter()
    .map(|(new, old)| (AnchorId(new.into()), AnchorId(old.into())))
    .collect();

    let relabeled = relabel_snapshot(&snapshot, &reverse);

    // Each snapshot scores with its own palette's `K`, relabeled along
    // with everything else.
    let with_k = |snap: &Snapshot| Params {
        k_feat: snap.palette.k_feat.clone(),
        time: Default::default(),
        calibration: Isotonic::default(),
        sign: Default::default(),
    };
    let original_scores = score(&snapshot, &with_k(&snapshot));
    let relabeled_scores = score(&relabeled, &with_k(&relabeled));
    // Not bit-exact: BTreeMaps are keyed by the anchor strings
    // themselves, so relabeling can shift floating-point summation
    // order enough to move the last ULP (nrs-core's own types.rs docs
    // note summation order, and so exact bit-pattern, depends on
    // BTreeMap iteration order) -- the actual claim under test is
    // "the same score", not "the same bit pattern".
    assert_eq!(original_scores.values.len(), relabeled_scores.values.len());
    for (entry, original) in &original_scores.values {
        let relabeled = relabeled_scores.values[entry];
        assert!(
            (original - relabeled).abs() < 1e-4,
            "{entry:?}: {original} vs {relabeled}"
        );
    }
}

fn relabel_snapshot(
    snap: &Snapshot,
    map: &std::collections::BTreeMap<AnchorId, AnchorId>,
) -> Snapshot {
    let relabel = |id: &AnchorId| map.get(id).cloned().unwrap_or_else(|| id.clone());

    let impacts = snap
        .impacts
        .iter()
        .map(|impact| Impact {
            id: impact.id.clone(),
            target: impact.target.clone(),
            atoms: impact
                .atoms
                .iter()
                .map(|atom| Atom {
                    source: atom.source.clone(),
                    blend: atom.blend.iter().map(|(a, w)| (relabel(a), *w)).collect(),
                    magnitude: atom.magnitude,
                    time: atom.time,
                })
                .collect(),
        })
        .collect();

    let relations = snap
        .relations
        .iter()
        .map(|rel| Relation {
            kind: rel.kind,
            from: rel.from.clone(),
            to: rel.to.clone(),
            transform: match &rel.transform {
                Transform::Scalar(w) => Transform::Scalar(*w),
                Transform::Regional(m) => {
                    Transform::Regional(m.iter().map(|(a, w)| (relabel(a), *w)).collect())
                }
                Transform::Transport(m) => Transform::Transport(
                    m.iter()
                        .map(|((a, b), w)| ((relabel(a), relabel(b)), *w))
                        .collect(),
                ),
            },
        })
        .collect();

    let anchors = snap
        .palette
        .anchors
        .iter()
        .map(|a| Anchor {
            id: relabel(&a.id),
            label: a.label.clone(),
            family: a.family.clone(),
        })
        .collect();
    let pairs = snap
        .palette
        .k_feat
        .pairs
        .iter()
        .map(|((a, b), k)| ((relabel(a), relabel(b)), *k))
        .collect();

    Snapshot {
        entries: snap.entries.clone(),
        impacts,
        relations,
        palette: Palette {
            anchors,
            k_feat: KFeatMatrix { pairs },
        },
    }
}

#[test]
fn shared_impacts_and_relation_contributors_reach_the_snapshot() {
    let doc = parse_str(
        r#"<document>
            <entry id="A-1" />
            <entry id="B-1" />
            <entry id="K-1" />
            <aei base="0.5" emotions="CP">
                <contributor id="B-1" factor="0.25" />
                <contributor id="A-1" factor="0.75" />
            </aei>
            <remix id="K-1">
                <contributor id="A-1" factor="0.5" />
                <contributor id="B-1" factor="0.25" />
            </remix>
        </document>"#,
        date!(2026 - 07 - 31),
    )
    .expect("parse");
    let (snapshot, _extras, warnings) = build_snapshot(&doc);
    assert!(warnings.is_empty(), "{warnings:?}");

    // Both shares come from one source: the largest contributor.
    assert_eq!(snapshot.impacts.len(), 2);
    for impact in &snapshot.impacts {
        for atom in &impact.atoms {
            assert_eq!(atom.source, SourceId("A-1".into()));
        }
    }

    let remix_weight = |from: &str| {
        snapshot
            .relations
            .iter()
            .find(|r| r.kind == RelKind::Remix && r.from == EntryId(from.into()))
            .map(|r| r.transform.clone())
    };
    assert_eq!(remix_weight("A-1"), Some(Transform::Scalar(0.2 * 0.5)));
    assert_eq!(remix_weight("B-1"), Some(Transform::Scalar(0.2 * 0.25)));
}

#[test]
fn validator_suppressions_become_entry_waivers() {
    let doc = parse_str(
        r#"<document>
            <entry id="A-MAL-1" title="One">
                <validatorSuppress rules=";dah-no-anime-altsrc;dah-visualless-entry" />
                <pads length="1" emotions="MP">
                    <validatorSuppress rules="dah-lone-pads" />
                    <contributor id="A-MAL-1" factor="0.5" />
                    <contributor id="A-MAL-2" factor="0.5" />
                </pads>
            </entry>
            <entry id="A-MAL-2" title="Two" />
        </document>"#,
        date!(2026 - 07 - 31),
    )
    .expect("parse");
    let (snapshot, extras, _) = build_snapshot(&doc);
    let rules = |id: &str| -> Vec<&str> {
        extras.waivers[&EntryId(id.into())]
            .iter()
            .map(String::as_str)
            .collect()
    };
    assert_eq!(
        rules("A-MAL-1"),
        ["dah-lone-pads", "dah-no-anime-altsrc", "dah-visualless-entry"]
    );
    assert_eq!(rules("A-MAL-2"), ["dah-lone-pads"]);

    let mut store = Store::open_in_memory().unwrap();
    write_snapshot(&mut store, &snapshot, &extras).unwrap();
    assert_eq!(store.waivers().unwrap().len(), 4);
}
