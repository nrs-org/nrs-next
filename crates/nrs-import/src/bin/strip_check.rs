//! Checks that stripped XML scores the same as the legacy XML it came from.
//!
//! Builds a snapshot from each (the stripped one with its `<bind>`s
//! resolved against the stores) and diffs entries, impacts and relations.
//! Titles are reported separately: stripped entries take the store's title,
//! which is allowed to differ.
//!
//! Usage: `cargo run -p nrs-import --features scripting --bin strip_check --
//! <legacy .xml> <stripped .xml> [--today YYYY-MM-DD] [--music-store URL]
//! [--anime-store URL] [--show N] [--tolerance REL]`
//!
//! `--tolerance` pairs up leftover impacts that differ only in atom
//! magnitudes, by at most REL relatively (store-filled progress lengths are
//! a few seconds off the hand-written ones), and reports them apart.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::exit;

use nrs_core::Snapshot;

fn fail(what: &str, e: impl std::fmt::Display) -> ! {
    eprintln!("{what}: {e}");
    exit(1)
}

fn build(path: &Path, today: time::Date, sources: &nrs_import::StoreSources) -> (Snapshot, usize) {
    let mut doc = nrs_import::parse_file(path, today).unwrap_or_else(|e| fail("parse error", e));
    let store_warnings =
        nrs_import::resolve_binds(&mut doc, sources).unwrap_or_else(|e| fail("store lookup", e));
    for w in &store_warnings {
        eprintln!("store: {w}");
    }
    let (snapshot, _, warnings) = nrs_import::build_snapshot(&doc);
    (snapshot, doc.warnings.len() + warnings.len())
}

/// Multiset of comparable keys.
fn counts(keys: impl Iterator<Item = String>) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for key in keys {
        *out.entry(key).or_default() += 1;
    }
    out
}

fn diff(
    name: &str,
    a: &BTreeMap<String, usize>,
    b: &BTreeMap<String, usize>,
    show: usize,
) -> usize {
    let mut only_a = Vec::new();
    let mut only_b = Vec::new();
    for (key, &n) in a {
        let m = b.get(key).copied().unwrap_or(0);
        if n > m {
            only_a.push(format!("{key} (x{})", n - m));
        }
    }
    for (key, &n) in b {
        let m = a.get(key).copied().unwrap_or(0);
        if n > m {
            only_b.push(format!("{key} (x{})", n - m));
        }
    }
    println!(
        "{name}: {} legacy-only, {} stripped-only",
        only_a.len(),
        only_b.len()
    );
    for line in only_a.iter().take(show) {
        println!("  - {line}");
    }
    for line in only_b.iter().take(show) {
        println!("  + {line}");
    }
    only_a.len() + only_b.len()
}

/// The atoms with magnitudes zeroed, and the magnitudes.
fn shape(impact: &nrs_core::Impact) -> (String, Vec<f32>) {
    let mut atoms = impact.atoms.clone();
    let magnitudes = atoms.iter().map(|a| a.magnitude).collect();
    for atom in &mut atoms {
        atom.magnitude = 0.0;
    }
    (format!("{} {:?}", impact.target.0, atoms), magnitudes)
}

/// Drops exact matches, then pairs leftovers of the same shape whose
/// magnitudes agree within `tolerance`; returns what is still unpaired.
fn pair_close<'a>(
    a: &'a [nrs_core::Impact],
    b: &'a [nrs_core::Impact],
    tolerance: f32,
    show: usize,
) -> (Vec<&'a nrs_core::Impact>, Vec<&'a nrs_core::Impact>) {
    let key = |i: &nrs_core::Impact| format!("{} {:?}", i.target.0, i.atoms);
    let mut exact: BTreeMap<String, usize> = BTreeMap::new();
    for impact in b {
        *exact.entry(key(impact)).or_default() += 1;
    }
    let mut left_a = Vec::new();
    for impact in a {
        match exact.get_mut(&key(impact)) {
            Some(n) if *n > 0 => *n -= 1,
            _ => left_a.push(impact),
        }
    }
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for impact in a {
        *seen.entry(key(impact)).or_default() += 1;
    }
    let mut left_b: Vec<&nrs_core::Impact> = Vec::new();
    for impact in b {
        match seen.get_mut(&key(impact)) {
            Some(n) if *n > 0 => *n -= 1,
            _ => left_b.push(impact),
        }
    }

    let mut close = Vec::new();
    let mut still_a = Vec::new();
    for impact in left_a {
        let (shape_a, mags_a) = shape(impact);
        let found = left_b.iter().position(|other| {
            let (shape_b, mags_b) = shape(other);
            shape_a == shape_b
                && mags_a
                    .iter()
                    .zip(&mags_b)
                    .all(|(x, y)| (x - y).abs() <= tolerance * x.abs().max(y.abs()))
        });
        match found {
            Some(index) => {
                let other = left_b.swap_remove(index);
                let worst = mags_a
                    .iter()
                    .zip(other.atoms.iter().map(|atom| atom.magnitude))
                    .map(|(x, y)| (y - x) / x.abs().max(f32::EPSILON))
                    .fold(0.0_f32, |m, r| if r.abs() > m.abs() { r } else { m });
                close.push((impact.target.0.clone(), worst));
            }
            None => still_a.push(impact),
        }
    }
    close.sort_by(|x, y| y.1.abs().total_cmp(&x.1.abs()));
    println!("impacts within {:.2}%: {}", tolerance * 100.0, close.len());
    for (target, change) in close.iter().take(show) {
        println!("  ~ {target} {:+.3}%", change * 100.0);
    }
    (still_a, left_b)
}

