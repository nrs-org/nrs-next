//! A local server for browsing and curating a real `nrsml`/`nrs.db`
//! dataset -- the "just let me look at this without needing a
//! spreadsheet app" tool, in `.db` mode also the write surface for
//! everything CRUD (INFRA.md roadmap phase 5).
//!
//! What's shown is the raw `V` value from the redundancy functional,
//! not a calibrated public score -- labelled honestly as "V (raw)"
//! rather than "Score".
//!
//! Usage: `cargo run -p nrs-app -- /path/to/NRSImplMain.xml [port]`
//! or `cargo run -p nrs-app -- /path/to/nrs.db [port]` (a path ending
//! in `.db` loads via `nrs-store` instead of re-parsing XML; write
//! routes need this mode).
//!
//! Frontend: the dependency-free client app (plain HTML, CSS, and
//! native ES modules in `assets/`) is served at `/`, backed by the JSON
//! API under `/api/v1`.

mod api;
mod score_filter;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::routing::{get, post, put};
use axum::Router;
use nrs_core::{
    AnchorId, Atom, Entry, EntryId, Isotonic, KFeatMatrix, Params, RelKind, Snapshot, SourceId,
    TimeParams,
};
use nrs_entry_store::HttpEntryStore;
use nrs_store::Store;
use serde::Deserialize;
use tower_http::services::{ServeDir, ServeFile};

struct AppState {
    snapshot: Snapshot,
    /// Only set when launched against a `.db` path -- writes need the
    /// store, which the read-only XML-mode doesn't have.
    store: Option<Mutex<TrackedStore>>,
    anime_store: HttpEntryStore,
    /// Where `entry_ingest` sends URLs the anime store doesn't take.
    music_store: HttpEntryStore,
    /// Where progress defaults come from (`NRS_MUSIC_STORE_URL`,
    /// `NRS_ANIME_STORE_URL`); see `api::progress_basis`.
    music_store_url: Option<String>,
    anime_store_url: Option<String>,
    /// Recently scored rankings, so paging and re-sorting the scores
    /// table don't re-run the whole functional (see `api::scored`).
    scores_cache: api::ScoresCache,
    /// `KindHints` as of a store generation (see `api::kind_hints`).
    kind_hints: Mutex<Option<(u64, Arc<KindHints>)>>,
}

/// The store plus a count of its writes. Every `Store` write takes
/// `&mut self` and every read `&self`, so bumping on `DerefMut` counts
/// writes without each handler having to remember to -- anything
/// cached from the store keys on `generation` to know it's stale.
struct TrackedStore {
    store: Store,
    generation: u64,
}

impl TrackedStore {
    fn new(store: Store) -> Self {
        Self {
            store,
            generation: 0,
        }
    }
}

impl std::ops::Deref for TrackedStore {
    type Target = Store;

    fn deref(&self) -> &Store {
        &self.store
    }
}

