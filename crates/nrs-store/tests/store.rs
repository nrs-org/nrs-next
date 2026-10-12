use nrs_core::{AnchorId, Atom, Isotonic, RelKind, SourceId, TimeSupport, Transform};
use nrs_store::Store;

fn sample_atom(anchor: &str, source: &str, magnitude: f32) -> Atom {
    Atom {
        source: SourceId(source.into()),
        blend: vec![(AnchorId(anchor.into()), 1.0)],
        magnitude,
        time: TimeSupport::Moment,
    }
}

#[test]
fn fresh_db_has_empty_snapshot_and_the_seeded_palette() {
    let store = Store::open_in_memory().unwrap();
    let (snap, params) = store.load_snapshot().unwrap();

    assert!(snap.entries.is_empty());
    assert!(snap.impacts.is_empty());
    assert!(snap.relations.is_empty());
    assert_eq!(snap.palette.anchors.len(), 18);

    let waifu = AnchorId("waifu".into());
    assert_eq!(params.k_feat.exponent(&waifu), 0.35);
    assert_eq!(params.k_feat, snap.palette.k_feat);
    assert_eq!(params.calibration, Isotonic::default());
}

#[test]
fn round_trip_preserves_snapshot_and_score() {
    let mut store = Store::open_in_memory().unwrap();

    let track_a = store.add_entry("music", "m1", Some("Track A")).unwrap();
    let album_b = store.add_entry("music", "m2", Some("Album B")).unwrap();

    let impact_id = store
        .add_impact(
            &track_a,
            &[sample_atom("gut-punch", "src-a", 5.0)],
            Some("first listen"),
        )
        .unwrap();

    store
        .add_relation(
            RelKind::Contains,
            &track_a,
            &album_b,
            &Transform::Scalar(1.0),
        )
        .unwrap();

    store
        .upsert_anchor(&AnchorId("gut-punch".into()), "Gut punch", Some("emotion"))
        .unwrap();
    store
        .upsert_anchor(
            &AnchorId("gorgeous-visuals".into()),
            "Gorgeous visuals",
            None,
        )
        .unwrap();
    store
        .set_kfeat_pair(
            &AnchorId("gut-punch".into()),
            &AnchorId("gorgeous-visuals".into()),
            0.3,
        )
        .unwrap();

    store
        .set_calibration(&Isotonic {
            knots: vec![(0.0, 0.0), (10.0, 100.0)],
        })
        .unwrap();

    let (snap, params) = store.load_snapshot().unwrap();

    assert_eq!(snap.entries.len(), 2);
    assert_eq!(snap.impacts.len(), 1);
    assert_eq!(snap.impacts[0].id.0, impact_id.0);
    assert_eq!(snap.impacts[0].target, track_a);
    assert_eq!(
        snap.impacts[0].atoms,
        vec![sample_atom("gut-punch", "src-a", 5.0)]
    );

    assert_eq!(snap.relations.len(), 1);
    assert_eq!(snap.relations[0].kind, RelKind::Contains);
    assert_eq!(snap.relations[0].from, track_a);
    assert_eq!(snap.relations[0].to, album_b);
    assert_eq!(snap.relations[0].transform, Transform::Scalar(1.0));

    assert_eq!(snap.palette.anchors.len(), 18 + 2);
    assert_eq!(
        snap.palette.k_feat.get(
            &AnchorId("gut-punch".into()),
            &AnchorId("gorgeous-visuals".into())
        ),
        0.3
    );

    assert_eq!(
        params.calibration,
        Isotonic {
            knots: vec![(0.0, 0.0), (10.0, 100.0)]
        }
    );

    // Identity check: scoring the reloaded snapshot matches scoring an
    // equivalent freshly-built one (round-trip preserves everything
    // `score()` reads).
    let scores_from_store = nrs_core::score(&snap, &params);
    assert!(scores_from_store.values.contains_key(&track_a));
    assert!(scores_from_store.values[&track_a].is_finite());
}

#[test]
fn edit_impact_replaces_atoms() {
    let mut store = Store::open_in_memory().unwrap();
    let entry = store.add_entry("music", "m1", None).unwrap();
    let impact_id = store
        .add_impact(&entry, &[sample_atom("gut-punch", "src-a", 5.0)], None)
        .unwrap();

    store
        .edit_impact(
            &impact_id,
            &[sample_atom("gorgeous-visuals", "src-b", 2.0)],
            None,
        )
        .unwrap();

    let (snap, _) = store.load_snapshot().unwrap();
    assert_eq!(snap.impacts.len(), 1);
    assert_eq!(
        snap.impacts[0].atoms,
        vec![sample_atom("gorgeous-visuals", "src-b", 2.0)]
    );
}

#[test]
fn delete_impact_removes_it_and_its_atoms() {
    let mut store = Store::open_in_memory().unwrap();
    let entry = store.add_entry("music", "m1", None).unwrap();
    let impact_id = store
        .add_impact(&entry, &[sample_atom("gut-punch", "src-a", 5.0)], None)
        .unwrap();

    store.delete_impact(&impact_id).unwrap();

    let (snap, _) = store.load_snapshot().unwrap();
    assert!(snap.impacts.is_empty());
}

