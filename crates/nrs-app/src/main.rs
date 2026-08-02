//! A tiny local server for browsing scores over a real `nrsml`
//! dataset — the "just let me look at this without needing a
//! spreadsheet app" tool. Parses + scores the dataset once at
//! startup (the expensive part: XML parsing, macro expansion, and
//! optional `<script>` execution) and caches the resulting
//! `Snapshot`; each request just re-runs `nrs_core::score()` (cheap
//! at this scale, since the whole document is a few thousand atoms),
//! so the `q` (redundancy exponent) box on the page reruns live.
//!
//! No calibration/fitting exists yet (`nrs-fit`, INFRA.md roadmap
//! phase 6), so what's shown is the raw `V` value from the
//! redundancy functional, not a calibrated public score — labelled
//! honestly as "V (raw)" rather than "Score".
//!
//! Usage: `cargo run -p nrs-app -- /path/to/NRSImplMain.xml [port]`

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::{Json, Router};
use nrs_core::{
    EntryId, Homogeneity, Isotonic, KFeatMatrix, KSrcLevels, Params, RelationScaling, Snapshot,
};
use serde::{Deserialize, Serialize};

struct AppState {
    snapshot: Snapshot,
    warnings: Vec<String>,
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let path = match args.next() {
        Some(p) => p,
        None => {
            eprintln!("usage: nrs-app <path to NRSImplMain.xml> [port]");
            std::process::exit(1);
        }
    };
    let port: u16 = args.next().and_then(|s| s.parse().ok()).unwrap_or(3737);

    eprintln!("parsing {path}...");
    let today = time::OffsetDateTime::now_utc().date();
    let doc = match nrs_import::parse_file(std::path::Path::new(&path), today) {
        Ok(doc) => doc,
        Err(e) => {
            eprintln!("parse error: {e}");
            std::process::exit(1);
        }
    };
    let (snapshot, conversion_warnings) = nrs_import::build_snapshot(&doc);
    let mut warnings = doc.warnings;
    warnings.extend(conversion_warnings);
    eprintln!(
        "loaded {} entries, {} impacts, {} relations ({} warnings)",
        snapshot.entries.len(),
        snapshot.impacts.len(),
        snapshot.relations.len(),
        warnings.len()
    );

    let state = Arc::new(AppState { snapshot, warnings });
    let app = Router::new()
        .route("/", get(index))
        .route("/scores.csv", get(scores_csv))
        .route("/scores.json", get(scores_json))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    eprintln!("listening on http://{addr}");
    axum::serve(listener, app).await.expect("serve");
}

#[derive(Deserialize)]
struct ViewParams {
    #[serde(default = "default_q")]
    q: f32,
    #[serde(default)]
    sort: SortBy,
    #[serde(default)]
    dir: SortDir,
    #[serde(default)]
    rel: RelScaling,
    #[serde(default)]
    mode: Mode,
    #[serde(default = "default_beta")]
    beta: f32,
}

fn default_q() -> f32 {
    0.6
}

fn default_beta() -> f32 {
    2.0
}

/// Which redundancy functional to score with. `q` still applies under
/// both (it drives the relation borrow-solve, and is `DegreeQ`'s
/// exponent); `beta` only bites under `Spectral`.
#[derive(Deserialize, Default, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Mode {
    #[default]
    Degreeq,
    Spectral,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Degreeq => "degreeq",
            Mode::Spectral => "spectral",
        }
    }
}

#[derive(Deserialize, Default, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum SortBy {
    #[default]
    Value,
    Title,
    Id,
}

#[derive(Deserialize, Default, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum SortDir {
    #[default]
    Desc,
    Asc,
}

/// Mirrors `nrs_core::RelationScaling` (kept as a separate app-local
/// enum so `nrs-core` doesn't need a serde dependency for this).
#[derive(Deserialize, Default, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum RelScaling {
    #[default]
    Fixed,
    Powq,
}

impl RelScaling {
    fn to_core(self) -> RelationScaling {
        match self {
            RelScaling::Fixed => RelationScaling::Fixed,
            RelScaling::Powq => RelationScaling::PowQ,
        }
    }
    fn name(self) -> &'static str {
        match self {
            RelScaling::Fixed => "fixed",
            RelScaling::Powq => "powq",
        }
    }
}

fn params(rel: RelScaling, mode: Mode, beta: f32) -> Params {
    let homogeneity = match mode {
        Mode::Degreeq => Homogeneity::DegreeQ,
        Mode::Spectral => Homogeneity::Spectral {
            beta: beta.clamp(1e-3, 1e6),
        },
    };
    Params {
        q: 0.6,
        k_src_levels: KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        },
        k_feat: KFeatMatrix::default(),
        homogeneity,
        calibration: Isotonic::default(),
        relation_scaling: rel.to_core(),
    }
}

