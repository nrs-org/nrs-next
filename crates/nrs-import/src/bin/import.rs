//! One-shot `nrsml` XML -> `nrs.db` import (INFRA.md §12 roadmap phase 3).
//! `nrs-app` can already browse an XML file directly in read-only mode,
//! but writing needs a real `.db` -- this is that write, run once
//! against a fresh/empty database.
//!
//! Usage: `cargo run -p nrs-import --bin import -- <path to .xml> <path to .db> [today]`
//!
//! Stripped XML (`<bind url>` entries) gets its titles and store-asserted
//! containment from `NRS_MUSIC_STORE_URL` (musiclib's server) and
//! `NRS_ANIME_STORE_URL` (the anime store).

use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(xml_path), Some(db_path)) = (args.next(), args.next()) else {
        eprintln!("usage: import <path to .xml> <path to .db> [today]");
        std::process::exit(1);
    };

    let today = match args.next() {
        Some(value) => {
            match time::Date::parse(&value, &time::format_description::well_known::Iso8601::DATE) {
                Ok(date) => date,
                Err(error) => {
                    eprintln!("invalid today date '{value}': {error}");
                    std::process::exit(1);
                }
            }
        }
        None => time::OffsetDateTime::now_utc().date(),
    };
    let mut doc = match nrs_import::parse_file(&PathBuf::from(&xml_path), today) {
        Ok(doc) => doc,
        Err(e) => {
            eprintln!("parse error: {e}");
            std::process::exit(1);
        }
    };
    let music_store = std::env::var("NRS_MUSIC_STORE_URL").ok();
    let anime_store = std::env::var("NRS_ANIME_STORE_URL").ok();
    let sources = nrs_import::StoreSources::from_env(&music_store, &anime_store);
    match nrs_import::resolve_binds(&mut doc, &sources) {
        Ok(warnings) => doc.warnings.extend(warnings),
        Err(e) => {
            eprintln!("store lookup: {e}");
            std::process::exit(1);
        }
    }
    for w in &doc.warnings {
        eprintln!("warning: {w}");
    }

    let (snapshot, extras, conversion_warnings) = nrs_import::build_snapshot(&doc);
    for w in &conversion_warnings {
        eprintln!("warning: {w}");
    }
    eprintln!(
        "parsed {} entries, {} impacts, {} relations",
        snapshot.entries.len(),
        snapshot.impacts.len(),
        snapshot.relations.len()
    );

    let mut store = match nrs_store::Store::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to open {db_path}: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = nrs_import::write_snapshot(&mut store, &snapshot, &extras) {
        eprintln!("failed to write snapshot: {e}");
        std::process::exit(1);
    }
    eprintln!("wrote {} entries to {db_path}", snapshot.entries.len());
}