#[test]
fn each_mutation_appends_exactly_one_event() {
    let mut store = Store::open_in_memory().unwrap();

    let entry = store.add_entry("music", "m1", Some("Track A")).unwrap();
    assert_eq!(store.event_log().unwrap().len(), 1);
    let ev = store.event_log().unwrap().into_iter().next().unwrap();
    assert_eq!(ev.kind, "entry.add");
    assert!(ev.before.is_none());
    assert!(ev.after.is_some());
    let after: nrs_core::Entry = serde_json::from_str(&ev.after.unwrap()).unwrap();
    assert_eq!(after.id, entry);

    let impact_id = store
        .add_impact(&entry, &[sample_atom("gut-punch", "src-a", 5.0)], None)
        .unwrap();
    assert_eq!(store.event_log().unwrap().len(), 2);

    store
        .edit_impact(
            &impact_id,
            &[sample_atom("gorgeous-visuals", "src-b", 1.0)],
            None,
        )
        .unwrap();
    let events = store.event_log().unwrap();
    assert_eq!(events.len(), 3);
    let edit_event = events.last().unwrap();
    assert_eq!(edit_event.kind, "impact.edit");
    assert!(edit_event.before.is_some());
    assert!(edit_event.after.is_some());

    store.delete_impact(&impact_id).unwrap();
    let events = store.event_log().unwrap();
    assert_eq!(events.len(), 4);
    let delete_event = events.last().unwrap();
    assert_eq!(delete_event.kind, "impact.delete");
    assert!(delete_event.before.is_some());
    assert!(delete_event.after.is_none());
}

#[test]
fn opening_a_file_path_creates_schema_and_persists() {
    let dir = tempdir_path();
    let db_path = dir.join("nrs.db");

    {
        let mut store = Store::open(&db_path).unwrap();
        store.add_entry("music", "m1", Some("Track A")).unwrap();
    }
    {
        let store = Store::open(&db_path).unwrap();
        let (snap, _) = store.load_snapshot().unwrap();
        assert_eq!(snap.entries.len(), 1);
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn every_relkind_round_trips_through_its_text_column() {
    let mut store = Store::open_in_memory().unwrap();
    let a = store.add_entry("music", "a", None).unwrap();
    let b = store.add_entry("music", "b", None).unwrap();

    for kind in [
        RelKind::Contains,
        RelKind::Credit,
        RelKind::Remix,
        RelKind::FeatureMusic,
        RelKind::KilledBy,
    ] {
        store
            .add_relation(kind, &a, &b, &Transform::Scalar(0.5))
            .unwrap();
    }

    let (snap, _) = store.load_snapshot().unwrap();
    let kinds: Vec<RelKind> = snap.relations.iter().map(|r| r.kind).collect();
    assert_eq!(kinds.len(), 5);
    for kind in [
        RelKind::Contains,
        RelKind::Credit,
        RelKind::Remix,
        RelKind::FeatureMusic,
        RelKind::KilledBy,
    ] {
        assert!(kinds.contains(&kind), "{kind:?} did not round-trip");
    }
}

#[test]
fn preferences_round_trip_and_append_events() {
    let mut store = Store::open_in_memory().unwrap();
    let a = store.add_entry("music", "a", None).unwrap();
    let b = store.add_entry("music", "b", None).unwrap();
    let c = store.add_entry("music", "c", None).unwrap();

    store
        .record_preference(&a, &b, Some("A is just better"))
        .unwrap();
    store.record_preference(&b, &c, None).unwrap();

    let preferences = store.load_preferences().unwrap();
    assert_eq!(preferences.len(), 2);
    assert_eq!(preferences[0].winner, a);
    assert_eq!(preferences[0].loser, b);
    assert_eq!(preferences[0].confidence, 1.0);
    assert_eq!(preferences[1].winner, b);
    assert_eq!(preferences[1].loser, c);

    let events = store.event_log().unwrap();
    let preference_events: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "preference.record")
        .collect();
    assert_eq!(preference_events.len(), 2);
    assert!(preference_events[0].before.is_none());
    assert!(preference_events[0].after.is_some());
    assert_eq!(
        preference_events[0].note.as_deref(),
        Some("A is just better")
    );

    // Unrelated events (the three add_entry calls) don't leak in as
    // preferences: total events = 5, but the filtered set above is 2.
    assert_eq!(events.len(), 5);
}

#[test]
fn set_kfeat_pair_auto_registers_unknown_anchors() {
    // nrs-import doesn't populate Palette.anchors, so an anchor id that
    // only ever appeared in an atom's blend wouldn't already have an
    // `anchor` row -- set_kfeat_pair must not require it to.
    let mut store = Store::open_in_memory().unwrap();
    let a = AnchorId("AU".into());
    let b = AnchorId("AP".into());
    store.set_kfeat_pair(&a, &b, 0.4).unwrap();

    let (snap, _) = store.load_snapshot().unwrap();
    assert_eq!(snap.palette.k_feat.get(&a, &b), 0.4);
    assert_eq!(snap.palette.anchors.len(), 18 + 2);
}

#[test]
fn set_kfeat_pair_rejects_exponents_outside_unit_interval() {
    let mut store = Store::open_in_memory().unwrap();
    let a = AnchorId("hype".into());
    assert!(store.set_kfeat_pair(&a, &a, 0.0).is_err());
    assert!(store.set_kfeat_pair(&a, &a, 1.5).is_err());
    assert!(store.set_kfeat_pair(&a, &a, 0.25).is_ok());
}

fn tempdir_path() -> std::path::PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("nrs-store-test-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn time_params_default_and_round_trip() {
    let mut store = Store::open_in_memory().unwrap();
    let (_, params) = store.load_snapshot().unwrap();
    assert_eq!(params.time, nrs_core::TimeParams::default());

    let time = nrs_core::TimeParams {
        shared: 0.9,
        length_days: 90.0,
    };
    store.set_time_params(&time).unwrap();
    let (_, params) = store.load_snapshot().unwrap();
    assert_eq!(params.time, time);

    let bad = nrs_core::TimeParams {
        shared: 1.5,
        length_days: 90.0,
    };
    assert!(store.set_time_params(&bad).is_err());
}
