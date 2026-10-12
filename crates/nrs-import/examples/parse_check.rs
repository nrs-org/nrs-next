//! Ad hoc check against a real `nrsml` document tree: `cargo run -p
//! nrs-import --example parse_check -- /path/to/NRSImplMain.xml`.
//! Reports entry/impact/relation counts and every warning, and tries
//! `nrs_core::score` over the result so a real crash surfaces here
//! rather than only inside a proper importer later.

use std::path::PathBuf;

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: parse_check <path to .xml>");
    let today = time::OffsetDateTime::now_utc().date();

    let doc = match nrs_import::xml::parse_file(&path, today) {
        Ok(doc) => doc,
        Err(e) => {
            eprintln!("parse error: {e}");
            std::process::exit(1);
        }
    };

    let impact_count: usize = doc.entries.iter().map(|e| e.impacts.len()).sum();
    let relation_count: usize = doc.entries.iter().map(|e| e.relations.len()).sum();
    let role_count: usize = doc.entries.iter().map(|e| e.roles.len()).sum();

    println!(
        "entries: {}  impacts: {}  relations: {}  role credits: {}  warnings: {}",
        doc.entries.len(),
        impact_count,
        relation_count,
        role_count,
        doc.warnings.len()
    );

    let (snapshot, _extras, mut conversion_warnings) = nrs_import::build_snapshot(&doc);
    println!(
        "snapshot: {} entries, {} impacts, {} relations",
        snapshot.entries.len(),
        snapshot.impacts.len(),
        snapshot.relations.len()
    );

    let mut all_warnings = doc.warnings;
    all_warnings.append(&mut conversion_warnings);
    println!("\n--- warnings ({}) ---", all_warnings.len());
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for w in &all_warnings {
        let key = w.split(':').nth(1).unwrap_or(w).trim();
        let bucket = key.split(' ').next().unwrap_or(key);
        *counts.entry(bucket).or_default() += 1;
    }
    for (bucket, count) in &counts {
        println!("{count:5}  {bucket}");
    }

    let params = nrs_core::Params {
        k_feat: snapshot.palette.k_feat.clone(),
        calibration: nrs_core::Isotonic::default(),
    };
    let scores = nrs_core::score(&snapshot, &params);
    let non_finite = scores.values.values().filter(|v| !v.is_finite()).count();
    println!(
        "\nscored {} entries; {} non-finite values",
        scores.values.len(),
        non_finite
    );
}