struct Row<'a> {
    id: &'a EntryId,
    title: &'a str,
    store: &'a str,
    value: f32,
}

#[allow(clippy::too_many_arguments)] // one flat view-state struct's worth of knobs
fn rows(
    snapshot: &Snapshot,
    q: f32,
    sort: SortBy,
    dir: SortDir,
    rel: RelScaling,
    mode: Mode,
    beta: f32,
) -> Vec<Row<'_>> {
    let mut p = params(rel, mode, beta);
    p.q = q.clamp(0.001, 4.0);
    let scores = nrs_core::score(snapshot, &p);

    let mut rows: Vec<Row> = snapshot
        .entries
        .iter()
        .map(|e| Row {
            id: &e.id,
            title: e.title_cache.as_deref().unwrap_or(""),
            store: &e.store,
            value: scores.values.get(&e.id).copied().unwrap_or(0.0),
        })
        .collect();

    rows.sort_by(|a, b| match sort {
        SortBy::Value => a
            .value
            .partial_cmp(&b.value)
            .unwrap_or(std::cmp::Ordering::Equal),
        SortBy::Title => a.title.cmp(b.title),
        SortBy::Id => a.id.0.cmp(&b.id.0),
    });
    if dir == SortDir::Desc {
        rows.reverse();
    }
    rows
}

async fn index(
    State(state): State<Arc<AppState>>,
    Query(view): Query<ViewParams>,
) -> impl IntoResponse {
    let rows = rows(
        &state.snapshot,
        view.q,
        view.sort,
        view.dir,
        view.rel,
        view.mode,
        view.beta,
    );

    let mut body = String::new();
    write!(
        body,
        r#"<!doctype html><html><head><meta charset="utf-8">
<title>nrs scores</title>
<link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/handsontable/dist/handsontable.full.min.css">
<style>
  :root {{ color-scheme: light dark; }}
  body {{ font-family: system-ui, sans-serif; max-width: 1100px; margin: 2rem auto; padding: 0 1rem; }}
  h1 {{ font-size: 1.3rem; }}
  .meta {{ color: #888; font-size: 0.85rem; margin-bottom: 1rem; }}
  form {{ margin-bottom: 1rem; display: flex; gap: 0.5rem; align-items: center; }}
  #grid {{ width: 100%; }}
</style></head><body>
<h1>nrs scores</h1>
<div class="meta">{} entries · {} warnings during import · uncalibrated (raw <em>V</em>, not a public score) · <a href="/scores.csv?q={}&rel={}&mode={}&beta={}">download CSV</a></div>
<form method="get">
  <label>mode:
    <select name="mode">
      <option value="degreeq" {}>degreeq (V = &Sigma; m&middot;c^(q-1))</option>
      <option value="spectral" {}>spectral (V = &Sigma; &beta;&middot;ln(1+&sigma;/&beta;), monotone)</option>
    </select>
  </label>
  <label>q (redundancy exponent): <input type="number" name="q" value="{}" step="any" min="0.001" max="4"></label>
  <label>&beta; (spectral saturation): <input type="number" name="beta" value="{}" step="any" min="0.001"></label>
  <label>relation scaling:
    <select name="rel">
      <option value="fixed" {}>fixed (spec default)</option>
      <option value="powq" {}>powq (w^(1/q), dampens diluted relations at low q)</option>
    </select>
  </label>
  <input type="hidden" name="sort" value="{}">
  <input type="hidden" name="dir" value="{}">
  <button type="submit">rerun</button>
</form>
<div id="grid"></div>
<script src="https://cdn.jsdelivr.net/npm/handsontable/dist/handsontable.full.min.js"></script>
<script>
  fetch('/scores.json' + location.search)
    .then(function (r) {{ return r.json(); }})
    .then(function (data) {{
      new Handsontable(document.getElementById('grid'), {{
        data: data,
        colHeaders: ['id', 'title', 'store', 'V (raw)'],
        columns: [
          {{ data: 'id' }},
          {{ data: 'title' }},
          {{ data: 'store' }},
          {{ data: 'value', type: 'numeric', numericFormat: {{ pattern: '0.0000' }} }},
        ],
        columnSorting: true,
        filters: true,
        dropdownMenu: true,
        stretchH: 'all',
        height: 600,
        rowHeaders: true,
        licenseKey: 'non-commercial-and-evaluation',
      }});
    }});
</script>
</body></html>
"#,
        rows.len(),
        state.warnings.len(),
        view.q,
        view.rel.name(),
        view.mode.name(),
        view.beta,
        if view.mode == Mode::Degreeq { "selected" } else { "" },
        if view.mode == Mode::Spectral { "selected" } else { "" },
        view.q,
        view.beta,
        if view.rel == RelScaling::Fixed { "selected" } else { "" },
        if view.rel == RelScaling::Powq { "selected" } else { "" },
        sort_by_name(view.sort),
        sort_dir_name(view.dir),
    )
    .unwrap();

    Html(body)
}

fn sort_by_name(s: SortBy) -> &'static str {
    match s {
        SortBy::Value => "value",
        SortBy::Title => "title",
        SortBy::Id => "id",
    }
}
fn sort_dir_name(d: SortDir) -> &'static str {
    match d {
        SortDir::Desc => "desc",
        SortDir::Asc => "asc",
    }
}

