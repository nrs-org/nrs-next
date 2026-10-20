//! Brings an existing `nrs.db` up to date with what the importer now
//! produces for the same XML, keeping everything done to the database
//! since (see `nrs_store::Store::plan_resync`). Dry run by default.
//!
//! Usage: `cargo run -p nrs-import --features scripting --bin resync --
//! <path to .xml> <path to .db> [--today YYYY-MM-DD] [--apply] [--verbose]
//! [--music-store URL] [--anime-store URL]`
//!
//! Stripped XML (`<bind url>` entries) gets its titles and store-asserted
//! containment from the stores; `--music-store`/`--anime-store` default to
//! `NRS_MUSIC_STORE_URL`/`NRS_ANIME_STORE_URL`.
//!
//! `--today` defaults to the database's last `snapshot.import` date, so
//! date-dependent impacts (memes, waifus, PADS running "to today") are
//! computed the same way the stored ones were and don't show up as
//! spurious changes. `--apply` first copies the database and its review
//! sidecar next to it as `*.pre-resync-<unix time>`.
//!
//! `--waivers-only` narrows the plan to the `<validatorSuppress>` waivers
//! the store lacks, for a database whose content has moved on from the
//! XML.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::exit;

use nrs_core::{EntryId, Impact, Relation, Snapshot};
use nrs_store::{ResyncPlan, Store};

const NOTE: &str = "resync: importer fixes (document-level impacts, \
maxAEIPADS/cryPADS pairs, relation contributors, nrsml script layer)";

struct Args {
    xml: PathBuf,
    db: PathBuf,
    today: Option<time::Date>,
    apply: bool,
    verbose: bool,
    waivers_only: bool,
    music_store: Option<String>,
    anime_store: Option<String>,
}

fn parse_args() -> Args {
    let usage =
        "usage: resync <path to .xml> <path to .db> [--today YYYY-MM-DD] [--apply] [--verbose] \
         [--waivers-only] [--music-store URL] [--anime-store URL]";
    let mut positional = Vec::new();
    let (mut today, mut apply, mut verbose, mut waivers_only) = (None, false, false, false);
    let mut music_store = std::env::var("NRS_MUSIC_STORE_URL").ok();
    let mut anime_store = std::env::var("NRS_ANIME_STORE_URL").ok();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--apply" => apply = true,
            "--verbose" => verbose = true,
            "--waivers-only" => waivers_only = true,
            "--music-store" => {
                music_store = Some(args.next().unwrap_or_else(|| {
                    eprintln!("{usage}");
                    exit(1)
                }))
            }
            "--anime-store" => {
                anime_store = Some(args.next().unwrap_or_else(|| {
                    eprintln!("{usage}");
                    exit(1)
                }))
            }
            "--today" => {
                let value = args.next().unwrap_or_else(|| {
                    eprintln!("{usage}");
                    exit(1)
                });
                let format = time::format_description::well_known::Iso8601::DATE;
                today = Some(time::Date::parse(&value, &format).unwrap_or_else(|e| {
                    eprintln!("invalid --today '{value}': {e}");
                    exit(1)
                }));
            }
            _ => positional.push(arg),
        }
    }
    let [xml, db] = <[String; 2]>::try_from(positional).unwrap_or_else(|_| {
        eprintln!("{usage}");
        exit(1)
    });
    Args {
        xml: xml.into(),
        db: db.into(),
        today,
        apply,
        verbose,
        waivers_only,
        music_store,
        anime_store,
    }
}

fn fail(what: &str, e: impl std::fmt::Display) -> ! {
    eprintln!("{what}: {e}");
    exit(1)
}

