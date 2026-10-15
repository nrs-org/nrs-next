//! MyAnimeList and AniList from the browser (`crate::lists`): logging
//! in, previewing a sync, pushing one entry, and running a full sync
//! (`POST /lists/sync`, which a scheduler can call too). Progress writes
//! push themselves (`progress::write` -> `lists::live`).
//!
//! MAL: `mal_connect` starts a PKCE login and returns MAL's authorize
//! URL; MAL sends the browser back to `mal_callback`, which trades the
//! code for a token. AniList's implicit grant puts the token in the
//! fragment of the `/lists` page, which posts it to `anilist_token`. Both
//! redirect URLs come from the request's `Host`, and `lists_status` shows
//! them for registering the apps.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Redirect;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::{api_error, ApiError, ApiResult};
use crate::lists::anilist::AniList;
use crate::lists::job::{self, JobState};
use crate::lists::live;
use crate::lists::mal::{self, Mal};
use crate::lists::plan::{self, Row};
use crate::lists::tokens::{self, Token, Tokens};
use crate::lists::{self as sites, network, parse_ref, random_hex, Describe};
use crate::{AppState, TrackedStore};

type ApiErr = (StatusCode, Json<ApiError>);

const PAGE: &str = "/lists";
const MAL_CALLBACK: &str = "/api/v1/lists/mal/callback";
/// How long a MAL login may sit at MAL's consent page.
const LOGIN_SECONDS: i64 = 3600;

struct Pending {
    verifier: String,
    client: mal::Client,
    started: i64,
}

/// MAL logins under way, by `state`.
static PENDING: Mutex<BTreeMap<String, Pending>> = Mutex::new(BTreeMap::new());

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn origin(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
        .unwrap_or("127.0.0.1:3737");
    format!("http://{host}")
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn sync_error(error: crate::lists::Error) -> ApiErr {
    let status = match error {
        crate::lists::Error::Auth(_) => StatusCode::UNAUTHORIZED,
        _ => StatusCode::BAD_GATEWAY,
    };
    api_error(status, error.to_string())
}

fn internal(error: impl std::fmt::Display) -> ApiErr {
    api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

fn load_tokens() -> Result<Tokens, ApiErr> {
    Tokens::load(&tokens::default_path()).map_err(sync_error)
}

fn save_token(set: impl FnOnce(&mut Tokens)) -> Result<(), ApiErr> {
    let path = tokens::default_path();
    let mut tokens = Tokens::load(&path).map_err(sync_error)?;
    set(&mut tokens);
    tokens.save(&path).map_err(sync_error)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ApiErr> + Send + 'static,
) -> Result<T, ApiErr> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| internal("the request task failed"))?
}

fn lock_store(state: &AppState) -> Result<MutexGuard<'_, TrackedStore>, ApiErr> {
    state
        .store
        .as_ref()
        .ok_or_else(|| {
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "list sync requires database mode",
            )
        })?
        .lock()
        .map_err(|_| internal("store lock poisoned"))
}

fn rows(state: &AppState) -> Result<Vec<Row>, ApiErr> {
    let store = lock_store(state)?;
    plan::rows(&store).map_err(|error| internal(format!("failed to score progress: {error}")))
}

#[derive(Serialize)]
pub(crate) struct SiteStatus {
    connected: bool,
    user: Option<String>,
    days_left: Option<i64>,
    client_id: Option<String>,
    has_secret: bool,
    /// What to register as the app's redirect URL.
    redirect_url: String,
    /// Unix seconds, while a 429 holds the site back.
    rate_limited_until: Option<i64>,
}

#[derive(Serialize)]
pub(crate) struct ListsStatus {
    tokens_path: String,
    mal: SiteStatus,
    anilist: SiteStatus,
    /// Recent live pushes, newest first.
    live: Vec<live::Pushed>,
    job: JobState,
}

fn site_status(
    site: &str,
    token: Option<&Token>,
    env_client: &str,
    redirect_url: String,
) -> SiteStatus {
    let now = now();
    SiteStatus {
        connected: token.is_some_and(|token| token.days_left(now) >= 0),
        user: token.and_then(|token| token.user.clone()),
        days_left: token.map(|token| token.days_left(now)),
        client_id: token
            .and_then(|token| token.client_id.clone())
            .or_else(|| env(env_client)),
        has_secret: token.is_some_and(|token| token.client_secret.is_some())
            || env("MAL_CLIENT_SECRET").is_some() && env_client == "MAL_CLIENT_ID",
        redirect_url,
        rate_limited_until: sites::rate_limited_until(site),
    }
}

