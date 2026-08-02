//! A tiny local server for browsing scores over a real `nrsml`
//! dataset — the "just let me look at this without needing a
//! spreadsheet app" tool. Loads + scores the dataset once at
//! startup and caches the resulting `Snapshot`; each request just
//! re-runs `nrs_core::score()` (cheap at this scale, since the whole
//! document is a few thousand atoms), so the `q` (redundancy
//! exponent) box on the page reruns live.
//!
//! The raw `V` value from the redundancy functional is shown as-is
//! (labelled "V (raw)"), not a calibrated public score; `/calibrate`
//! (INFRA.md roadmap phase 6, `nrs-fit`) exists to work towards one.
//!
//! Usage: `cargo run -p nrs-app -- /path/to/NRSImplMain.xml [port]`
//! or `cargo run -p nrs-app -- /path/to/nrs.db [port]` (a path
//! ending in `.db` loads via `nrs-store` instead of re-parsing XML,
//! and unlocks `/calibrate` -- XML mode is read-only).

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{Form, Query, State};
use axum::http::header;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{get, post};
use axum::{Json, Router};
use nrs_core::{
    AnchorId, EntryId, Homogeneity, Isotonic, KFeatMatrix, KSrcLevels, Params, RelationScaling,
    Snapshot,
};
use nrs_store::Store;
use serde::{Deserialize, Serialize};

struct AppState {
    snapshot: Snapshot,
    warnings: Vec<String>,
    /// Only set when launched against a `.db` path — the calibration UI
    /// (INFRA.md roadmap phase 6) needs write access, which the
    /// read-only XML-mode doesn't have.
    store: Option<Mutex<Store>>,
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

    let is_db = std::path::Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        == Some("db");

    let (snapshot, warnings, store) = if is_db {
        eprintln!("loading {path}...");
        let store = match Store::open(&path) {
            Ok(store) => store,
            Err(e) => {
                eprintln!("failed to open store: {e}");
                std::process::exit(1);
            }
        };
        match store.load_snapshot() {
            Ok((snapshot, _params)) => (snapshot, Vec::new(), Some(store)),
            Err(e) => {
                eprintln!("failed to load snapshot: {e}");
                std::process::exit(1);
            }
        }
    } else {
        eprintln!("parsing {path}...");
        let today = time::OffsetDateTime::now_utc().date();
        let doc = match nrs_import::parse_file(std::path::Path::new(&path), today) {
            Ok(doc) => doc,
            Err(e) => {
                eprintln!("parse error: {e}");
                std::process::exit(1);
            }
        };
        // XML-mode is read-only browsing (no Store), so BuildExtras
        // (comments/needs-review, feeding the review UI) has nowhere
        // to go here -- only meaningful in `.db` mode.
        let (snapshot, _extras, conversion_warnings) = nrs_import::build_snapshot(&doc);
        let mut warnings = doc.warnings;
        warnings.extend(conversion_warnings);
        (snapshot, warnings, None)
    };
    eprintln!(
        "loaded {} entries, {} impacts, {} relations ({} warnings)",
        snapshot.entries.len(),
        snapshot.impacts.len(),
        snapshot.relations.len(),
        warnings.len()
    );
    if store.is_none() {
        eprintln!("(read-only XML mode -- /calibrate needs a .db path)");
    }

    let state = Arc::new(AppState {
        snapshot,
        warnings,
        store: store.map(Mutex::new),
    });
    let app = Router::new()
        .route("/", get(index))
        .route("/scores.csv", get(scores_csv))
        .route("/scores.json", get(scores_json))
        .route("/calibrate", get(calibrate))
        .route("/calibrate/vote", post(calibrate_vote))
        .route("/calibrate/fit", get(calibrate_fit))
        .route("/calibrate/fit/save", post(calibrate_fit_save))
        .route("/calibrate/kfeat", get(calibrate_kfeat))
        .route("/calibrate/kfeat/set", post(calibrate_kfeat_set))
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

fn params(rel: RelScaling, mode: Mode, beta: f32, k_feat: KFeatMatrix) -> Params {
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
        k_feat,
        homogeneity,
        calibration: Isotonic::default(),
        relation_scaling: rel.to_core(),
    }
}

