//! End-to-end: parse an nrsml-shaped document, build a `Snapshot`, and
//! confirm `nrs-core` can actually score it without panicking - the
//! real point of having a parser at all.

use nrs_core::*;
use nrs_import::{build_snapshot, xml::parse_str};
use time::macros::date;

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

    let (snapshot, conversion_warnings) = build_snapshot(&doc);
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
/// track via an auto-generated Contains relation, and the whole thing
/// must still score finitely.
#[test]
fn role_credit_generates_a_weighted_contains_relation() {
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

    let (snapshot, warnings) = build_snapshot(&doc);
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
        .find(|r| r.kind == RelKind::Contains && r.from == artist && r.to == track)
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

    let (snapshot, warnings) = build_snapshot(&doc);
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

    let (snapshot, warnings) = build_snapshot(&doc);
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
