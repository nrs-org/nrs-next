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
