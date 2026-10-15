//! What an entry looks like, for grading its visuals on the entry page:
//! the type stores' artwork for its `<bind url>` (an anime's poster),
//! and the YouTube videos and MusicBrainz releases among a music
//! record's sources (an MV, a release's cover from the Cover Art
//! Archive). A track's albums are listed but their covers aren't shown:
//! a track's visual is its MV, and album art is graded on the album.
//! Plus every visual grade already given to entries of one kind, to
//! grade against.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use nrs_core::{EntryId, RelKind, SourceId};
use nrs_entry_store::{EntryStore as _, HttpEntryStore};
use serde::{Deserialize, Serialize};

use super::{api_error, kind_hints, store_error, ApiResult};
use crate::{entry_display_name, entry_kind, AppState};

#[derive(Serialize, Default)]
pub(crate) struct MediaResponse {
    images: Vec<MediaImage>,
    videos: Vec<MediaVideo>,
    /// The albums containing the entry, where their art is graded.
    albums: Vec<super::EntrySummary>,
    /// Store lookups that failed; what was found is still returned.
    warnings: Vec<String>,
}

#[derive(Serialize, PartialEq)]
struct MediaImage {
    url: String,
    label: String,
}

#[derive(Serialize, PartialEq)]
struct MediaVideo {
    /// The YouTube video id.
    youtube: String,
    label: String,
}

/// The YouTube video id in a watch, short-link, shorts or embed URL.
fn youtube_id(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    let host = host.trim_start_matches("www.").trim_start_matches("m.");
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let mut segments = path.split('/');
    let id = match host {
        "youtu.be" => segments.next()?,
        "youtube.com" | "music.youtube.com" | "youtube-nocookie.com" => match segments.next()? {
            "watch" => query
                .split('&')
                .find_map(|pair| pair.strip_prefix("v="))?,
            "shorts" | "embed" | "live" => segments.next()?,
            _ => return None,
        },
        _ => return None,
    };
    let id = id.split('#').next()?;
    let valid = id.len() == 11
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    valid.then(|| id.to_string())
}

/// The Cover Art Archive front cover for a MusicBrainz release or
/// release group URL.
fn cover_art(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://musicbrainz.org/")
        .or_else(|| url.strip_prefix("http://musicbrainz.org/"))?;
    let (kind, mbid) = rest.split_once('/')?;
    let mbid = mbid.split(['/', '?', '#']).next()?;
    matches!(kind, "release" | "release-group")
        .then(|| format!("https://coverartarchive.org/{kind}/{mbid}/front-500"))
}

impl MediaResponse {
    fn image(&mut self, url: String, label: impl Into<String>) {
        if !self.images.iter().any(|image| image.url == url) {
            self.images.push(MediaImage {
                url,
                label: label.into(),
            });
        }
    }

    /// Whatever `url` itself shows: an MV, or a release's cover.
    fn from_url(&mut self, url: &str, label: &str) {
        if let Some(youtube) = youtube_id(url) {
            if !self.videos.iter().any(|video| video.youtube == youtube) {
                self.videos.push(MediaVideo {
                    youtube,
                    label: label.to_string(),
                });
            }
        } else if let Some(cover) = cover_art(url) {
            self.image(cover, label);
        }
    }

    /// The stores' record for `url`: its cover, and for a music record
    /// every source it was merged from.
    fn from_stores(&mut self, url: &str, label: &str, music: Option<&str>, anime: Option<&str>) {
        self.from_url(url, label);
        let result = if nrs_import::stores::is_anime_store_url(url) {
            anime.map(|base| self.from_anime_store(base, url, label))
        } else {
            music.map(|base| self.from_music_store(base, url, label))
        };
        if let Some(Err(warning)) = result {
            self.warnings.push(warning);
        }
    }

    fn from_anime_store(&mut self, base: &str, url: &str, label: &str) -> Result<(), String> {
        let err = |e: nrs_entry_store::Error| format!("anime store: {e}");
        let store = HttpEntryStore::new(base).map_err(err)?;
        // `ingest` is how the anime store maps a URL to its record; it's
        // a no-op for one already ingested.
        let meta = store.ingest(url).and_then(|id| store.resolve(&id)).map_err(err)?;
        if let Some(cover) = meta.cover_url {
            self.image(cover, label);
        }
        Ok(())
    }

    fn from_music_store(&mut self, base: &str, url: &str, label: &str) -> Result<(), String> {
        let err = |e: nrs_entry_store::Error| format!("music store: {e}");
        let store = HttpEntryStore::new(base)
            .map_err(err)?
            .expecting_store("musiclib");
        let Some(found) = store.lookup(url).map_err(err)? else {
            return Ok(());
        };
        let meta = store.resolve(&found.store_id).map_err(err)?;
        if let Some(cover) = meta.cover_url {
            self.image(cover, label);
        }
        for source in meta.extra["sources"].as_array().into_iter().flatten() {
            let Some(identifier) = source["identifier"].as_str() else {
                continue;
            };
            let title = source["title"].as_str().unwrap_or(label);
            self.from_url(identifier, title);
        }
        Ok(())
    }
}