fn csv_field(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

async fn scores_csv(
    State(state): State<Arc<AppState>>,
    Query(view): Query<ViewParams>,
) -> impl IntoResponse {
    let rows = rows(
        &state.snapshot,
        view.q,
        view.sort,
        view.dir,
        view.rel,
        view.mode,
        view.beta,
    );

    let mut csv = String::from("id,title,store,value\n");
    for row in &rows {
        writeln!(
            csv,
            "{},{},{},{:.6}",
            csv_field(&row.id.0),
            csv_field(row.title),
            csv_field(row.store),
            row.value
        )
        .unwrap();
    }

    (
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"scores.csv\"",
            ),
        ],
        csv,
    )
}

#[derive(Serialize)]
struct RowJson {
    id: String,
    title: String,
    store: String,
    value: f32,
}

async fn scores_json(
    State(state): State<Arc<AppState>>,
    Query(view): Query<ViewParams>,
) -> impl IntoResponse {
    let rows = rows(
        &state.snapshot,
        view.q,
        view.sort,
        view.dir,
        view.rel,
        view.mode,
        view.beta,
    );

    Json(
        rows.iter()
            .map(|row| RowJson {
                id: row.id.0.clone(),
                title: row.title.to_string(),
                store: row.store.to_string(),
                value: row.value,
            })
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrs_core::{Entry, Impact, ImpactId, Palette, Taxonomy};

    #[test]
    fn csv_field_quotes_and_escapes_embedded_quotes() {
        assert_eq!(csv_field("plain"), "\"plain\"");
        assert_eq!(csv_field("has \"quotes\""), "\"has \"\"quotes\"\"\"");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
    }

    fn tiny_snapshot() -> Snapshot {
        Snapshot {
            entries: vec![
                Entry {
                    id: EntryId("a".into()),
                    store: "s".into(),
                    store_id: "a".into(),
                    title_cache: Some("Zebra".into()),
                },
                Entry {
                    id: EntryId("b".into()),
                    store: "s".into(),
                    store_id: "b".into(),
                    title_cache: Some("Apple".into()),
                },
            ],
            impacts: vec![
                Impact {
                    id: ImpactId("i1".into()),
                    target: EntryId("a".into()),
                    atoms: vec![nrs_core::Atom {
                        source: nrs_core::SourceId("a".into()),
                        blend: vec![(nrs_core::AnchorId("x".into()), 1.0)],
                        magnitude: 5.0,
                        time: nrs_core::TimeSupport::Moment,
                    }],
                },
                Impact {
                    id: ImpactId("i2".into()),
                    target: EntryId("b".into()),
                    atoms: vec![nrs_core::Atom {
                        source: nrs_core::SourceId("b".into()),
                        blend: vec![(nrs_core::AnchorId("x".into()), 1.0)],
                        magnitude: 1.0,
                        time: nrs_core::TimeSupport::Moment,
                    }],
                },
            ],
            relations: vec![],
            palette: Palette {
                anchors: Vec::new(),
                k_feat: KFeatMatrix::default(),
            },
            taxonomy: Taxonomy::default(),
        }
    }

    #[test]
    fn rows_sort_by_value_descending_by_default() {
        let snap = tiny_snapshot();
        let r = rows(
            &snap,
            0.6,
            SortBy::Value,
            SortDir::Desc,
            RelScaling::Fixed,
            Mode::Degreeq,
            2.0,
        );
        assert_eq!(r[0].id.0, "a"); // higher magnitude -> higher value
        assert_eq!(r[1].id.0, "b");
    }

    #[test]
    fn rows_sort_by_title_ascending() {
        let snap = tiny_snapshot();
        let r = rows(
            &snap,
            0.6,
            SortBy::Title,
            SortDir::Asc,
            RelScaling::Fixed,
            Mode::Degreeq,
            2.0,
        );
        assert_eq!(r[0].title, "Apple");
        assert_eq!(r[1].title, "Zebra");
    }

    #[test]
    fn spectral_mode_produces_finite_ordered_values() {
        let snap = tiny_snapshot();
        let r = rows(
            &snap,
            0.6,
            SortBy::Value,
            SortDir::Desc,
            RelScaling::Fixed,
            Mode::Spectral,
            2.0,
        );
        assert!(r.iter().all(|row| row.value.is_finite()));
        // Entry "a" (mass 5) still outranks "b" (mass 1) under spectral.
        assert_eq!(r[0].id.0, "a");
        assert!(r[0].value > r[1].value);
    }
}
