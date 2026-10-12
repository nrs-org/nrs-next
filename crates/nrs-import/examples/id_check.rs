fn main() {
    let path = std::path::Path::new("/home/ayaneso/dev/nrs-org/nrs-impl/nrs/NRSImplMain.xml");
    let today = time::OffsetDateTime::now_utc().date();
    let doc = nrs_import::xml::parse_file(path, today).expect("parse");
    let (snap, _extras, _w) = nrs_import::build_snapshot(&doc);

    let params = nrs_core::Params {
        k_feat: snap.palette.k_feat.clone(),
        calibration: nrs_core::Isotonic::default(),
    };
    let scores = nrs_core::score(&snap, &params);

    let mut ranked: Vec<(&nrs_core::EntryId, f32)> =
        scores.values.iter().map(|(k, &v)| (k, v)).collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    println!("--- top 10 ---");
    for (id, v) in ranked.iter().take(10) {
        let title = snap
            .entries
            .iter()
            .find(|e| &e.id == *id)
            .and_then(|e| e.title_cache.clone());
        println!("{v:>10.3}  {id:?}  {title:?}");
    }

    for target in ["F-VGMDB-4499", "F-VGMDB-4192"] {
        let id = nrs_core::EntryId(target.into());
        let ex = nrs_core::explain(&snap, &params, id.clone());
        let mut by_anchor: std::collections::BTreeMap<String, (f64, usize)> =
            std::collections::BTreeMap::new();
        for c in &ex.contributions {
            let e = by_anchor.entry(c.anchor.0.clone()).or_insert((0.0, 0));
            e.0 += c.mass.abs() as f64;
            e.1 += 1;
        }
        println!(
            "\n{target}: score={:.3} value={:.3} cells={}",
            ex.score,
            ex.value,
            ex.contributions.len()
        );
        for (a, (mass, n)) in &by_anchor {
            println!("  anchor {a:<10} n_cells={n:<4} total_|mass|={mass:.2}");
        }
    }
}