/// The live `k_feat` calibrated via `/calibrate/kfeat`, when running
/// against a `.db` -- otherwise the identity default. Without this, the
/// scores view would be completely blind to anything calibrated there.
fn live_k_feat(state: &AppState) -> KFeatMatrix {
    let Some(store_lock) = &state.store else {
        return KFeatMatrix::default();
    };
    let store = store_lock.lock().unwrap();
    match store.load_snapshot() {
        Ok((_, params)) => params.k_feat,
        Err(_) => KFeatMatrix::default(),
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
    k_feat: KFeatMatrix,
) -> Vec<Row<'_>> {
    let mut p = params(rel, mode, beta, k_feat);
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
    let k_feat = live_k_feat(&state);
    let rows = rows(
        &state.snapshot,
        view.q,
        view.sort,
        view.dir,
        view.rel,
        view.mode,
        view.beta,
        k_feat,
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
<div class="meta">{} entries · {} warnings during import · uncalibrated (raw <em>V</em>, not a public score) · <a href="/scores.csv?q={}&rel={}&mode={}&beta={}">download CSV</a> · <a href="/calibrate">calibrate</a></div>
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
    let k_feat = live_k_feat(&state);
    let rows = rows(
        &state.snapshot,
        view.q,
        view.sort,
        view.dir,
        view.rel,
        view.mode,
        view.beta,
        k_feat,
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
    let k_feat = live_k_feat(&state);
    let rows = rows(
        &state.snapshot,
        view.q,
        view.sort,
        view.dir,
        view.rel,
        view.mode,
        view.beta,
        k_feat,
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

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn no_store_message() -> String {
    "<!doctype html><p>calibration needs write access -- launch <code>nrs-app</code> against a \
     <code>.db</code> path instead of XML.</p>"
        .to_string()
}

fn page_shell(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8">
<title>{title}</title>
<style>
  :root {{ color-scheme: light dark; }}
  body {{ font-family: system-ui, sans-serif; max-width: 700px; margin: 2rem auto; padding: 0 1rem; }}
  nav a {{ margin-right: 1rem; }}
  .pair {{ display: flex; gap: 1rem; margin: 1.5rem 0; }}
  .pair form {{ flex: 1; border: 1px solid #8884; border-radius: 6px; padding: 1rem; text-align: center; }}
  .pair button {{ width: 100%; padding: 0.75rem; font-size: 1rem; }}
  .value {{ color: #888; font-size: 0.85rem; }}
  dl {{ display: grid; grid-template-columns: max-content auto; gap: 0.25rem 1rem; }}
  .kfeat-grid {{ display: flex; flex-direction: column; gap: 0.3rem; }}
  .kfeat-row {{ display: flex; align-items: center; gap: 0.5rem; }}
  .kfeat-row input[type="number"] {{ width: 5rem; }}
</style></head><body>
<nav><a href="/">scores</a><a href="/calibrate">calibrate</a><a href="/calibrate/fit">run fit</a><a href="/calibrate/kfeat">k_feat</a></nav>
{body}
</body></html>
"#
    )
}

/// A random pair for the calibrate page's "which is better" vote
/// (INFRA.md §8's "corrections" -- v1 doesn't try to pick an
/// informative pair, just two distinct entries).
async fn calibrate(State(state): State<Arc<AppState>>) -> Html<String> {
    let Some(store_lock) = &state.store else {
        return Html(no_store_message());
    };
    if state.snapshot.entries.len() < 2 {
        return Html(page_shell(
            "nrs calibrate",
            "<p>need at least two entries to compare.</p>",
        ));
    }

    let live_params = {
        let store = store_lock.lock().unwrap();
        match store.load_snapshot() {
            Ok((_, params)) => params,
            Err(e) => {
                return Html(page_shell(
                    "nrs calibrate",
                    &format!("<p>failed to load params: {e}</p>"),
                ))
            }
        }
    };
    let scores = nrs_core::score(&state.snapshot, &live_params);

    let n = state.snapshot.entries.len();
    let mut rng = rand::rng();
    let idx_a = rand::Rng::random_range(&mut rng, 0..n);
    let mut idx_b = rand::Rng::random_range(&mut rng, 0..n);
    while idx_b == idx_a {
        idx_b = rand::Rng::random_range(&mut rng, 0..n);
    }
    let a = &state.snapshot.entries[idx_a];
    let b = &state.snapshot.entries[idx_b];
    let value_a = scores.values.get(&a.id).copied().unwrap_or(0.0);
    let value_b = scores.values.get(&b.id).copied().unwrap_or(0.0);

    let body = format!(
        r#"<h1>calibrate</h1>
<p>Which of these do you actually think is better?</p>
<div class="pair">
  <form method="post" action="/calibrate/vote">
    <input type="hidden" name="winner" value="{a_id}">
    <input type="hidden" name="loser" value="{b_id}">
    <button type="submit">{a_title}</button>
    <div class="value">current V: {value_a:.3}</div>
  </form>
  <form method="post" action="/calibrate/vote">
    <input type="hidden" name="winner" value="{b_id}">
    <input type="hidden" name="loser" value="{a_id}">
    <button type="submit">{b_title}</button>
    <div class="value">current V: {value_b:.3}</div>
  </form>
</div>
"#,
        a_id = html_escape(&a.id.0),
        b_id = html_escape(&b.id.0),
        a_title = html_escape(a.title_cache.as_deref().unwrap_or(&a.id.0)),
        b_title = html_escape(b.title_cache.as_deref().unwrap_or(&b.id.0)),
    );
    Html(page_shell("nrs calibrate", &body))
}

#[derive(Deserialize)]
struct VoteForm {
    winner: String,
    loser: String,
}

async fn calibrate_vote(
    State(state): State<Arc<AppState>>,
    Form(form): Form<VoteForm>,
) -> impl IntoResponse {
    let Some(store_lock) = &state.store else {
        return Html(no_store_message()).into_response();
    };
    let winner = EntryId(form.winner);
    let loser = EntryId(form.loser);
    let mut store = store_lock.lock().unwrap();
    if let Err(e) = store.record_preference(&winner, &loser, None) {
        return Html(page_shell(
            "nrs calibrate",
            &format!("<p>failed to record: {e}</p>"),
        ))
        .into_response();
    }
    drop(store);
    Redirect::to("/calibrate").into_response()
}

async fn calibrate_fit(State(state): State<Arc<AppState>>) -> Html<String> {
    let Some(store_lock) = &state.store else {
        return Html(no_store_message());
    };
    let store = store_lock.lock().unwrap();
    let (_, prior) = match store.load_snapshot() {
        Ok(pair) => pair,
        Err(e) => {
            return Html(page_shell(
                "nrs fit",
                &format!("<p>failed to load: {e}</p>"),
            ))
        }
    };
    let preferences = match store.load_preferences() {
        Ok(p) => p,
        Err(e) => {
            return Html(page_shell(
                "nrs fit",
                &format!("<p>failed to load preferences: {e}</p>"),
            ))
        }
    };
    drop(store);

    let result = nrs_fit::fit(&state.snapshot, &prior, &preferences);
    let prior_beta = match prior.homogeneity {
        Homogeneity::Spectral { beta } => format!("{beta:.4}"),
        _ => "n/a (prior isn't spectral)".to_string(),
    };

    let body = format!(
        r#"<h1>fit</h1>
<dl>
  <dt>preferences recorded</dt><dd>{n_preferences}</dd>
  <dt>prior beta</dt><dd>{prior_beta}</dd>
  <dt>fitted beta</dt><dd>{fitted_beta:.4}</dd>
  <dt>loss</dt><dd>{loss:.6}</dd>
  <dt>violations</dt><dd>{violations} / {n_preferences}</dd>
</dl>
<form method="post" action="/calibrate/fit/save">
  <button type="submit">save fitted beta</button>
</form>
"#,
        n_preferences = preferences.len(),
        fitted_beta = result.beta,
        loss = result.loss,
        violations = result.violations,
    );
    Html(page_shell("nrs fit", &body))
}

async fn calibrate_fit_save(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(store_lock) = &state.store else {
        return Html(no_store_message()).into_response();
    };
    let mut store = store_lock.lock().unwrap();
    let (_, prior) = match store.load_snapshot() {
        Ok(pair) => pair,
        Err(e) => {
            return Html(page_shell(
                "nrs fit",
                &format!("<p>failed to load: {e}</p>"),
            ))
            .into_response()
        }
    };
    let preferences = match store.load_preferences() {
        Ok(p) => p,
        Err(e) => {
            return Html(page_shell(
                "nrs fit",
                &format!("<p>failed to load preferences: {e}</p>"),
            ))
            .into_response()
        }
    };

    let result = nrs_fit::fit(&state.snapshot, &prior, &preferences);
    let params = &result.params;
    if let Err(e) = store.set_params(
        params.q,
        params.k_src_levels,
        params.homogeneity,
        params.relation_scaling,
    ) {
        return Html(page_shell(
            "nrs fit",
            &format!("<p>failed to save: {e}</p>"),
        ))
        .into_response();
    }
    drop(store);
    Redirect::to("/calibrate/fit").into_response()
}

/// Every distinct anchor actually used by some atom's blend in
/// `snapshot` -- `Palette.anchors` isn't populated by `nrs-import` yet,
/// so this is the honest source of "what anchors exist" for real data.
fn used_anchors(snapshot: &Snapshot) -> Vec<AnchorId> {
    let mut set = std::collections::BTreeSet::new();
    for impact in &snapshot.impacts {
        for atom in &impact.atoms {
            for (anchor, _) in &atom.blend {
                set.insert(anchor.clone());
            }
        }
    }
    set.into_iter().collect()
}

/// `k_feat` is directly interpretable (unlike `beta`), so this skips
/// `nrs-fit` entirely: pick two anchors, say how redundant they feel,
/// write straight into `k_feat` via the existing `set_kfeat_pair`.
async fn calibrate_kfeat(State(state): State<Arc<AppState>>) -> Html<String> {
    let Some(store_lock) = &state.store else {
        return Html(no_store_message());
    };
    let k_feat = {
        let store = store_lock.lock().unwrap();
        match store.load_snapshot() {
            Ok((_, params)) => params.k_feat,
            Err(e) => {
                return Html(page_shell(
                    "nrs k_feat",
                    &format!("<p>failed to load: {e}</p>"),
                ))
            }
        }
    };

    let anchors = used_anchors(&state.snapshot);
    if anchors.len() < 2 {
        return Html(page_shell(
            "nrs k_feat",
            "<p>need at least two anchors in use to compare.</p>",
        ));
    }

    let mut rows = String::new();
    for i in 0..anchors.len() {
        for j in (i + 1)..anchors.len() {
            let a = &anchors[i];
            let b = &anchors[j];
            let current = k_feat.get(a, b);
            write!(
                rows,
                r#"<form method="post" action="/calibrate/kfeat/set" class="kfeat-row">
  <span>{a_label}</span> &harr; <span>{b_label}</span>
  <input type="hidden" name="a" value="{a_val}">
  <input type="hidden" name="b" value="{b_val}">
  <input type="number" name="k" value="{current}" step="0.05" min="0" max="1">
  <button type="submit">save</button>
</form>
"#,
                a_label = html_escape(&a.0),
                b_label = html_escape(&b.0),
                a_val = html_escape(&a.0),
                b_val = html_escape(&b.0),
            )
            .unwrap();
        }
    }

    let body = format!(
        r#"<h1>k_feat</h1>
<p>How redundant does each anchor pair feel? 0 = unrelated, 1 = fully redundant
(the same signal counted twice).</p>
<div class="kfeat-grid">
{rows}</div>
"#
    );
    Html(page_shell("nrs k_feat", &body))
}

#[derive(Deserialize)]
struct KfeatForm {
    a: String,
    b: String,
    k: f32,
}

async fn calibrate_kfeat_set(
    State(state): State<Arc<AppState>>,
    Form(form): Form<KfeatForm>,
) -> impl IntoResponse {
    let Some(store_lock) = &state.store else {
        return Html(no_store_message()).into_response();
    };
    let mut store = store_lock.lock().unwrap();
    if let Err(e) =
        store.set_kfeat_pair(&AnchorId(form.a), &AnchorId(form.b), form.k.clamp(0.0, 1.0))
    {
        return Html(page_shell(
            "nrs k_feat",
            &format!("<p>failed to save: {e}</p>"),
        ))
        .into_response();
    }
    drop(store);
    Redirect::to("/calibrate/kfeat").into_response()
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
            KFeatMatrix::default(),
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
            KFeatMatrix::default(),
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
            KFeatMatrix::default(),
        );
        assert!(r.iter().all(|row| row.value.is_finite()));
        // Entry "a" (mass 5) still outranks "b" (mass 1) under spectral.
        assert_eq!(r[0].id.0, "a");
        assert!(r[0].value > r[1].value);
    }
}