fn main() {
    let mut positional = Vec::new();
    let mut today = time::macros::date!(2026 - 09 - 11);
    let mut music_store = std::env::var("NRS_MUSIC_STORE_URL").ok();
    let mut anime_store = std::env::var("NRS_ANIME_STORE_URL").ok();
    let mut show = 10;
    let mut tolerance = 0.0_f32;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .unwrap_or_else(|| fail("missing value for", &arg))
        };
        match arg.as_str() {
            "--today" => {
                let format = time::format_description::well_known::Iso8601::DATE;
                today = time::Date::parse(&value(), &format).unwrap_or_else(|e| fail("--today", e));
            }
            "--music-store" => music_store = Some(value()),
            "--anime-store" => anime_store = Some(value()),
            "--show" => show = value().parse().unwrap_or_else(|e| fail("--show", e)),
            "--tolerance" => tolerance = value().parse().unwrap_or_else(|e| fail("--tolerance", e)),
            _ => positional.push(arg),
        }
    }
    let [legacy, stripped] = <[String; 2]>::try_from(positional).unwrap_or_else(|_| {
        fail(
            "usage",
            "strip_check <legacy .xml> <stripped .xml> [options]",
        )
    });
    let (legacy, stripped) = (PathBuf::from(legacy), PathBuf::from(stripped));

    let none = nrs_import::StoreSources::default();
    let sources = nrs_import::StoreSources::from_env(&music_store, &anime_store);
    let (a, wa) = build(&legacy, today, &none);
    let (b, wb) = build(&stripped, today, &sources);
    println!(
        "legacy:   {} entries, {} impacts, {} relations, {wa} warnings",
        a.entries.len(),
        a.impacts.len(),
        a.relations.len()
    );
    println!(
        "stripped: {} entries, {} impacts, {} relations, {wb} warnings",
        b.entries.len(),
        b.impacts.len(),
        b.relations.len()
    );

    let mut differences = 0;
    differences += diff(
        "entries",
        &counts(a.entries.iter().map(|e| e.id.0.clone())),
        &counts(b.entries.iter().map(|e| e.id.0.clone())),
        show,
    );
    let impact_key = |i: &nrs_core::Impact| format!("{} {:?}", i.target.0, i.atoms);
    let (a_impacts, b_impacts) = if tolerance > 0.0 {
        pair_close(&a.impacts, &b.impacts, tolerance, show)
    } else {
        (a.impacts.iter().collect(), b.impacts.iter().collect())
    };
    differences += diff(
        "impacts",
        &counts(a_impacts.into_iter().map(impact_key)),
        &counts(b_impacts.into_iter().map(impact_key)),
        show,
    );
    let relation_key = |r: &nrs_core::Relation| {
        format!("{:?} {} -> {} {:?}", r.kind, r.from.0, r.to.0, r.transform)
    };
    differences += diff(
        "relations",
        &counts(a.relations.iter().map(relation_key)),
        &counts(b.relations.iter().map(relation_key)),
        show,
    );

    let titles: BTreeMap<&str, Option<&str>> = a
        .entries
        .iter()
        .map(|e| (e.id.0.as_str(), e.title_cache.as_deref()))
        .collect();
    let mut changed = 0;
    let mut missing = 0;
    for e in &b.entries {
        match (
            titles.get(e.id.0.as_str()).copied().flatten(),
            e.title_cache.as_deref(),
        ) {
            (_, None) => missing += 1,
            (Some(old), Some(new)) if old != new => {
                if changed < show {
                    println!("  title {}: {old:?} -> {new:?}", e.id.0);
                }
                changed += 1;
            }
            _ => {}
        }
    }
    println!(
        "titles: {changed} differ from legacy (store-owned), {missing} stripped entries have none"
    );

    if differences > 0 {
        exit(2);
    }
}
