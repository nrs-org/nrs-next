//! Thin CLI wrapper around `nrs_fit::fit` (INFRA.md §8): open a `.db`,
//! load the snapshot/prior params/recorded preferences, run the fit,
//! print the result, and optionally write it back with `--save`.
//!
//! Usage: `cargo run -p nrs-fit -- /path/to/nrs.db [--save]`

use nrs_core::Homogeneity;
use nrs_store::Store;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = match args.next() {
        Some(p) => p,
        None => {
            eprintln!("usage: nrs-fit <path to nrs.db> [--save]");
            std::process::exit(1);
        }
    };
    let save = args.any(|a| a == "--save");

    let mut store = match Store::open(&path) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("failed to open store: {e}");
            std::process::exit(1);
        }
    };

    let (snapshot, prior) = match store.load_snapshot() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("failed to load snapshot: {e}");
            std::process::exit(1);
        }
    };
    let preferences = match store.load_preferences() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("failed to load preferences: {e}");
            std::process::exit(1);
        }
    };

    println!(
        "{} entries, {} recorded preferences",
        snapshot.entries.len(),
        preferences.len()
    );
    let prior_beta = match prior.homogeneity {
        Homogeneity::Spectral { beta } => format!("{beta:.4}"),
        _ => "n/a (prior isn't Spectral)".to_string(),
    };
    println!("prior beta: {prior_beta}");

    let result = nrs_fit::fit(&snapshot, &prior, &preferences);
    println!(
        "fitted beta: {:.4}  loss: {:.6}  violations: {}/{}",
        result.beta,
        result.loss,
        result.violations,
        preferences.len()
    );

    if save {
        let params = &result.params;
        if let Err(e) = store.set_params(
            params.q,
            params.k_src_levels,
            params.homogeneity,
            params.relation_scaling,
        ) {
            eprintln!("failed to save params: {e}");
            std::process::exit(1);
        }
        println!("saved.");
    } else {
        println!("(dry run -- pass --save to write this back)");
    }
}
