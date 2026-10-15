//! MyAnimeList (API v2): the user's list, entries by id, list saves, and
//! the OAuth login (authorization code with a `plain` PKCE challenge, the
//! only kind MAL takes) that issues the token.
//!
//! A rewatch under way is `completed` with `is_rewatching` set (MAL only
//! offers the flag on completed entries); either flag reads back as
//! `Current` + rewatching. MAL reads counts as `num_episodes_watched` but
//! takes them as `num_watched_episodes`.

use std::collections::HashMap;

use serde::Deserialize;
use time::Date;

use crate::lists::http::Paced;
use crate::lists::plan::{Found, Listed, Site};
use crate::lists::tokens::Token;
use crate::lists::{Error, ListEntry, ListStatus, Medium, Patch, Result, SiteRef, Totals};

const API: &str = "https://api.myanimelist.net/v2";
const OAUTH: &str = "https://myanimelist.net/v1/oauth2";

pub struct Mal {
    http: Paced,
    bearer: String,
}

#[derive(Deserialize)]
struct Page<T> {
    data: Vec<T>,
    #[serde(default)]
    paging: Paging,
}

#[derive(Deserialize, Default)]
struct Paging {
    next: Option<String>,
}

#[derive(Deserialize)]
struct Item {
    node: Node,
    list_status: Status,
}

#[derive(Deserialize)]
struct Node {
    id: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    num_episodes: u32,
    #[serde(default)]
    num_chapters: u32,
    #[serde(default)]
    num_volumes: u32,
}

impl Node {
    /// MAL says 0 for unknown.
    fn totals(&self, medium: Medium) -> Totals {
        let known = |count: u32| Some(count).filter(|&count| count > 0);
        match medium {
            Medium::Anime => Totals {
                units: known(self.num_episodes),
                volumes: None,
            },
            Medium::Manga => Totals {
                units: known(self.num_chapters),
                volumes: known(self.num_volumes),
            },
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Status {
    status: String,
    score: u32,
    is_rewatching: bool,
    is_rereading: bool,
    num_episodes_watched: u32,
    num_chapters_read: u32,
    num_volumes_read: u32,
    num_times_rewatched: u32,
    num_times_reread: u32,
    start_date: Option<String>,
    finish_date: Option<String>,
}

/// MAL dates can be partial (`2020-05`); only whole ones count.
fn parse_date(value: Option<&str>) -> Option<Date> {
    let format = time::format_description::well_known::Iso8601::DATE;
    Date::parse(value?, &format).ok()
}

fn list_entry(medium: Medium, status: &Status) -> ListEntry {
    let rewatching = status.is_rewatching || status.is_rereading;
    let list_status = match status.status.as_str() {
        _ if rewatching => ListStatus::Current,
        "watching" | "reading" => ListStatus::Current,
        "completed" => ListStatus::Completed,
        "on_hold" => ListStatus::Paused,
        "dropped" => ListStatus::Dropped,
        _ => ListStatus::Planned,
    };
    let (progress, volumes) = match medium {
        Medium::Anime => (status.num_episodes_watched, None),
        Medium::Manga => (status.num_chapters_read, Some(status.num_volumes_read)),
    };
    ListEntry {
        status: list_status,
        rewatching,
        progress: Some(progress),
        volumes,
        repeats: status.num_times_rewatched + status.num_times_reread,
        started: parse_date(status.start_date.as_deref()),
        finished: parse_date(status.finish_date.as_deref()),
        score: Some(status.score).filter(|&score| score > 0),
    }
}

/// `patch` as `my_list_status` form fields.
fn form(medium: Medium, patch: &Patch) -> Vec<(&'static str, String)> {
    let anime = medium == Medium::Anime;
    let mut form = Vec::new();
    if let Some((status, rewatching)) = patch.status {
        let status = match (status, anime) {
            _ if rewatching => "completed",
            (ListStatus::Current, true) => "watching",
            (ListStatus::Current, false) => "reading",
            (ListStatus::Planned, true) => "plan_to_watch",
            (ListStatus::Planned, false) => "plan_to_read",
            (ListStatus::Paused, _) => "on_hold",
            (ListStatus::Completed, _) => "completed",
            (ListStatus::Dropped, _) => "dropped",
        };
        form.push(("status", status.to_string()));
        let flag = if anime { "is_rewatching" } else { "is_rereading" };
        form.push((flag, rewatching.to_string()));
    }
    if let Some(progress) = patch.progress {
        let field = if anime { "num_watched_episodes" } else { "num_chapters_read" };
        form.push((field, progress.to_string()));
    }
    if let Some(volumes) = patch.volumes {
        form.push(("num_volumes_read", volumes.to_string()));
    }
    if let Some(repeats) = patch.repeats {
        let field = if anime { "num_times_rewatched" } else { "num_times_reread" };
        form.push((field, repeats.to_string()));
    }
    if let Some(started) = patch.started {
        form.push(("start_date", started.to_string()));
    }
    if let Some(finished) = patch.finished {
        form.push(("finish_date", finished.to_string()));
    }
    if let Some(score) = patch.score {
        form.push(("score", score.to_string()));
    }
    form
}

/// Plain `list_status` leaves out the repeat count; naming it keeps the
/// rest.
fn list_fields(medium: Medium) -> &'static str {
    match medium {
        Medium::Anime => "list_status{num_times_rewatched},num_episodes",
        Medium::Manga => "list_status{num_times_reread},num_chapters,num_volumes",
    }
}

fn total_fields(medium: Medium) -> &'static str {
    match medium {
        Medium::Anime => "num_episodes",
        Medium::Manga => "num_chapters,num_volumes",
    }
}

impl Mal {
    pub fn new(token: &Token) -> Self {
        Self {
            http: Paced::new("MAL"),
            bearer: format!("Bearer {}", token.access_token),
        }
    }

