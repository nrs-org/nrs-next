//! AniList (GraphQL): the viewer's list, entries by AniList or MAL id,
//! and list saves. Its tokens come from the implicit-grant login on the
//! `/lists` page; they last a year and can't be refreshed.
//!
//! A rewatch under way is `REPEATING`. `http` paces requests by the
//! rate-limit headers AniList sends.

use std::collections::HashMap;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use time::{Date, Month};

use crate::lists::http::Paced;
use crate::lists::plan::{Found, Listed, Site};
use crate::lists::tokens::Token;
use crate::lists::{Error, ListEntry, ListStatus, Medium, Patch, Result, SiteRef, Totals};

const API: &str = "https://graphql.anilist.co";
const PER_PAGE: usize = 50;

pub struct AniList {
    http: Paced,
    bearer: String,
    viewer: Option<u64>,
}

#[derive(Deserialize)]
struct Response<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FuzzyDate {
    year: Option<i32>,
    month: Option<u8>,
    day: Option<u8>,
}

impl FuzzyDate {
    /// Only whole dates count.
    fn date(&self) -> Option<Date> {
        let month = Month::try_from(self.month?).ok()?;
        Date::from_calendar_date(self.year?, month, self.day?).ok()
    }
}

fn fuzzy(date: Date) -> Value {
    json!({ "year": date.year(), "month": u8::from(date.month()), "day": date.day() })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Media {
    id: u64,
    id_mal: Option<u64>,
    episodes: Option<u32>,
    chapters: Option<u32>,
    volumes: Option<u32>,
    #[serde(default)]
    title: Title,
}

#[derive(Deserialize, Default)]
struct Title {
    romaji: Option<String>,
}

impl Media {
    fn totals(&self, medium: Medium) -> Totals {
        match medium {
            Medium::Anime => Totals {
                units: self.episodes,
                volumes: None,
            },
            Medium::Manga => Totals {
                units: self.chapters,
                volumes: self.volumes,
            },
        }
    }

    fn refs(&self, medium: Medium) -> Vec<SiteRef> {
        let mut refs = vec![SiteRef::AniList(medium, self.id)];
        refs.extend(self.id_mal.map(|id| SiteRef::Mal(medium, id)));
        refs
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaList {
    status: String,
    #[serde(default)]
    progress: u32,
    #[serde(default)]
    progress_volumes: Option<u32>,
    #[serde(default)]
    repeat: u32,
    /// `POINT_100`; 0 is unscored.
    #[serde(default)]
    score: f32,
    #[serde(default)]
    started_at: FuzzyDate,
    #[serde(default)]
    completed_at: FuzzyDate,
    media: Media,
}

fn list_entry(medium: Medium, list: &MediaList) -> ListEntry {
    let (status, rewatching) = match list.status.as_str() {
        "CURRENT" => (ListStatus::Current, false),
        "REPEATING" => (ListStatus::Current, true),
        "COMPLETED" => (ListStatus::Completed, false),
        "PAUSED" => (ListStatus::Paused, false),
        "DROPPED" => (ListStatus::Dropped, false),
        _ => (ListStatus::Planned, false),
    };
    ListEntry {
        status,
        rewatching,
        progress: Some(list.progress),
        volumes: (medium == Medium::Manga).then(|| list.progress_volumes.unwrap_or(0)),
        repeats: list.repeat,
        started: list.started_at.date(),
        finished: list.completed_at.date(),
        score: Some(list.score.round() as u32).filter(|&score| score > 0),
    }
}

fn media_type(medium: Medium) -> &'static str {
    match medium {
        Medium::Anime => "ANIME",
        Medium::Manga => "MANGA",
    }
}

/// `SaveMediaListEntry` with only `patch`'s fields as arguments, so the
/// rest stay as they are.
fn mutation(id: u64, patch: &Patch) -> (String, Value) {
    let mut args: Vec<(&str, &str, Value)> = vec![("mediaId", "Int", json!(id))];
    if let Some((status, rewatching)) = patch.status {
        let status = match status {
            ListStatus::Current if rewatching => "REPEATING",
            ListStatus::Current => "CURRENT",
            ListStatus::Planned => "PLANNING",
            ListStatus::Paused => "PAUSED",
            ListStatus::Completed => "COMPLETED",
            ListStatus::Dropped => "DROPPED",
        };
        args.push(("status", "MediaListStatus", json!(status)));
    }
    if let Some(progress) = patch.progress {
        args.push(("progress", "Int", json!(progress)));
    }
    if let Some(volumes) = patch.volumes {
        args.push(("progressVolumes", "Int", json!(volumes)));
    }
    if let Some(repeats) = patch.repeats {
        args.push(("repeat", "Int", json!(repeats)));
    }
    if let Some(started) = patch.started {
        args.push(("startedAt", "FuzzyDateInput", fuzzy(started)));
    }
    if let Some(finished) = patch.finished {
        args.push(("completedAt", "FuzzyDateInput", fuzzy(finished)));
    }
    // `scoreRaw` is always out of 100, whatever the user's display format.
    if let Some(score) = patch.score {
        args.push(("scoreRaw", "Int", json!(score)));
    }
    let declared: Vec<String> = args.iter().map(|(name, ty, _)| format!("${name}: {ty}")).collect();
    let passed: Vec<String> = args.iter().map(|(name, _, _)| format!("{name}: ${name}")).collect();
    let query = format!(
        "mutation ({}) {{ SaveMediaListEntry({}) {{ id }} }}",
        declared.join(", "),
        passed.join(", ")
    );
    let variables = args.into_iter().map(|(name, _, value)| (name.to_string(), value)).collect();
    (query, Value::Object(variables))
}

impl AniList {
    pub fn new(token: &Token) -> Self {
        Self {
            http: Paced::new("AniList"),
            bearer: format!("Bearer {}", token.access_token),
            viewer: None,
        }
    }

    fn query<T: DeserializeOwned>(&mut self, query: &str, variables: Value) -> Result<T> {
        let bearer = self.bearer.clone();
        let body = json!({ "query": query, "variables": variables });
        let response: Response<T> = self.http.json(|agent| {
            agent
                .post(API)
                .set("Authorization", &bearer)
                .set("Accept", "application/json")
                .send_json(body.clone())
        })?;
        match (response.data, response.errors.is_empty()) {
            (Some(data), true) => Ok(data),
            (_, _) => Err(Error::Protocol {
                site: "AniList",
                message: Value::Array(response.errors).to_string(),
            }),
        }
    }

    /// The token's user: id and name.
    pub fn viewer(&mut self) -> Result<(u64, String)> {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Data {
            viewer: Viewer,
        }
        #[derive(Deserialize)]
        struct Viewer {
            id: u64,
            name: String,
        }
        let data: Data = self.query("query { Viewer { id name } }", json!({}))?;
        self.viewer = Some(data.viewer.id);
        Ok((data.viewer.id, data.viewer.name))
    }
}

const LIST: &str = "query ($userId: Int, $type: MediaType, $chunk: Int) {
  MediaListCollection(userId: $userId, type: $type, chunk: $chunk, perChunk: 500) {
    hasNextChunk
    lists { entries {
      status progress progressVolumes repeat score(format: POINT_100)
      startedAt { year month day } completedAt { year month day }
      media { id idMal episodes chapters volumes title { romaji } }
    } }
  }
}";

impl Site for AniList {
    fn name(&self) -> &'static str {
        "AniList"
    }

    fn points(&self) -> u32 {
        100
    }

    fn list(&mut self, medium: Medium) -> Result<Vec<Listed>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Data {
            media_list_collection: Collection,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Collection {
            has_next_chunk: Option<bool>,
            lists: Vec<Group>,
        }
        #[derive(Deserialize)]
        struct Group {
            entries: Vec<MediaList>,
        }
        let user = match self.viewer {
            Some(user) => user,
            None => self.viewer()?.0,
        };
        let mut listed: Vec<Listed> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for chunk in 1.. {
            let variables = json!({ "userId": user, "type": media_type(medium), "chunk": chunk });
            let data: Data = self.query(LIST, variables)?;
            let collection = data.media_list_collection;
            // An entry in custom lists shows up once per list.
            for list in collection.lists.into_iter().flat_map(|group| group.entries) {
                if seen.insert(list.media.id) {
                    listed.push(Listed {
                        id: list.media.id,
                        refs: list.media.refs(medium),
                        title: list.media.title.romaji.clone().unwrap_or_default(),
                        entry: list_entry(medium, &list),
                        totals: list.media.totals(medium),
                    });
                }
            }
            if collection.has_next_chunk != Some(true) {
                break;
            }
        }
        Ok(listed)
    }

    fn find(&mut self, refs: &[SiteRef]) -> Result<HashMap<SiteRef, Found>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Data {
            page: Page,
        }
        #[derive(Deserialize)]
        struct Page {
            media: Vec<Media>,
        }
        let mut found = HashMap::new();
        let Some(medium) = refs.first().map(|site_ref| site_ref.medium()) else {
            return Ok(found);
        };
        let ids = |mal: bool| -> Vec<u64> {
            refs.iter()
                .filter_map(|&site_ref| match site_ref {
                    SiteRef::Mal(_, id) if mal => Some(id),
                    SiteRef::AniList(_, id) if !mal => Some(id),
                    _ => None,
                })
                .collect()
        };
        for (filter, ids) in [("idMal_in", ids(true)), ("id_in", ids(false))] {
            for chunk in ids.chunks(PER_PAGE) {
                let query = format!(
                    "query ($ids: [Int], $type: MediaType) {{ Page(perPage: {PER_PAGE}) {{ \
                     media({filter}: $ids, type: $type) {{ id idMal episodes chapters volumes }} }} }}"
                );
                let variables = json!({ "ids": chunk, "type": media_type(medium) });
                let data: Data = self.query(&query, variables)?;
                for media in data.page.media {
                    let entry = Found {
                        id: media.id,
                        totals: media.totals(medium),
                    };
                    for site_ref in media.refs(medium) {
                        found.entry(site_ref).or_insert(entry);
                    }
                }
            }
        }
        found.retain(|site_ref, _| refs.contains(site_ref));
        Ok(found)
    }

    fn save(&mut self, _: Medium, id: u64, patch: &Patch) -> Result<()> {
        #[derive(Deserialize)]
        #[allow(dead_code)]
        #[serde(rename_all = "PascalCase")]
        struct Data {
            save_media_list_entry: Value,
        }
        let (query, variables) = mutation(id, patch);
        self.query::<Data>(&query, variables)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use time::macros::date;

    use super::*;

    #[test]
    fn list_entries_read_repeating_as_rewatching() {
        let list: MediaList = serde_json::from_str(
            r#"{"status":"REPEATING","progress":2,"score":75.0,"progressVolumes":null,"repeat":1,
                "startedAt":{"year":2026,"month":1,"day":5},"completedAt":{"year":2026,"month":null,"day":null},
                "media":{"id":21,"idMal":21,"episodes":null,"chapters":null,"volumes":null,"title":{"romaji":"One Piece"}}}"#,
        )
        .unwrap();
        let entry = list_entry(Medium::Anime, &list);
        assert_eq!((entry.status, entry.rewatching, entry.repeats), (ListStatus::Current, true, 1));
        assert_eq!((entry.started, entry.finished), (Some(date!(2026 - 01 - 05)), None));
        assert_eq!(entry.score, Some(75));
        assert_eq!(
            list.media.refs(Medium::Anime),
            [SiteRef::AniList(Medium::Anime, 21), SiteRef::Mal(Medium::Anime, 21)]
        );
    }

    #[test]
    fn mutations_name_only_patched_fields() {
        let patch = Patch {
            status: Some((ListStatus::Completed, false)),
            finished: Some(date!(2026 - 03 - 30)),
            ..Patch::default()
        };
        let (query, variables) = mutation(9, &patch);
        assert_eq!(
            query,
            "mutation ($mediaId: Int, $status: MediaListStatus, $completedAt: FuzzyDateInput) \
             { SaveMediaListEntry(mediaId: $mediaId, status: $status, completedAt: $completedAt) { id } }"
        );
        assert_eq!(
            variables,
            json!({ "mediaId": 9, "status": "COMPLETED",
                    "completedAt": { "year": 2026, "month": 3, "day": 30 } })
        );
    }
}