impl std::ops::DerefMut for TrackedStore {
    fn deref_mut(&mut self) -> &mut Store {
        self.generation += 1;
        &mut self.store
    }
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
        let mut doc = match nrs_import::parse_file(std::path::Path::new(&path), today) {
            Ok(doc) => doc,
            Err(e) => {
                eprintln!("parse error: {e}");
                std::process::exit(1);
            }
        };
        // Stripped XML: titles and store-asserted containment come from the
        // stores (`NRS_MUSIC_STORE_URL`, `NRS_ANIME_STORE_URL`).
        let music_store_url = Some(music_store_url());
        let anime_store_url = Some(
            std::env::var("NRS_ANIME_STORE_URL").unwrap_or_else(|_| "http://127.0.0.1:3738".into()),
        );
        let sources = nrs_import::StoreSources::from_env(&music_store_url, &anime_store_url);
        match nrs_import::resolve_binds(&mut doc, &sources) {
            Ok(store_warnings) => doc.warnings.extend(store_warnings),
            Err(e) => {
                eprintln!("store lookup: {e}");
                std::process::exit(1);
            }
        }
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
        eprintln!("(read-only XML mode -- write routes need a .db path)");
    }

    let anime_store_url =
        std::env::var("NRS_ANIME_STORE_URL").unwrap_or_else(|_| "http://127.0.0.1:3738".into());
    let anime_store = match HttpEntryStore::new(&anime_store_url) {
        Ok(store) => store.expecting_store("anime"),
        Err(error) => {
            eprintln!("invalid NRS_ANIME_STORE_URL: {error}");
            std::process::exit(1);
        }
    };
    eprintln!("anime entry store: {anime_store_url}");
    let music_store = match HttpEntryStore::new(&music_store_url()) {
        // A musiclib ingest fetches every provider, then reconciles.
        Ok(store) => store
            .expecting_store("musiclib")
            .with_job_timeout(std::time::Duration::from_secs(15 * 60)),
        Err(error) => {
            eprintln!("invalid NRS_MUSIC_STORE_URL: {error}");
            std::process::exit(1);
        }
    };
    eprintln!("music entry store: {}", music_store_url());

    let state = Arc::new(AppState {
        snapshot,
        store: store.map(|store| Mutex::new(TrackedStore::new(store))),
        anime_store,
        music_store,
        music_store_url: Some(music_store_url()),
        anime_store_url: Some(anime_store_url),
        scores_cache: Default::default(),
        kind_hints: Default::default(),
    });
    let assets_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");
    let api = Router::new()
        .route("/scores", get(api::scores))
        .route("/scores.csv", get(api::scores_csv))
        .route("/palette", get(api::palette))
        .route(
            "/calibration/pair",
            get(api::calibration_get).post(api::calibration_vote),
        )
        .route(
            "/calibration/kfeat",
            get(api::kfeat_get).put(api::kfeat_set),
        )
        .route("/calibration/time", put(api::time_set))
        .route("/review", get(api::review_queue))
        .route("/entries", get(api::entries_index))
        .route("/entries/ingest", post(api::entry_ingest))
        .route("/entries/{entry_id}", get(api::entry_detail))
        .route("/entries/{entry_id}/impacts", post(api::impact_add))
        .route(
            "/entries/{entry_id}/progress-basis",
            get(api::progress_basis),
        )
        .route("/entries/{entry_id}/tracklist", get(api::entry_tracklist))
        .route(
            "/entries/{entry_id}/impacts/{impact_id}",
            put(api::impact_edit).delete(api::impact_delete),
        )
        .route(
            "/entries/{entry_id}/impacts/{impact_id}/confirm",
            post(api::impact_confirm),
        )
        .route(
            "/entries/{entry_id}/impacts/{impact_id}/raw",
            put(api::impact_raw_edit),
        )
        .route(
            "/entries/{entry_id}/impacts/{impact_id}/shares",
            put(api::impact_shares_edit),
        )
        .route(
            "/entries/{entry_id}/impacts/{impact_id}/share",
            post(api::impact_make_shared),
        )
        .route("/entries/{entry_id}/relations", post(api::relation_add))
        .route(
            "/entries/{entry_id}/relations/{relation_id}",
            put(api::relation_edit).delete(api::relation_delete),
        )
        .fallback(api::not_found);
    // Everything outside `/api/v1` is the client app: real files are
    // served as-is, any other path gets `index.html` so client-side
    // routes survive a reload.
    let app = Router::new()
        .nest("/api/v1", api)
        .fallback_service(
            ServeDir::new(assets_dir).fallback(ServeFile::new(format!("{assets_dir}/index.html"))),
        )
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    eprintln!("listening on http://{addr}");
    axum::serve(listener, app).await.expect("serve");
}

// ---------------------------------------------------------------
// Scores.
// ---------------------------------------------------------------

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

/// musiclib's Store Protocol endpoint: `NRS_MUSIC_STORE_URL`, else its
/// server's default.
fn music_store_url() -> String {
    std::env::var("NRS_MUSIC_STORE_URL")
        .unwrap_or_else(|_| nrs_import::stores::DEFAULT_MUSIC_STORE_URL.into())
}

fn params(k_feat: KFeatMatrix, time: TimeParams) -> Params {
    Params {
        k_feat,
        time,
        calibration: Isotonic::default(),
    }
}

struct Row<'a> {
    id: &'a EntryId,
    title: &'a str,
    store: &'a str,
    value: f32,
}

fn rows(
    snapshot: &Snapshot,
    sort: SortBy,
    dir: SortDir,
    k_feat: KFeatMatrix,
    time: TimeParams,
) -> Vec<Row<'_>> {
    let p = params(k_feat, time);
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