    pub fn user_name(&mut self) -> Result<String> {
        #[derive(Deserialize)]
        struct User {
            name: String,
        }
        let bearer = self.bearer.clone();
        let user: User = self.http.json(|agent| {
            agent
                .get(&format!("{API}/users/@me"))
                .set("Authorization", &bearer)
                .call()
        })?;
        Ok(user.name)
    }
}

impl Site for Mal {
    fn name(&self) -> &'static str {
        "MAL"
    }

    fn points(&self) -> u32 {
        10
    }

    fn list(&mut self, medium: Medium) -> Result<Vec<Listed>> {
        let first = url::Url::parse_with_params(
            &format!("{API}/users/@me/{}list", medium.as_str()),
            [
                ("fields", list_fields(medium)),
                ("limit", "1000"),
                ("nsfw", "true"),
            ],
        )
        .expect("static URL");
        let bearer = self.bearer.clone();
        let mut next = Some(first.to_string());
        let mut listed = Vec::new();
        while let Some(url) = next {
            let page: Page<Item> = self
                .http
                .json(|agent| agent.get(&url).set("Authorization", &bearer).call())?;
            listed.extend(page.data.into_iter().map(|item| Listed {
                id: item.node.id,
                refs: vec![SiteRef::Mal(medium, item.node.id)],
                entry: list_entry(medium, &item.list_status),
                totals: item.node.totals(medium),
                title: item.node.title,
            }));
            next = page.paging.next;
        }
        Ok(listed)
    }

    /// One request per ref; AniList refs aren't MAL's to find.
    fn find(&mut self, refs: &[SiteRef]) -> Result<HashMap<SiteRef, Found>> {
        let bearer = self.bearer.clone();
        let mut found = HashMap::new();
        for &site_ref in refs {
            let SiteRef::Mal(medium, id) = site_ref else {
                continue;
            };
            let url = format!("{API}/{}/{id}", medium.as_str());
            let node: Node = match self.http.json(|agent| {
                agent
                    .get(&url)
                    .query("fields", total_fields(medium))
                    .set("Authorization", &bearer)
                    .call()
            }) {
                Ok(node) => node,
                Err(Error::Http { status: 404, .. }) => continue,
                Err(error) => return Err(error),
            };
            let totals = node.totals(medium);
            found.insert(site_ref, Found { id, totals });
        }
        Ok(found)
    }

    fn save(&mut self, medium: Medium, id: u64, patch: &Patch) -> Result<()> {
        let url = format!("{API}/{}/{id}/my_list_status", medium.as_str());
        let form = form(medium, patch);
        let pairs: Vec<(&str, &str)> = form.iter().map(|(key, value)| (*key, value.as_str())).collect();
        let bearer = self.bearer.clone();
        self.http.send(|agent| {
            agent
                .request("PATCH", &url)
                .set("Authorization", &bearer)
                .send_form(&pairs)
        })?;
        Ok(())
    }
}

/// An app registered at <https://myanimelist.net/apiconfig>.
pub struct Client {
    pub id: String,
    /// Only "web" apps have one.
    pub secret: Option<String>,
    /// Needed when the app has more than one.
    pub redirect_uri: Option<String>,
}

pub fn authorize_url(client: &Client, verifier: &str, state: &str) -> String {
    let mut params = vec![
        ("response_type", "code"),
        ("client_id", client.id.as_str()),
        ("code_challenge", verifier),
        ("code_challenge_method", "plain"),
        ("state", state),
    ];
    if let Some(redirect_uri) = &client.redirect_uri {
        params.push(("redirect_uri", redirect_uri));
    }
    url::Url::parse_with_params(&format!("{OAUTH}/authorize"), params)
        .expect("static URL")
        .to_string()
}