fn main() {
    let args = parse_args();
    if !args.db.exists() {
        fail("no database at", args.db.display());
    }
    let mut store = Store::open(&args.db).unwrap_or_else(|e| fail("failed to open database", e));

    let today = match args.today {
        Some(today) => today,
        None => store
            .last_import_date()
            .unwrap_or_else(|e| fail("failed to read event log", e))
            .unwrap_or_else(|| fail("no snapshot.import event; pass --today", "")),
    };
    eprintln!("parsing {} as of {today}", args.xml.display());
    let mut doc =
        nrs_import::parse_file(&args.xml, today).unwrap_or_else(|e| fail("parse error", e));
    let sources = nrs_import::StoreSources::from_env(&args.music_store, &args.anime_store);
    let store_warnings =
        nrs_import::resolve_binds(&mut doc, &sources).unwrap_or_else(|e| fail("store lookup", e));
    doc.warnings.extend(store_warnings);
    let (mut snapshot, extras, conversion_warnings) = nrs_import::build_snapshot(&doc);
    let warnings = doc.warnings.len() + conversion_warnings.len();
    if args.verbose {
        for w in doc.warnings.iter().chain(&conversion_warnings) {
            eprintln!("warning: {w}");
        }
    }
    eprintln!(
        "parsed {} entries, {} impacts, {} relations ({warnings} warnings{})",
        snapshot.entries.len(),
        snapshot.impacts.len(),
        snapshot.relations.len(),
        if args.verbose {
            ""
        } else {
            "; --verbose lists them"
        }
    );

    let mut extras = nrs_import::import_extras(&extras);
    // Entries merged in the app since are read as their survivors, not
    // added back.
    store
        .follow_merges(&mut snapshot, &mut extras)
        .unwrap_or_else(|e| fail("failed to read merges", e));
    let mut plan = store
        .plan_resync(&snapshot, &extras)
        .unwrap_or_else(|e| fail("failed to plan resync", e));
    if args.waivers_only {
        plan = ResyncPlan {
            waivers_to_add: plan.waivers_to_add,
            ..ResyncPlan::default()
        };
    }
    report(&plan, &snapshot, &extras, args.verbose);

    if plan.is_empty() {
        println!("\nnothing to do");
        return;
    }
    if !args.apply {
        println!("\ndry run; pass --apply to write this (stop nrs-app first)");
        return;
    }

    let stamp = time::OffsetDateTime::now_utc().unix_timestamp();
    for path in [args.db.clone(), args.db.with_extension("review.json")] {
        if path.exists() {
            let backup = backup_path(&path, stamp);
            std::fs::copy(&path, &backup).unwrap_or_else(|e| fail("backup failed", e));
            println!("backed up {} -> {}", path.display(), backup.display());
        }
    }
    store
        .apply_resync(&plan, &extras, NOTE)
        .unwrap_or_else(|e| fail("resync failed (database unchanged)", e));
    println!("applied");
}

fn backup_path(path: &Path, stamp: i64) -> PathBuf {
    let mut name = path.file_name().unwrap().to_os_string();
    name.push(format!(".pre-resync-{stamp}"));
    path.with_file_name(name)
}