pub(crate) async fn lists_status(headers: HeaderMap) -> ApiResult<ListsStatus> {
    let tokens = load_tokens()?;
    let origin = origin(&headers);
    Ok(Json(ListsStatus {
        tokens_path: tokens::default_path().display().to_string(),
        mal: site_status(
            "MAL",
            tokens.mal.as_ref(),
            "MAL_CLIENT_ID",
            format!("{origin}{MAL_CALLBACK}"),
        ),
        anilist: site_status(
            "AniList",
            tokens.anilist.as_ref(),
            "ANILIST_CLIENT_ID",
            format!("{origin}{PAGE}"),
        ),
        live: live::recent(),
        job: job::state(),
    }))
}

#[derive(Deserialize)]
pub(crate) struct MalConnect {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct Authorize {
    url: String,
}

/// A blank secret keeps the saved (or `MAL_CLIENT_SECRET`) one for the
/// same client.
pub(crate) async fn mal_connect(
    headers: HeaderMap,
    Json(body): Json<MalConnect>,
) -> ApiResult<Authorize> {
    let id = body.client_id.trim().to_string();
    if id.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "a MAL client ID is needed"));
    }
    let saved = load_tokens()?
        .mal
        .filter(|token| token.client_id.as_deref() == Some(id.as_str()))
        .and_then(|token| token.client_secret);
    let secret = body
        .client_secret
        .map(|secret| secret.trim().to_string())
        .filter(|secret| !secret.is_empty())
        .or(saved)
        .or_else(|| env("MAL_CLIENT_SECRET"));
    let client = mal::Client {
        id,
        secret,
        redirect_uri: Some(format!("{}{MAL_CALLBACK}", origin(&headers))),
    };
    let verifier = random_hex(48).map_err(internal)?;
    let state = random_hex(8).map_err(internal)?;
    let url = mal::authorize_url(&client, &verifier, &state);
    let mut pending = PENDING.lock().map_err(|_| internal("login lock poisoned"))?;
    let now = now();
    pending.retain(|_, login| now - login.started < LOGIN_SECONDS);
    pending.insert(
        state,
        Pending {
            verifier,
            client,
            started: now,
        },
    );
    Ok(Json(Authorize { url }))
}

#[derive(Deserialize)]
pub(crate) struct MalCallback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    message: Option<String>,
}

fn back_to_page(result: Result<String, String>) -> Redirect {
    let query = match result {
        Ok(site) => format!("connected={site}"),
        Err(error) => url::form_urlencoded::Serializer::new(String::new())
            .append_pair("error", &error)
            .finish(),
    };
    Redirect::to(&format!("{PAGE}?{query}"))
}

pub(crate) async fn mal_callback(Query(query): Query<MalCallback>) -> Redirect {
    if let Some(error) = query.error {
        let message = query.message.unwrap_or_default();
        return back_to_page(Err(format!("MAL said {error}: {message}")));
    }
    let login = query.state.and_then(|state| PENDING.lock().ok()?.remove(&state));
    let (Some(login), Some(code)) = (login, query.code) else {
        return back_to_page(Err("this MAL login expired or was already used".into()));
    };
    let result = blocking(move || {
        let mut token =
            mal::exchange(&login.client, &code, &login.verifier, now()).map_err(sync_error)?;
        token.user = Some(Mal::new(&token).user_name().map_err(sync_error)?);
        save_token(|tokens| tokens.mal = Some(token))
    })
    .await;
    back_to_page(
        result
            .map(|()| "mal".to_string())
            .map_err(|(_, Json(error))| format!("MAL login failed: {}", error.error)),
    )
}

#[derive(Deserialize)]
pub(crate) struct AniListToken {
    access_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    client_id: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct Connected {
    user: String,
}

pub(crate) async fn anilist_token(Json(body): Json<AniListToken>) -> ApiResult<Connected> {
    let mut token = Token {
        access_token: body.access_token.trim().to_string(),
        refresh_token: None,
        expires_at: now() + body.expires_in.unwrap_or(365 * 86_400),
        client_id: body.client_id.filter(|id| !id.trim().is_empty()),
        client_secret: None,
        user: None,
    };
    if token.access_token.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "no AniList token"));
    }
    let user = blocking(move || {
        let (_, user) = AniList::new(&token).viewer().map_err(sync_error)?;
        token.user = Some(user.clone());
        save_token(|tokens| tokens.anilist = Some(token))?;
        Ok(user)
    })
    .await?;
    Ok(Json(Connected { user }))
}

#[derive(Serialize)]
pub(crate) struct Disconnected {
    site: String,
}

/// Forgets the saved token (the site's own grant stays until revoked
/// there).
pub(crate) async fn lists_disconnect(Path(site): Path<String>) -> ApiResult<Disconnected> {
    save_token(|tokens| match site.as_str() {
        "mal" => tokens.mal = None,
        _ => tokens.anilist = None,
    })?;
    Ok(Json(Disconnected { site }))
}