#[derive(Deserialize)]
struct Issued {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: i64,
}

fn token_request(
    client_id: &str,
    client_secret: Option<&str>,
    grant: &[(&str, &str)],
    now: i64,
) -> Result<Token> {
    let mut form = vec![("client_id", client_id)];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret));
    }
    form.extend_from_slice(grant);
    let mut http = Paced::new("MAL");
    let issued: Issued = http.json(|agent| agent.post(&format!("{OAUTH}/token")).send_form(&form))?;
    Ok(Token {
        access_token: issued.access_token,
        refresh_token: issued.refresh_token,
        expires_at: now + issued.expires_in,
        client_id: Some(client_id.to_string()),
        client_secret: client_secret.map(String::from),
        user: None,
    })
}

pub fn exchange(client: &Client, code: &str, verifier: &str, now: i64) -> Result<Token> {
    let mut grant = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", verifier),
    ];
    if let Some(redirect_uri) = &client.redirect_uri {
        grant.push(("redirect_uri", redirect_uri));
    }
    token_request(&client.id, client.secret.as_deref(), &grant, now)
}

/// A new token (and refresh token) for `token`'s refresh token.
pub fn refresh(token: &Token, now: i64) -> Result<Token> {
    let missing = |what: &str| Error::Auth(format!("the saved MAL token has no {what}"));
    let refresh_token = token.refresh_token.as_deref().ok_or_else(|| missing("refresh token"))?;
    let client_id = token.client_id.as_deref().ok_or_else(|| missing("client id"))?;
    let grant = [("grant_type", "refresh_token"), ("refresh_token", refresh_token)];
    token_request(client_id, token.client_secret.as_deref(), &grant, now)
}

#[cfg(test)]
mod tests {
    use time::macros::date;

    use super::*;

    #[test]
    fn list_status_reads_rewatches_as_current() {
        let status: Status = serde_json::from_str(
            r#"{"status":"completed","is_rewatching":true,"num_episodes_watched":4,
                "num_times_rewatched":1,"start_date":"2020-05","finish_date":"2020-06-01","score":8}"#,
        )
        .unwrap();
        let entry = list_entry(Medium::Anime, &status);
        assert_eq!((entry.status, entry.rewatching), (ListStatus::Current, true));
        assert_eq!((entry.progress, entry.repeats), (Some(4), 1));
        assert_eq!((entry.started, entry.finished), (None, Some(date!(2020 - 06 - 01))));
        assert_eq!(entry.score, Some(8));
    }

    #[test]
    fn list_pages_parse() {
        let page: Page<Item> = serde_json::from_str(
            r#"{"data":[{"node":{"id":2,"title":"Berserk","num_chapters":0,"num_volumes":41},
                "list_status":{"status":"reading","num_chapters_read":30,"num_volumes_read":3}}],
                "paging":{}}"#,
        )
        .unwrap();
        let item = &page.data[0];
        let entry = list_entry(Medium::Manga, &item.list_status);
        assert_eq!((entry.progress, entry.volumes), (Some(30), Some(3)));
        assert_eq!(
            item.node.totals(Medium::Manga),
            Totals { units: None, volumes: Some(41) }
        );
        assert!(page.paging.next.is_none());
    }

    #[test]
    fn patches_become_form_fields() {
        let patch = Patch {
            status: Some((ListStatus::Current, true)),
            progress: Some(3),
            started: Some(date!(2026 - 10 - 01)),
            score: Some(7),
            ..Patch::default()
        };
        assert_eq!(
            form(Medium::Anime, &patch),
            [
                ("status", "completed".to_string()),
                ("is_rewatching", "true".into()),
                ("num_watched_episodes", "3".into()),
                ("start_date", "2026-10-01".into()),
                ("score", "7".into()),
            ]
        );
        let manga = Patch {
            status: Some((ListStatus::Planned, false)),
            volumes: Some(2),
            repeats: Some(1),
            ..Patch::default()
        };
        assert_eq!(
            form(Medium::Manga, &manga),
            [
                ("status", "plan_to_read".to_string()),
                ("is_rereading", "false".into()),
                ("num_volumes_read", "2".into()),
                ("num_times_reread", "1".into()),
            ]
        );
    }

    #[test]
    fn the_authorize_url_carries_the_challenge() {
        let client = Client {
            id: "abc".into(),
            secret: None,
            redirect_uri: Some("http://localhost/cb".into()),
        };
        let url = authorize_url(&client, "verifier", "st");
        assert_eq!(
            url,
            "https://myanimelist.net/v1/oauth2/authorize?response_type=code&client_id=abc\
             &code_challenge=verifier&code_challenge_method=plain&state=st\
             &redirect_uri=http%3A%2F%2Flocalhost%2Fcb"
        );
    }
}