fn report(plan: &ResyncPlan, snapshot: &Snapshot, extras: &nrs_store::ImportExtras, verbose: bool) {
    let titles: BTreeMap<&str, &str> = snapshot
        .entries
        .iter()
        .filter_map(|e| Some((e.id.0.as_str(), e.title_cache.as_deref()?)))
        .collect();
    let name = |id: &EntryId| match titles.get(id.0.as_str()) {
        Some(title) => format!("{} ({title})", id.0),
        None => id.0.clone(),
    };
    let mass = |impact: &Impact| impact.atoms.iter().map(|a| a.magnitude).sum::<f32>();
    let kind = |impact: &Impact| {
        extras
            .impact_dah
            .get(&impact.id)
            .map_or("?", |(kind, _, _)| kind.as_str())
            .to_string()
    };
    let relation = |r: &Relation| format!("{:?} {} -> {}", r.kind, name(&r.from), name(&r.to));

    println!("entries to add: {}", plan.entries_to_add.len());
    for entry in &plan.entries_to_add {
        println!("  + {}", name(&entry.id));
    }
    println!("entries to retitle: {}", plan.entries_to_retitle.len());
    for retitle in plan.entries_to_retitle.iter().take(20) {
        println!(
            "  ~ {}: {:?} -> {:?}",
            retitle.id.0,
            retitle.before.as_deref().unwrap_or(""),
            retitle.after
        );
    }
    if plan.entries_to_retitle.len() > 20 {
        println!("  ... and {} more", plan.entries_to_retitle.len() - 20);
    }
    println!("entries to rebind: {}", plan.entries_to_rebind.len());
    for rebind in plan.entries_to_rebind.iter().take(5) {
        println!(
            "  ~ {}: {}:{} -> {}:{}",
            rebind.id.0, rebind.before.0, rebind.before.1, rebind.after.0, rebind.after.1
        );
    }
    if plan.entries_to_rebind.len() > 5 {
        println!("  ... and {} more", plan.entries_to_rebind.len() - 5);
    }

    let mut by_kind: BTreeMap<String, (usize, f32)> = BTreeMap::new();
    let mut by_target: BTreeMap<&EntryId, f32> = BTreeMap::new();
    for impact in &plan.impacts_to_add {
        let slot = by_kind.entry(kind(impact)).or_default();
        slot.0 += 1;
        slot.1 += mass(impact);
        *by_target.entry(&impact.target).or_default() += mass(impact);
    }
    for impact in &plan.impacts_to_remove {
        *by_target.entry(&impact.target).or_default() -= mass(impact);
    }
    println!("impacts to add: {}", plan.impacts_to_add.len());
    for (kind, (count, total)) in &by_kind {
        println!("  {kind:<24} {count:>4}  total mass {total:+.2}");
    }
    println!("impacts to remove: {}", plan.impacts_to_remove.len());
    for impact in &plan.impacts_to_remove {
        println!(
            "  - {} on {}  mass {:.3}",
            impact.id.0,
            name(&impact.target),
            mass(impact)
        );
    }
    println!(
        "impacts kept (edited/confirmed/added by hand, no counterpart in the import): {}",
        plan.impacts_kept.len()
    );
    for impact in &plan.impacts_kept {
        println!("  = {} on {}", impact.id.0, name(&impact.target));
    }
    let (mut created, mut folded, mut extended) = (0, 0, 0);
    let mut folded_rows = 0;
    for change in &plan.shared_changes {
        match change.base {
            nrs_store::SharedBase::New => created += 1,
            nrs_store::SharedBase::Fold(_) => folded += 1,
            nrs_store::SharedBase::Existing(_) => extended += 1,
        }
        folded_rows += change.fold.len();
    }
    println!(
        "shared impacts: {created} new, {folded} folded from {folded_rows} per-contributor rows, \
         {extended} gaining members"
    );
    if verbose {
        for change in &plan.shared_changes {
            let members = change
                .add
                .iter()
                .map(|i| format!("{} {:.3}", i.target.0, mass(i)));
            let folded = change
                .fold
                .iter()
                .map(|(_, e, share)| format!("{} {share:.2}", e.0));
            let kind = change.add.first().map_or("?".to_string(), kind);
            println!(
                "  * {} [{kind}] {:?}: {}",
                change.key,
                change.base,
                members.chain(folded).collect::<Vec<_>>().join(", ")
            );
        }
    }

    println!("relations to add: {}", plan.relations_to_add.len());
    if verbose {
        for r in &plan.relations_to_add {
            println!("  + {}", relation(r));
        }
    }
    println!("relations to remove: {}", plan.relations_to_remove.len());
    for (id, r) in &plan.relations_to_remove {
        println!("  - {id} {}", relation(r));
    }
    println!("relations kept: {}", plan.relations_kept.len());
    for (id, r) in &plan.relations_kept {
        println!("  = {id} {}", relation(r));
    }

    println!("waivers to add: {}", plan.waivers_to_add.len());
    if verbose {
        for (entry, rule) in &plan.waivers_to_add {
            println!("  + {} {rule}", name(entry));
        }
    }

    let mut deltas: Vec<_> = by_target.into_iter().collect();
    deltas.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
    let shown = if verbose { deltas.len() } else { 15 };
    println!("largest own-mass changes by entry:");
    for (target, delta) in deltas.iter().take(shown) {
        println!("  {delta:+8.3}  {}", name(target));
    }
    if deltas.len() > shown {
        println!("  ... {} more (--verbose)", deltas.len() - shown);
    }
}