/// The entry's artwork and videos, and the albums containing it.
pub(crate) async fn entry_media(
    State(state): State<Arc<AppState>>,
    Path(entry_id): Path<String>,
) -> ApiResult<MediaResponse> {
    let (target, albums) = {
        let store = state.store.as_ref().ok_or_else(|| {
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "entry API requires database mode",
            )
        })?;
        let store = store
            .lock()
            .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
        let entries = store
            .list_entries()
            .map_err(|error| store_error("load entries", error))?;
        let by_id: HashMap<&str, _> = entries.iter().map(|e| (e.id.0.as_str(), e)).collect();
        let entry = by_id
            .get(entry_id.as_str())
            .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "entry not found"))?;
        let hints = kind_hints(&state, &store)?;
        let albums: Vec<_> = store
            .relations_for_entry(&EntryId(entry_id.clone()))
            .map_err(|error| store_error("load relations", error))?
            .into_iter()
            .filter(|relation| relation.kind == RelKind::Contains && relation.to.0 == entry_id)
            .filter_map(|relation| by_id.get(relation.from.0.as_str()).copied())
            .filter(|parent| entry_kind(&parent.id.0, &parent.store, &hints) == "album")
            .map(|album| super::EntrySummary::new(album, &hints))
            .collect();
        let target = (entry.store == "url")
            .then(|| (entry.store_id.clone(), entry_display_name(entry).to_string()));
        (target, albums)
    };
    let (music, anime) = (state.music_store_url.clone(), state.anime_store_url.clone());
    let media = tokio::task::spawn_blocking(move || {
        let mut media = MediaResponse {
            albums,
            ..MediaResponse::default()
        };
        if let Some((url, label)) = &target {
            media.from_stores(url, label, music.as_deref(), anime.as_deref());
        }
        media
    })
    .await
    .map_err(|error| store_error("run store lookup", error))?;
    Ok(Json(media))
}

#[derive(Deserialize)]
pub(crate) struct VisualGradesQuery {
    kind: String,
}

#[derive(Serialize)]
pub(crate) struct VisualGrade {
    entry: String,
    title: String,
    impact: String,
    visual_kind: f32,
    base: f32,
    unique: f32,
    /// The visual impact's magnitude (`nrs_import::visual`).
    score: f32,
}

/// Every visual grade on entries of one kind, highest first.
pub(crate) async fn visual_grades(
    State(state): State<Arc<AppState>>,
    Query(query): Query<VisualGradesQuery>,
) -> ApiResult<Vec<VisualGrade>> {
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "entry API requires database mode",
        )
    })?;
    let store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    let hints = kind_hints(&state, &store)?;
    let entries = store
        .list_entries()
        .map_err(|error| store_error("load entries", error))?;
    let by_id: HashMap<&str, _> = entries.iter().map(|e| (e.id.0.as_str(), e)).collect();
    let impacts = store
        .visual_impacts()
        .map_err(|error| store_error("load visual impacts", error))?;
    drop(store);

    let number = |params: &serde_json::Value, key: &str| -> Option<f32> {
        match &params[key] {
            serde_json::Value::String(value) => value.parse().ok(),
            value => value.as_f64().map(|value| value as f32),
        }
    };
    let mut grades: Vec<VisualGrade> = impacts
        .into_iter()
        .filter_map(|(entry_id, impact, params)| {
            let entry = by_id.get(entry_id.as_str())?;
            if entry_kind(&entry.id.0, &entry.store, &hints) != query.kind {
                return None;
            }
            let params: serde_json::Value = serde_json::from_str(&params?).ok()?;
            let (visual_kind, base, unique) = (
                number(&params, "visual_kind")?,
                number(&params, "base")?,
                number(&params, "unique")?,
            );
            let score = nrs_import::visual(SourceId(entry_id.clone()), visual_kind, base, unique)
                .iter()
                .map(|atom| atom.magnitude)
                .sum();
            Some(VisualGrade {
                entry: entry_id,
                title: entry_display_name(entry).to_string(),
                impact,
                visual_kind,
                base,
                unique,
                score,
            })
        })
        .collect();
    grades.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(Json(grades))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn youtube_ids_come_from_every_url_shape() {
        for url in [
            "https://youtu.be/dXUJ0IDdX28",
            "https://www.youtube.com/watch?v=dXUJ0IDdX28&list=x",
            "https://youtube.com/shorts/dXUJ0IDdX28",
            "https://music.youtube.com/watch?v=dXUJ0IDdX28",
        ] {
            assert_eq!(youtube_id(url).as_deref(), Some("dXUJ0IDdX28"), "{url}");
        }
        assert_eq!(youtube_id("https://www.youtube.com/channel/UC123"), None);
        assert_eq!(youtube_id("https://vgmdb.net/album/1"), None);
    }

    #[test]
    fn releases_get_cover_art_and_recordings_do_not() {
        assert_eq!(
            cover_art("https://musicbrainz.org/release/e2f16906-826f-4b49-bd64-807a581e099d").as_deref(),
            Some("https://coverartarchive.org/release/e2f16906-826f-4b49-bd64-807a581e099d/front-500")
        );
        assert_eq!(
            cover_art("https://musicbrainz.org/recording/abcaa850-feb8-4aa2-b6d9-9673d01af62a"),
            None
        );
    }
}
