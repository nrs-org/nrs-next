//! Gives every entry that was already watched/read a progress row, from
//! its consumed and dropped impacts (see
//! `nrs_store::Store::plan_progress_backfill`). Entries that already have
//! one are left alone, so it can be re-run. Dry run by default.
//!
//! Usage: `cargo run -p nrs-store --bin progress_backfill -- <path to .db>
//! [--apply] [--verbose]`
//!
//! Opening the database migrates it (an empty `progress` table) even on a
//! dry run. `--apply` first copies the database and its review sidecar next
//! to it as `*.pre-progress-backfill-<unix time>`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::exit;

use nrs_store::{ProgressStatus, Store};

const NOTE: &str = "progress backfill: from consumed/dropped impacts";

fn fail(what: &str, e: impl std::fmt::Display) -> ! {
    eprintln!("{what}: {e}");
    exit(1)
}

fn backup_path(path: &Path, stamp: i64) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".pre-progress-backfill-{stamp}"));
    PathBuf::from(name)
}

fn main() {
    let usage = "usage: progress_backfill <path to .db> [--apply] [--verbose]";
    let (mut db, mut apply, mut verbose) = (None, false, false);
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--apply" => apply = true,
            "--verbose" => verbose = true,
            _ if db.is_none() && !arg.starts_with("--") => db = Some(PathBuf::from(arg)),
            _ => fail(usage, arg),
        }
    }
    let db = db.unwrap_or_else(|| fail(usage, "missing database path"));
    if !db.exists() {
        fail("no database at", db.display());
    }
    let mut store = Store::open(&db).unwrap_or_else(|e| fail("failed to open database", e));
    let plan = store
        .plan_progress_backfill()
        .unwrap_or_else(|e| fail("failed to plan", e));

    let titles: HashMap<String, String> = store
        .list_entries()
        .unwrap_or_else(|e| fail("failed to load entries", e))
        .into_iter()
        .map(|entry| (entry.id.0, entry.title_cache.unwrap_or(entry.store_id)))
        .collect();
    let completed = plan
        .iter()
        .filter(|progress| progress.status == ProgressStatus::Completed)
        .count();
    println!(
        "{} progress rows: {completed} completed, {} dropped",
        plan.len(),
        plan.len() - completed
    );
    if verbose {
        for progress in &plan {
            println!(
                "  {:<9} {:>6} {}  {}",
                progress.status.as_str(),
                progress.current,
                progress.unit.as_str(),
                titles.get(&progress.entry.0).map_or("?", String::as_str)
            );
        }
    }
    if !apply {
        println!("dry run; --apply writes them");
        return;
    }
    if plan.is_empty() {
        return;
    }

    let stamp = time::OffsetDateTime::now_utc().unix_timestamp();
    let review = db.with_extension("review.json");
    for path in [&db, &review] {
        if path.exists() {
            let backup = backup_path(path, stamp);
            std::fs::copy(path, &backup).unwrap_or_else(|e| fail("backup failed", e));
            println!("backed up {} -> {}", path.display(), backup.display());
        }
    }
    for progress in &plan {
        store
            .set_progress(progress, Some(NOTE))
            .unwrap_or_else(|e| fail("failed to write progress", e));
    }
    println!("wrote {} progress rows", plan.len());
}