fn csv_field(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

// ---------------------------------------------------------------
// Palette and calibration.
// ---------------------------------------------------------------

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

// ---------------------------------------------------------------
// Impact presentation (entry page and review queue).
// ---------------------------------------------------------------

/// Human-facing identity for an impact kind. The acronym is useful to
/// long-time NRS users, but it is never left to stand on its own: the
/// title and one-line explanation make the old vocabulary readable to
/// somebody who did not memorize DAH_standards.
fn impact_kind_presentation(kind: Option<&str>) -> (&'static str, &'static str, &'static str) {
    match kind {
        Some("cry") => (
            "response",
            "Cry response",
            "A moment strong enough to provoke tears.",
        ),
        Some("nei") => (
            "response",
            "Noticeable emotional impact",
            "A feeling that registered clearly in the moment.",
        ),
        Some("aei") => (
            "response",
            "Appreciable emotional impact",
            "A substantial emotional response with real weight.",
        ),
        Some("ehi") => (
            "highlight",
            "Exceptional humor impact",
            "A standout comic moment rather than general amusement.",
        ),
        Some("epi") => (
            "highlight",
            "Exceptional plot impact",
            "A standout story or revelation moment.",
        ),
        Some("pads") => (
            "response",
            "Persistent aftereffect",
            "An emotional aftereffect that continued beyond the work.",
        ),
        Some("jumpscare") => ("response", "Jumpscare", "A sharp startle response."),
        Some("sleepless_night") => (
            "response",
            "Sleepless night",
            "Impact strong enough to disrupt sleep.",
        ),
        Some("politics") => ("personal", "Politics", "A political or worldview impact."),
        Some("interest_field") => (
            "personal",
            "Interest gateway",
            "Opened or deepened a real-world field of interest.",
        ),
        Some("additional") => (
            "adjustment",
            "Manual adjustment",
            "An explicit score buff or nerf.",
        ),
        Some("music") => (
            "craft",
            "Music craft",
            "Direct judgment of the work's music.",
        ),
        Some("visual") => (
            "craft",
            "Visual craft",
            "Direct judgment of visual execution.",
        ),
        Some("writing") => (
            "craft",
            "Writing craft",
            "Character, story, pacing, and originality.",
        ),
        Some("osu_song") => (
            "personal",
            "osu! song",
            "Personal and community rhythm-game impact.",
        ),
        Some("waifu") => (
            "personal",
            "Character attachment",
            "Sustained attachment to a character.",
        ),
        Some("meme") => (
            "personal",
            "Meme impact",
            "A joke or reference that persisted socially.",
        ),
        Some("consumed") => ("progress", "Consumed", "Time spent, adjusted for boredom."),
        Some("anime_consumed") => (
            "progress",
            "Anime consumed",
            "Episode watch time, adjusted for boredom.",
        ),
        Some("dropped") => ("progress", "Dropped", "The work was abandoned."),
        Some("music_consumed_progress") => (
            "progress",
            "Music consumed",
            "Time spent listening to the music.",
        ),
        Some(_) => ("other", "Other impact", "A standards-based impact."),
        None => (
            "custom",
            "Custom anchor impact",
            "A directly authored anchor composition.",
        ),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImpactSection {
    Profile,
    Responses,
    Resonance,
    Adjustments,
    Other,
}

fn impact_section(kind: Option<&str>) -> ImpactSection {
    match kind {
        Some(
            "music"
            | "visual"
            | "writing"
            | "consumed"
            | "anime_consumed"
            | "music_consumed_progress"
            | "dropped",
        ) => ImpactSection::Profile,
        Some("cry" | "nei" | "aei" | "ehi" | "epi" | "pads" | "jumpscare" | "sleepless_night") => {
            ImpactSection::Responses
        }
        Some("politics" | "interest_field" | "osu_song" | "waifu" | "meme") => {
            ImpactSection::Resonance
        }
        Some("additional") => ImpactSection::Adjustments,
        _ => ImpactSection::Other,
    }
}

fn format_duration_minutes(value: &str) -> String {
    let Some(minutes) = value
        .parse::<f64>()
        .ok()
        .filter(|minutes| minutes.is_finite() && *minutes >= 0.0)
    else {
        return value.to_string();
    };
    let total_seconds = (minutes * 60.0).round() as u64;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn impact_primary_metric(
    kind: Option<&str>,
    fields: &HashMap<String, String>,
    impact: &nrs_store::ReviewImpact,
) -> (String, String, Option<f32>) {
    let bounded = |key: &str| {
        fields
            .get(key)
            .and_then(|value| value.parse::<f32>().ok())
            .map(|value| value.clamp(0.0, 1.0))
    };
    let plain = |key: &str| fields.get(key).filter(|value| !value.is_empty()).cloned();
    match kind {
        Some("nei" | "aei" | "epi") => {
            let value = bounded("factor");
            (
                "strength".into(),
                value
                    .map(|v| format!("{:.0}%", v * 100.0))
                    .unwrap_or_else(|| "—".into()),
                value,
            )
        }
        Some("pads") => (
            "duration".into(),
            plain("days")
                .map(|v| format!("{v}d"))
                .unwrap_or_else(|| "—".into()),
            None,
        ),
        Some("music") => {
            let value = bounded("music_base");
            (
                "quality".into(),
                value
                    .map(|v| format!("{:.0}%", v * 100.0))
                    .unwrap_or_else(|| "—".into()),
                value,
            )
        }
        Some("visual") => {
            let value = bounded("base");
            (
                "quality".into(),
                value
                    .map(|v| format!("{:.0}%", v * 100.0))
                    .unwrap_or_else(|| "—".into()),
                value,
            )
        }
        Some("waifu") => (
            "duration".into(),
            plain("days")
                .map(|v| format!("{v}d"))
                .unwrap_or_else(|| "—".into()),
            None,
        ),
        Some("meme") => (
            "duration".into(),
            plain("days")
                .map(|v| format!("{v}d"))
                .unwrap_or_else(|| "—".into()),
            None,
        ),
        Some("consumed" | "music_consumed_progress") => (
            "duration".into(),
            plain("duration_minutes")
                .map(|value| format_duration_minutes(&value))
                .unwrap_or_else(|| "—".into()),
            None,
        ),
        Some("anime_consumed") => (
            "episodes".into(),
            plain("episodes").unwrap_or_else(|| "—".into()),
            None,
        ),
        Some("additional") => (
            "adjustment".into(),
            plain("value").unwrap_or_else(|| "—".into()),
            None,
        ),
        _ => {
            let magnitude: f32 = impact.atoms.iter().map(|atom| atom.magnitude).sum();
            ("magnitude".into(), format!("{magnitude:.2}"), None)
        }
    }
}

struct PeriodSummary {
    from: String,
    to: String,
    caption: String,
}

fn compact_days(value: &str) -> String {
    value
        .parse::<f32>()
        .map(|days| {
            if days.fract().abs() < 0.005 {
                format!("{days:.0}")
            } else {
                format!("{days:.1}")
            }
        })
        .unwrap_or_else(|_| value.to_string())
}

fn impact_period_summary(
    kind: Option<&str>,
    fields: &HashMap<String, String>,
) -> Option<PeriodSummary> {
    if !matches!(kind, Some("pads" | "waifu" | "meme")) {
        return None;
    }
    let from = fields.get("period_from").filter(|value| !value.is_empty());
    let to = fields.get("period_to").filter(|value| !value.is_empty());
    let from_expression = fields
        .get("period_from_expression")
        .filter(|value| !value.is_empty());
    let to_expression = fields
        .get("period_to_expression")
        .filter(|value| !value.is_empty());
    let days = fields
        .get("days")
        .map(|value| compact_days(value))
        .unwrap_or_else(|| "?".into());
    match (from, to) {
        (Some(from), Some(to)) => {
            let dynamic = from_expression.is_some() || to_expression.is_some();
            Some(PeriodSummary {
                from: from_expression.unwrap_or(from).clone(),
                to: to_expression.unwrap_or(to).clone(),
                caption: if dynamic {
                    format!("{days} days · resolved as {from} → {to} on import")
                } else {
                    format!("{days} days")
                },
            })
        }
        _ => Some(PeriodSummary {
            from: "start".into(),
            to: format!("day {days}"),
            caption: "relative duration · calendar dates not recorded".into(),
        }),
    }
}

/// A display-only composition of the impact's stored atoms. Absolute
/// contribution is used for the percentages so a negative impact still
/// communicates *what it is made of*; polarity remains visible in the
/// signed magnitude metric and the standards parameters.
fn impact_anchor_mix(impact: &nrs_store::ReviewImpact) -> Vec<(String, f32)> {
    let mut mix: Vec<(String, f32)> = Vec::new();
    for atom in &impact.atoms {
        for (anchor, weight) in &atom.blend {
            let contribution = atom.magnitude.abs() * weight.abs();
            match mix.iter_mut().find(|(name, _)| name == &anchor.0) {
                Some((_, total)) => *total += contribution,
                None => mix.push((anchor.0.clone(), contribution)),
            }
        }
    }
    let total: f32 = mix.iter().map(|(_, contribution)| contribution).sum();
    if total > f32::EPSILON {
        for (_, contribution) in &mut mix {
            *contribution /= total;
        }
    }
    mix.sort_by(|a, b| b.1.total_cmp(&a.1));
    mix
}

/// Reads back a form-submission-shaped JSON blob (`{"factor":"0.5",...}`)
/// stored by `set_impact_dah`/`set_relation_dah` -- the same shape
/// `impact_form`/`relation_form` need as prefill, and what's shown as
/// the DAH_standards params readout on the entry page.
fn parsed_dah_params(json: &str) -> HashMap<String, String> {
    serde_json::from_str(json).unwrap_or_default()
}

// ---------------------------------------------------------------
// Entries.
// ---------------------------------------------------------------

fn entry_display_name(e: &Entry) -> &str {
    e.title_cache.as_deref().unwrap_or(&e.id.0)
}

/// Every entry kind, as the slugs `entry_kind` returns. The scores
/// filter accepts these (and `kind_alias`'s spellings) after `type:`.
const ENTRY_KINDS: &[&str] = &[
    "anime",
    "album",
    "track",
    "artist",
    "franchise",
    "game",
    "vn",
    "literature",
    "other",
];

/// The kinds `type:music` stands for.
const MUSIC_KINDS: &[&str] = &["album", "track", "artist"];

/// Whether a `type:` selection (a kind, or the `music` group) covers
/// `kind`.
fn kind_covers(selected: &str, kind: &str) -> bool {
    selected == kind || (selected == "music" && MUSIC_KINDS.contains(&kind))
}

/// The relation facts `entry_kind` uses to tell music apart. Legacy
/// data marked ambiguous music ids with an explicit `entrytype` that
/// the importer doesn't keep, so artists and albums are recognized by
/// what they do instead.
#[derive(Default)]
struct KindHints {
    /// Entries credited on other work.
    credited: std::collections::HashSet<String>,
    /// Entries containing other music.
    contains_music: std::collections::HashSet<String>,
}

impl KindHints {
    fn new(snapshot: &Snapshot) -> Self {
        let mut hints = Self::default();
        for relation in &snapshot.relations {
            match relation.kind {
                RelKind::Credit => {
                    hints.credited.insert(relation.from.0.clone());
                }
                RelKind::Contains if relation.to.0.starts_with("M-") => {
                    hints.contains_music.insert(relation.from.0.clone());
                }
                _ => {}
            }
        }
        hints
    }
}

/// What kind of work an entry is. Legacy ids carry it in their shape,
/// read the way nrsml's `guessEntryTypeFromId` does (`A-MAL-5114` is
/// anime, `M-VGMDB-AL-85759` an album and `M-VGMDB-AL-85759-1` one of
/// its tracks, ...). Music ids that shape doesn't settle are told apart
/// by `hints`; against the legacy XML's explicit types this gets 1208
/// of 1216 music entries right. Entries added through the store since
/// get opaque ULIDs, so those fall back to what their store implies.
fn entry_kind(id: &str, store: &str, hints: &KindHints) -> &'static str {
    match id.split('-').next().unwrap_or("") {
        "A" => "anime",
        "M" if id.starts_with("M-VGMDB-AR-") => "artist",
        "M" if id.starts_with("M-VGMDB-AL-") => match id.split('-').count() {
            4 => "album",
            _ => "track",
        },
        "M" if hints.credited.contains(id) => "artist",
        "M" if hints.contains_music.contains(id) => "album",
        "M" => "track",
        "F" => "franchise",
        // nrsml reads only the first letter, so `GF-` is a game too.
        "G" | "GF" => "game",
        "V" => "vn",
        "L" => "literature",
        "O" => "other",
        _ => match store {
            "mal" | "anilist" | "anime" => "anime",
            "mal-manga" => "literature",
            "vndb" => "vn",
            "vgmdb-album" => "album",
            "vgmdb-artist" => "artist",
            s if s.starts_with("vgmdb") => "track",
            _ => "other",
        },
    }
}

/// `type:` spellings beyond the slugs themselves; `music` is the group
/// in `MUSIC_KINDS`.
fn kind_alias(value: &str) -> Option<&'static str> {
    let value = value.to_lowercase();
    if let Some(kind) = ENTRY_KINDS.iter().find(|kind| **kind == value) {
        return Some(kind);
    }
    Some(match value.as_str() {
        "music" | "m" => "music",
        "a" => "anime",
        "al" | "albums" => "album",
        "song" | "songs" | "tracks" => "track",
        "ar" | "artists" => "artist",
        "f" | "series" => "franchise",
        "g" => "game",
        "v" | "visual-novel" | "visualnovel" => "vn",
        "l" | "lit" | "manga" | "novel" | "ln" | "book" => "literature",
        "o" => "other",
        _ => return None,
    })
}

// ---------------------------------------------------------------
// DAH_standards: impact/relation kinds are named formulas (a "cry"
// gives 4.0 split across its blend, a "waifu" is 1.2*(days/90)^0.35
// on the waifu anchor, ...), not hand-picked anchor/weight/magnitude
// triples. `nrs_import::legacy` already implements every formula
// verbatim (ported from `nrs-lib-ts`, tested against real legacy
// data) -- these helpers are a thin layer over exactly that, so
// creating an impact means picking the kind DAH_standards would call
// it and filling in *its* natural parameters, and the score vector is
// computed from the spec rather than picked by hand. Free-form
// per-anchor editing is still available afterwards via the raw
// impact editor.
// ---------------------------------------------------------------

fn f32_field(fields: &HashMap<String, String>, key: &str) -> f32 {
    f32_field_or(fields, key, 0.0)
}

fn f32_field_or(fields: &HashMap<String, String>, key: &str, default: f32) -> f32 {
    fields
        .get(key)
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(default)
}

fn resolve_period_expression(expression: &str, today: time::Date) -> Result<time::Date, String> {
    if expression == "today" {
        return Ok(today);
    }
    if let Some(offset) = expression.strip_prefix("today+") {
        let days = offset
            .parse::<i64>()
            .map_err(|_| format!("invalid dynamic period endpoint: {expression}"))?;
        return Ok(today + time::Duration::days(days));
    }
    Err(format!("unsupported dynamic period endpoint: {expression}"))
}

fn normalize_period_fields_at(
    kind: &str,
    fields: &mut HashMap<String, String>,
    today: time::Date,
) -> Result<(), String> {
    if !matches!(kind, "pads" | "waifu" | "meme") {
        return Ok(());
    }
    for (date_key, expression_key) in [
        ("period_from", "period_from_expression"),
        ("period_to", "period_to_expression"),
    ] {
        if let Some(expression) = fields
            .get(expression_key)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            let resolved = resolve_period_expression(&expression, today)?;
            fields.insert(date_key.into(), resolved.to_string());
            fields.insert(expression_key.into(), expression);
        }
    }
    let from = fields
        .get("period_from")
        .map(|value| value.trim())
        .unwrap_or("");
    let to = fields
        .get("period_to")
        .map(|value| value.trim())
        .unwrap_or("");
    if from.is_empty() && to.is_empty() {
        return Ok(());
    }
    if from.is_empty() || to.is_empty() {
        return Err("period needs both a start and end date".into());
    }
    let format = &time::format_description::well_known::Iso8601::DATE;
    let from_date = time::Date::parse(from, format).map_err(|_| "invalid period start date")?;
    let to_date = time::Date::parse(to, format).map_err(|_| "invalid period end date")?;
    let days = (to_date - from_date).whole_days();
    if days < 0 {
        return Err("period end date must not be before its start date".into());
    }
    fields.insert("days".into(), days.to_string());
    Ok(())
}

fn normalize_period_fields(kind: &str, fields: &mut HashMap<String, String>) -> Result<(), String> {
    normalize_period_fields_at(kind, fields, time::OffsetDateTime::now_utc().date())
}

fn parse_duration_minutes_input(value: &str) -> Result<f32, String> {
    let value = value.trim();
    if !value.contains(':') {
        return value
            .parse::<f32>()
            .ok()
            .filter(|minutes| minutes.is_finite() && *minutes >= 0.0)
            .ok_or_else(|| "duration must be non-negative minutes, M:SS, or H:MM:SS".into());
    }

    let parts: Vec<&str> = value.split(':').collect();
    let invalid = || "duration must be M:SS or H:MM:SS".to_string();
    let number = |part: &str| part.parse::<f64>().map_err(|_| invalid());
    let total_seconds = match parts.as_slice() {
        [minutes, seconds] => {
            let minutes = number(minutes)?;
            let seconds = number(seconds)?;
            if minutes < 0.0 || !(0.0..60.0).contains(&seconds) {
                return Err(invalid());
            }
            minutes * 60.0 + seconds
        }
        [hours, minutes, seconds] => {
            let hours = number(hours)?;
            let minutes = number(minutes)?;
            let seconds = number(seconds)?;
            if hours < 0.0 || !(0.0..60.0).contains(&minutes) || !(0.0..60.0).contains(&seconds) {
                return Err(invalid());
            }
            hours * 3_600.0 + minutes * 60.0 + seconds
        }
        _ => return Err(invalid()),
    };
    Ok((total_seconds / 60.0) as f32)
}

fn normalize_duration_fields(
    kind: &str,
    fields: &mut HashMap<String, String>,
) -> Result<(), String> {
    let key = match kind {
        "consumed" | "music_consumed_progress" => "duration_minutes",
        "anime_consumed" => "episode_duration_minutes",
        _ => return Ok(()),
    };
    let value = fields.get(key).map(String::as_str).unwrap_or("");
    let minutes = parse_duration_minutes_input(value)?;
    fields.insert(key.into(), minutes.to_string());
    Ok(())
}

fn blend_field(fields: &HashMap<String, String>) -> Vec<(AnchorId, f32)> {
    (1..=4u8)
        .filter_map(|n| {
            let anchor = fields.get(&format!("anchor{n}"))?.trim();
            if anchor.is_empty() {
                return None;
            }
            let weight: f32 = fields.get(&format!("weight{n}"))?.trim().parse().ok()?;
            Some((AnchorId(anchor.to_string()), weight))
        })
        .collect()
}

/// Dispatches to the matching `nrs_import::legacy` formula. Every
/// "0-1" parameter is clamped before the call: several formulas
/// (`nei`/`aei`/`epi` via `remap`, `osu_song`) assert their input is
/// in range and panic otherwise -- appropriate for import-time bad
/// data, not for a live form a fat-fingered percentage shouldn't be
/// able to crash a request over.
fn build_impact_atoms(
    kind: &str,
    source: SourceId,
    fields: &HashMap<String, String>,
) -> Result<Vec<Atom>, String> {
    let f = |k: &str| f32_field(fields, k);
    match kind {
        "cry" => Ok(nrs_import::cry(source, &blend_field(fields))),
        "ehi" => Ok(nrs_import::ehi_with_blend(source, &blend_field(fields))),
        "nei" => Ok(nrs_import::nei(
            source,
            f("factor").clamp(0.0, 1.0),
            f("sign"),
            &blend_field(fields),
        )),
        "aei" => Ok(nrs_import::aei(
            source,
            f("factor").clamp(0.0, 1.0),
            f("sign"),
            &blend_field(fields),
        )),
        "epi" => Ok(nrs_import::epi_with_blend(
            source,
            f("factor").clamp(0.0, 1.0),
            &blend_field(fields),
        )),
        "pads" => Ok(nrs_import::pads(
            source,
            f("days").max(0.0),
            &blend_field(fields),
        )),
        "jumpscare" => Ok(nrs_import::jumpscare(source)),
        "sleepless_night" => Ok(nrs_import::sleepless_night(source)),
        "politics" => Ok(nrs_import::politics(source)),
        "dropped" => Ok(nrs_import::dropped(source)),
        "interest_field" => Ok(nrs_import::interest_field(
            source,
            fields
                .get("new_field")
                .map(|s| s == "true")
                .unwrap_or(false),
        )),
        "additional" => Ok(nrs_import::additional(source, f("value"))),
        "music" => Ok(nrs_import::music(source, f("music_base").clamp(0.0, 1.0))),
        "waifu" => Ok(nrs_import::waifu(source, f("days").max(0.0))),
        "music_consumed_progress" => Ok(nrs_import::music_consumed_progress(
            source,
            f("duration_minutes").max(0.0),
        )),
        "meme" => Ok(nrs_import::meme(
            source,
            f("strength").clamp(0.0, 1.999),
            f("days").max(0.0),
        )),
        "osu_song" => Ok(nrs_import::osu_song(
            source,
            f("personal").clamp(0.0, 1.0),
            f("community").clamp(0.0, 1.0),
        )),
        "consumed" => Ok(nrs_import::consumed(
            source,
            f("boredom").clamp(0.0, 1.0),
            f("duration_minutes").max(0.0),
        )),
        "anime_consumed" => Ok(nrs_import::anime_consumed(
            source,
            f("boredom").clamp(0.0, 1.0),
            f("episodes").max(0.0),
            f("episode_duration_minutes").max(0.0),
        )),
        "visual" => Ok(nrs_import::visual(
            source,
            f("visual_kind").clamp(0.0, 1.0),
            f("base").clamp(0.0, 1.0),
            f("unique").clamp(0.0, 1.0),
        )),
        "writing" => Ok(nrs_import::writing(
            source,
            f("character_complexity").clamp(0.0, 1.0),
            f("story_quality").clamp(0.0, 1.0),
            f("pacing").clamp(0.0, 1.0),
            f("originality").clamp(0.0, 1.0),
        )),
        other => Err(format!("unknown impact kind: {other}")),
    }
}

/// The whole submitted form, stored verbatim -- `impact_form`/
/// `relation_form` read the same shape back as edit-mode prefill, and
/// `dah_params_summary` filters the bookkeeping keys out at display
/// time rather than here.
fn dah_params_json(fields: &HashMap<String, String>) -> String {
    serde_json::to_string(fields).unwrap_or_default()
}

/// A `<contributor factor="..">` share (INFRA.md's fan-out: one
/// `<impact>` tag with several contributors becomes one row per
/// contributor, each carrying the same kind/params but a different
/// slice of the formula's full-magnitude result) -- 1.0 for a plain
/// impact, clamped to `[0, 1]` since that's the only range a share
/// makes sense in.
fn dah_factor_field(fields: &HashMap<String, String>) -> f32 {
    f32_field_or(fields, "dah_factor", 1.0).clamp(0.0, 1.0)
}

fn build_relation(
    kind: &str,
    from: EntryId,
    to: EntryId,
    fields: &HashMap<String, String>,
) -> Result<nrs_core::Relation, String> {
    match kind {
        "contains" => Ok(nrs_import::contains_weighted(
            from,
            to,
            f32_field_or(fields, "factor", 1.0).clamp(0.0, 1.0),
        )),
        "remix" => Ok(nrs_import::remix(from, to)),
        "feature_music" => Ok(nrs_import::feature_music(from, to)),
        "killed_by" => Ok(nrs_import::killed_by(
            from,
            to,
            f32_field_or(fields, "potential", 0.5).clamp(0.0, 1.0),
            f32_field_or(fields, "effect", 0.5).clamp(0.0, 1.0),
        )),
        other => Err(format!("unknown relation kind: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrs_core::{Entry, Impact, ImpactId, Palette};

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
                anchors: vec![],
                k_feat: KFeatMatrix::default(),
            },
        }
    }

    #[test]
    fn rows_sort_by_value_descending_by_default() {
        let snap = tiny_snapshot();
        let r = rows(
            &snap,
            SortBy::Value,
            SortDir::Desc,
            KFeatMatrix::default(),
            TimeParams::default(),
        );
        assert_eq!(r[0].id.0, "a");
        assert_eq!(r[1].id.0, "b");
    }

    #[test]
    fn rows_sort_by_title_ascending() {
        let snap = tiny_snapshot();
        let r = rows(
            &snap,
            SortBy::Title,
            SortDir::Asc,
            KFeatMatrix::default(),
            TimeParams::default(),
        );
        assert_eq!(r[0].title, "Apple");
        assert_eq!(r[1].title, "Zebra");
    }

    #[test]
    fn scoring_produces_finite_values() {
        let snap = tiny_snapshot();
        let r = rows(
            &snap,
            SortBy::Value,
            SortDir::Desc,
            KFeatMatrix::default(),
            TimeParams::default(),
        );
        for row in &r {
            assert!(row.value.is_finite());
        }
    }

    #[test]
    fn calendar_period_recomputes_legacy_day_count() {
        let mut fields = HashMap::from([
            ("period_from".into(), "2025-01-01".into()),
            ("period_to".into(), "2025-02-01".into()),
            ("days".into(), "999".into()),
        ]);
        normalize_period_fields("meme", &mut fields).unwrap();
        assert_eq!(fields.get("days").map(String::as_str), Some("31"));
    }

    #[test]
    fn dynamic_today_period_keeps_expression_and_refreshes_resolution() {
        let mut fields = HashMap::from([
            ("period_from".into(), "2025-01-01".into()),
            ("period_to".into(), "2025-01-31".into()),
            ("period_to_expression".into(), "today".into()),
            ("days".into(), "30".into()),
        ]);
        normalize_period_fields_at("meme", &mut fields, time::macros::date!(2025 - 02 - 01))
            .unwrap();
        assert_eq!(
            fields.get("period_to").map(String::as_str),
            Some("2025-02-01")
        );
        assert_eq!(
            fields.get("period_to_expression").map(String::as_str),
            Some("today")
        );
        assert_eq!(fields.get("days").map(String::as_str), Some("31"));
    }

    #[test]
    fn dynamic_period_summary_shows_expression_and_resolved_date() {
        let fields = HashMap::from([
            ("period_from".into(), "2025-01-01".into()),
            ("period_to".into(), "2025-02-01".into()),
            ("period_to_expression".into(), "today".into()),
            ("days".into(), "31".into()),
        ]);
        let period = impact_period_summary(Some("meme"), &fields).unwrap();
        assert_eq!(period.from, "2025-01-01");
        assert_eq!(period.to, "today");
        assert!(period
            .caption
            .contains("resolved as 2025-01-01 → 2025-02-01"));
    }

    #[test]
    fn legacy_day_only_period_is_presented_as_relative() {
        let fields = HashMap::from([("days".into(), "528".into())]);
        let period = impact_period_summary(Some("waifu"), &fields).unwrap();
        assert_eq!(period.from, "start");
        assert_eq!(period.to, "day 528");
        assert!(period.caption.contains("dates not recorded"));
    }

    #[test]
    fn music_kinds_follow_legacy_ids_then_relations() {
        let mut hints = KindHints::default();
        hints.credited.insert("M-20221224T220452".into());
        hints.contains_music.insert("M-20220818T163913".into());
        let kind = |id| entry_kind(id, "none", &hints);
        assert_eq!(kind("M-VGMDB-AR-14655"), "artist");
        assert_eq!(kind("M-VGMDB-AL-85759"), "album");
        assert_eq!(kind("M-VGMDB-AL-85759-1"), "track");
        assert_eq!(kind("M-20221224T220452"), "artist");
        assert_eq!(kind("M-20220818T163913"), "album");
        assert_eq!(kind("M-20241202T141801"), "track");
        assert_eq!(kind("A-MAL-5114"), "anime");
        assert_eq!(kind("GF-VGMDB-7059"), "game");
        let ulid = "01M2600Y960YYWR278THQT8546";
        assert_eq!(entry_kind(ulid, "vgmdb-album", &hints), "album");
        assert_eq!(entry_kind(ulid, "anime", &hints), "anime");
    }

    #[test]
    fn duration_minutes_are_displayed_as_media_time() {
        assert_eq!(format_duration_minutes("3.6833334"), "3:41");
        assert_eq!(format_duration_minutes("65.7"), "1:05:42");
        assert_eq!(format_duration_minutes("180"), "3:00:00");
    }

    #[test]
    fn duration_input_accepts_media_time() {
        let mut submitted = HashMap::from([("duration_minutes".into(), "3:41".into())]);
        normalize_duration_fields("music_consumed_progress", &mut submitted).unwrap();
        let minutes = submitted["duration_minutes"].parse::<f32>().unwrap();
        assert!((minutes - (3.0 + 41.0 / 60.0)).abs() < 1e-5);
        assert!((parse_duration_minutes_input("1:05:42").unwrap() - 65.7).abs() < 1e-5);
    }
}