fn open_site(site: &str) -> Result<(Box<dyn sites::Site + Send>, Option<String>), ApiErr> {
    if !sites::SITES.contains(&site) {
        return Err(api_error(StatusCode::NOT_FOUND, format!("no list site {site:?}")));
    }
    sites::connect(site).map_err(sync_error)
}

#[derive(Serialize)]
pub(crate) struct ChangeView {
    entry: String,
    /// What the row is bound by; `lists_push` takes it.
    url: String,
    title: String,
    action: &'static str,
    description: String,
}

#[derive(Serialize)]
pub(crate) struct PlanView {
    site: &'static str,
    note: Option<String>,
    changes: Vec<ChangeView>,
    in_sync: usize,
    unknown: Vec<String>,
    duplicates: Vec<String>,
    untracked: Vec<String>,
    /// Progress rows bound by no MAL/AniList URL.
    unbound: usize,
}

/// What a sync would save: a dry run.
pub(crate) async fn lists_plan(
    State(state): State<Arc<AppState>>,
    Path(site): Path<String>,
) -> ApiResult<PlanView> {
    let rows = rows(&state)?;
    let view = blocking(move || {
        let (mut client, note) = open_site(&site)?;
        let plan = plan::plan(client.as_mut(), &rows, true).map_err(sync_error)?;
        let changes = plan
            .changes
            .iter()
            .map(|change| ChangeView {
                entry: change.entry.0.clone(),
                url: change.site_ref.url(),
                title: change.title.clone(),
                action: if change.have.is_some() { "update" } else { "add" },
                description: Describe {
                    medium: change.medium,
                    patch: &change.patch,
                    have: change.have.as_ref(),
                }
                .to_string(),
            })
            .collect();
        Ok(PlanView {
            site: client.name(),
            note,
            changes,
            in_sync: plan.in_sync,
            unknown: plan.unknown,
            duplicates: plan.duplicates,
            untracked: plan.untracked,
            unbound: rows.iter().filter(|row| row.site_ref.is_none()).count(),
        })
    })
    .await?;
    Ok(Json(view))
}

#[derive(Deserialize)]
pub(crate) struct Push {
    url: String,
}

#[derive(Serialize)]
pub(crate) struct Pushed {
    /// What was saved; `None` when the entry was already in sync.
    saved: Option<String>,
}

/// Saves one row's entry: the single-entry test before a full sync.
pub(crate) async fn lists_push(
    State(state): State<Arc<AppState>>,
    Path(site): Path<String>,
    Json(body): Json<Push>,
) -> ApiResult<Pushed> {
    let site_ref = parse_ref(&body.url)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "not a MAL/AniList entry URL"))?;
    let mut rows = rows(&state)?;
    rows.retain(|row| row.site_ref == Some(site_ref));
    if rows.is_empty() {
        return Err(api_error(StatusCode::NOT_FOUND, "no progress row is bound by that URL"));
    }
    let saved = blocking(move || {
        let (mut client, _) = open_site(&site)?;
        let _network = network(&site);
        let plan = plan::plan(client.as_mut(), &rows, false).map_err(sync_error)?;
        if !plan.unknown.is_empty() {
            let message = format!("{} has no entry for {}", client.name(), body.url);
            return Err(api_error(StatusCode::NOT_FOUND, message));
        }
        let Some(change) = plan.changes.first() else {
            return Ok(None);
        };
        client
            .save(change.medium, change.id, &change.patch)
            .map_err(sync_error)?;
        let describe = Describe {
            medium: change.medium,
            patch: &change.patch,
            have: change.have.as_ref(),
        };
        Ok(Some(describe.to_string()))
    })
    .await?;
    Ok(Json(Pushed { saved }))
}

#[derive(Deserialize)]
pub(crate) struct SyncQuery {
    /// `mal` or `anilist`; every connected site when absent.
    site: Option<String>,
    /// Answer once the sync is done (for a scheduler's log).
    #[serde(default)]
    wait: bool,
}

/// Starts a full sync, progress and scores; 409 while one runs.
pub(crate) async fn lists_sync_start(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SyncQuery>,
) -> ApiResult<JobState> {
    let connected = sites::connected();
    let chosen: Vec<&'static str> = match query.site.as_deref() {
        None | Some("") => connected,
        Some(site) => connected.into_iter().filter(|&known| known == site).collect(),
    };
    if chosen.is_empty() {
        return Err(api_error(StatusCode::UNAUTHORIZED, "no connected list site to sync"));
    }
    let state = Arc::clone(&state);
    let rows = blocking(move || rows(&state)).await?;
    if !job::start(rows, chosen) {
        return Err(api_error(StatusCode::CONFLICT, "a list sync is already running"));
    }
    let report = match query.wait {
        true => blocking(|| Ok(job::wait())).await?,
        false => job::state(),
    };
    Ok(Json(report))
}

pub(crate) async fn lists_sync_state() -> Json<JobState> {
    Json(job::state())
}
