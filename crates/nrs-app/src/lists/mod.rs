//! Exports NRS watch/read progress (`nrs_store::Progress`) and 1-10
//! scores (`score`) to MyAnimeList and AniList lists. NRS is the source
//! of truth: a list entry is made to say what its progress row says, and
//! only the fields that differ are written. Notes and list entries NRS
//! doesn't track are never touched, and a field NRS doesn't know (no
//! start date, a count in minutes) is left as the site has it.
//!
//! Two ways out: `live` pushes a row's progress (not its score) as soon
//! as it's written; `job` runs a full sync, progress and scores, from the
//! `/lists` page or a scheduled `POST /api/v1/lists/sync`. Each site has
//! its own `network` lock: work for one site goes one batch at a time, so
//! it can't race itself or overrun that site's rate limit, while the two
//! sites run alongside each other.
//!
//! `want` and `diff` are the mapping; `plan::plan` runs them over a
//! `Site`'s list; `mal` and `anilist` are the two sites.

// `ureq::Error` is ureq's, returned from the request closures `http::Paced`
// takes.
#![allow(clippy::result_large_err)]

pub mod anilist;
mod http;
pub mod job;
pub mod live;
pub mod mal;
pub mod plan;
pub mod score;
pub mod tokens;

use std::fmt;
use std::sync::{Mutex, MutexGuard};

use nrs_core::Entry;
use nrs_store::{Progress, ProgressStatus, ProgressUnit};
use time::Date;

pub use http::{rate_limited_until, Error};
pub use plan::Site;

pub type Result<T> = std::result::Result<T, Error>;

pub const SITES: [&str; 2] = ["mal", "anilist"];

static NETWORK: [Mutex<()>; SITES.len()] = [Mutex::new(()), Mutex::new(())];

fn site_index(site: &str) -> usize {
    SITES.iter().position(|&known| known == site).unwrap_or(0)
}

/// Held for any batch of requests to `site` (`"mal"` or `"anilist"`).
pub fn network(site: &str) -> MutexGuard<'static, ()> {
    NETWORK[site_index(site)]
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// A logged-in client for `site` (`"mal"` or `"anilist"`), with any note
/// on its token (a refresh).
pub fn connect(site: &str) -> Result<(Box<dyn Site + Send>, Option<String>)> {
    let path = tokens::default_path();
    match site {
        "mal" => {
            let (token, note) = tokens::fresh_mal(&path, now())?;
            Ok((Box::new(mal::Mal::new(&token)), note))
        }
        "anilist" => {
            let token = tokens::Tokens::load(&path)?
                .anilist
                .filter(|token| token.days_left(now()) >= 0)
                .ok_or_else(|| Error::Auth("not logged in to AniList".into()))?;
            Ok((Box::new(anilist::AniList::new(&token)), None))
        }
        _ => Err(Error::Auth(format!("no list site {site:?}"))),
    }
}

/// The sites with a saved token that hasn't run out (a MAL one may still
/// refresh).
pub fn connected() -> Vec<&'static str> {
    let Ok(tokens) = tokens::Tokens::load(&tokens::default_path()) else {
        return Vec::new();
    };
    let now = now();
    let mut sites = Vec::new();
    if tokens.mal.is_some() {
        sites.push("mal");
    }
    if tokens.anilist.is_some_and(|token| token.days_left(now) >= 0) {
        sites.push("anilist");
    }
    sites
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Medium {
    Anime,
    Manga,
}

impl Medium {
    pub fn as_str(self) -> &'static str {
        match self {
            Medium::Anime => "anime",
            Medium::Manga => "manga",
        }
    }

    /// What `ListEntry::progress` counts.
    fn units(self) -> &'static str {
        match self {
            Medium::Anime => "episodes",
            Medium::Manga => "chapters",
        }
    }
}

/// `bytes` random bytes as hex, for PKCE verifiers and login states.
pub fn random_hex(bytes: usize) -> std::io::Result<String> {
    use std::io::Read;
    let mut buffer = vec![0u8; bytes];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buffer)?;
    Ok(buffer.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// A site's id for an entry, read from the URL the entry is bound by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SiteRef {
    Mal(Medium, u64),
    AniList(Medium, u64),
}

impl SiteRef {
    pub fn medium(self) -> Medium {
        match self {
            SiteRef::Mal(medium, _) | SiteRef::AniList(medium, _) => medium,
        }
    }

    /// What `parse_ref` reads back.
    pub fn url(self) -> String {
        match self {
            SiteRef::Mal(medium, id) => format!("https://myanimelist.net/{}/{id}", medium.as_str()),
            SiteRef::AniList(medium, id) => format!("https://anilist.co/{}/{id}", medium.as_str()),
        }
    }
}

/// `https://myanimelist.net/anime/5114` and `https://anilist.co/manga/30013`
/// (trailing slugs allowed); anything else is `None`.
pub fn parse_ref(url: &str) -> Option<SiteRef> {
    let url = url::Url::parse(url.trim()).ok()?;
    let mut segments = url.path_segments()?;
    let medium = match segments.next()? {
        "anime" => Medium::Anime,
        "manga" => Medium::Manga,
        _ => return None,
    };
    let id = segments.next()?.parse().ok()?;
    match url.host_str()?.trim_start_matches("www.") {
        "myanimelist.net" => Some(SiteRef::Mal(medium, id)),
        "anilist.co" => Some(SiteRef::AniList(medium, id)),
        _ => None,
    }
}

/// The site entry a URL-bound NRS entry is.
pub fn site_ref(entry: &Entry) -> Option<SiteRef> {
    (entry.store == "url")
        .then(|| parse_ref(&entry.store_id))
        .flatten()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListStatus {
    Planned,
    Current,
    Paused,
    Completed,
    Dropped,
}

impl ListStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ListStatus::Planned => "planned",
            ListStatus::Current => "current",
            ListStatus::Paused => "paused",
            ListStatus::Completed => "completed",
            ListStatus::Dropped => "dropped",
        }
    }
}

/// One list entry, as a site has it or as NRS wants it. `None` counts and
/// dates are unknown: on the site's side, unset; on NRS's, left alone.
#[derive(Debug, Clone, PartialEq)]
pub struct ListEntry {
    pub status: ListStatus,
    /// A rewatch/reread under way; only with `Current`.
    pub rewatching: bool,
    /// Episodes, or chapters for manga.
    pub progress: Option<u32>,
    /// Manga only.
    pub volumes: Option<u32>,
    /// Finished rewatches/rereads.
    pub repeats: u32,
    pub started: Option<Date>,
    pub finished: Option<Date>,
    /// On the site's own scale (`Site::points`); `None`: unscored.
    pub score: Option<u32>,
}

/// How long the site says an entry is; `None` when it doesn't know.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Episodes, or chapters for manga.
    pub units: Option<u32>,
    pub volumes: Option<u32>,
}

const EPOCH: Date = time::macros::date!(1970 - 01 - 01);

fn day(days: i32) -> Option<Date> {
    Date::from_julian_day(EPOCH.to_julian_day() + days).ok()
}

fn count(value: f32) -> u32 {
    value.max(0.0).floor() as u32
}

/// What `progress` says the site's entry should be.
///
/// `rewatch` counts finished viewings before the current one, so an
/// active row with `rewatch > 0` is a rewatch under way, and a planned,
/// paused or dropped one is a rewatch that isn't: the list keeps showing
/// the last finished viewing (completed). Dates describe the first
/// viewing on both sites, so they're only sent while `rewatch` is 0.
///
/// A count goes over only in the site's own unit (episodes; chapters or
/// volumes), clamped to the site's total. A completed entry is sent as
/// all of the site's total, or NRS's own when the site has none.
///
/// `score` is already on the site's scale; a planned entry has none.
pub fn want(progress: &Progress, medium: Medium, totals: Totals, score: Option<u32>) -> ListEntry {
    let earlier = progress.rewatch;
    let (status, rewatching, repeats) = match (progress.status, earlier) {
        (ProgressStatus::Completed, n) => (ListStatus::Completed, false, n),
        (ProgressStatus::Active, 0) => (ListStatus::Current, false, 0),
        (ProgressStatus::Active, n) => (ListStatus::Current, true, n - 1),
        (ProgressStatus::Planned, 0) => (ListStatus::Planned, false, 0),
        (ProgressStatus::Paused, 0) => (ListStatus::Paused, false, 0),
        (ProgressStatus::Dropped, 0) => (ListStatus::Dropped, false, 0),
        (_, n) => (ListStatus::Completed, false, n - 1),
    };
    let finished = status == ListStatus::Completed;
    let count_in = |unit: ProgressUnit, total: Option<u32>| {
        let own = (progress.unit == unit).then_some(progress);
        if finished {
            total.or_else(|| own.and_then(|own| own.total).map(count))
        } else {
            own.map(|own| {
                let current = count(own.current);
                total.map_or(current, |total| current.min(total))
            })
        }
    };
    let (progress_count, volumes) = match medium {
        Medium::Anime => (count_in(ProgressUnit::Episode, totals.units), None),
        Medium::Manga => (
            count_in(ProgressUnit::Chapter, totals.units),
            count_in(ProgressUnit::Volume, totals.volumes),
        ),
    };
    let first = earlier == 0;
    ListEntry {
        status,
        rewatching,
        progress: progress_count,
        volumes,
        repeats,
        started: progress.started.filter(|_| first).and_then(day),
        finished: progress.finished.filter(|_| first).and_then(day),
        score: score.filter(|_| status != ListStatus::Planned),
    }
}

/// The fields a save sets; `None` ones are left as they are.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Patch {
    /// Always sent together with `rewatching` (MAL folds one into the other).
    pub status: Option<(ListStatus, bool)>,
    pub progress: Option<u32>,
    pub volumes: Option<u32>,
    pub repeats: Option<u32>,
    pub started: Option<Date>,
    pub finished: Option<Date>,
    pub score: Option<u32>,
}

impl Patch {
    fn is_empty(&self) -> bool {
        *self == Patch::default()
    }
}

/// What it takes to make `have` (`None`: not on the list) say `want`;
/// `None` when it already does.
pub fn diff(want: &ListEntry, have: Option<&ListEntry>) -> Option<Patch> {
    let Some(have) = have else {
        return Some(Patch {
            status: Some((want.status, want.rewatching)),
            progress: want.progress,
            volumes: want.volumes,
            repeats: Some(want.repeats).filter(|&repeats| repeats > 0),
            started: want.started,
            finished: want.finished,
            score: want.score,
        });
    };
    let changed = |want: Option<u32>, have: Option<u32>| want.filter(|&n| Some(n) != have);
    let patch = Patch {
        status: (want.status != have.status || want.rewatching != have.rewatching)
            .then_some((want.status, want.rewatching)),
        progress: changed(want.progress, have.progress),
        volumes: changed(want.volumes, have.volumes),
        repeats: (want.repeats != have.repeats).then_some(want.repeats),
        started: want.started.filter(|&date| Some(date) != have.started),
        finished: want.finished.filter(|&date| Some(date) != have.finished),
        score: changed(want.score, have.score),
    };
    (!patch.is_empty()).then_some(patch)
}

/// `patch` against `have`, e.g. `status current→completed, episodes 3→12`.
pub struct Describe<'a> {
    pub medium: Medium,
    pub patch: &'a Patch,
    pub have: Option<&'a ListEntry>,
}

impl fmt::Display for Describe<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn show<T: fmt::Display>(value: Option<T>) -> String {
            value.map_or("-".into(), |value| value.to_string())
        }
        fn status((status, rewatching): (ListStatus, bool)) -> String {
            match rewatching {
                true => format!("{} (rewatch)", status.as_str()),
                false => status.as_str().to_string(),
            }
        }
        let have = self.have;
        let mut parts = Vec::new();
        let mut field = |name: &str, from: String, to: String| match have {
            Some(_) => parts.push(format!("{name} {from}→{to}")),
            None => parts.push(format!("{name} {to}")),
        };
        let patch = self.patch;
        if let Some(to) = patch.status {
            field(
                "status",
                show(have.map(|have| status((have.status, have.rewatching)))),
                status(to),
            );
        }
        if let Some(to) = patch.progress {
            field(self.medium.units(), show(have.and_then(|have| have.progress)), to.to_string());
        }
        if let Some(to) = patch.volumes {
            field("volumes", show(have.and_then(|have| have.volumes)), to.to_string());
        }
        if let Some(to) = patch.repeats {
            field("repeats", show(have.map(|have| have.repeats)), to.to_string());
        }
        if let Some(to) = patch.started {
            field("started", show(have.and_then(|have| have.started)), to.to_string());
        }
        if let Some(to) = patch.finished {
            field("finished", show(have.and_then(|have| have.finished)), to.to_string());
        }
        if let Some(to) = patch.score {
            field("score", show(have.and_then(|have| have.score)), to.to_string());
        }
        f.write_str(&parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use nrs_core::EntryId;
    use time::macros::date;

    use super::*;

    fn progress(status: ProgressStatus, unit: ProgressUnit, current: f32) -> Progress {
        let mut progress = Progress::new(EntryId("e".into()), status, unit);
        progress.current = current;
        progress
    }

    fn days(date: Date) -> i32 {
        date.to_julian_day() - EPOCH.to_julian_day()
    }

    fn entry(status: ListStatus, progress: Option<u32>) -> ListEntry {
        ListEntry {
            status,
            rewatching: false,
            progress,
            volumes: None,
            repeats: 0,
            started: None,
            finished: None,
            score: None,
        }
    }

    #[test]
    fn refs_come_from_mal_and_anilist_urls() {
        assert_eq!(
            parse_ref("https://myanimelist.net/anime/5114"),
            Some(SiteRef::Mal(Medium::Anime, 5114))
        );
        assert_eq!(
            parse_ref("https://myanimelist.net/manga/2/Berserk"),
            Some(SiteRef::Mal(Medium::Manga, 2))
        );
        assert_eq!(
            parse_ref("https://anilist.co/anime/21/"),
            Some(SiteRef::AniList(Medium::Anime, 21))
        );
        assert_eq!(parse_ref("https://vndb.org/v17516"), None);
        for site_ref in [SiteRef::Mal(Medium::Manga, 2), SiteRef::AniList(Medium::Anime, 21)] {
            assert_eq!(parse_ref(&site_ref.url()), Some(site_ref));
        }
        assert_eq!(parse_ref("https://myanimelist.net/people/1"), None);
        let entry = |store: &str| Entry {
            id: EntryId("e".into()),
            store: store.into(),
            store_id: "https://myanimelist.net/anime/1".into(),
            title_cache: None,
        };
        assert_eq!(site_ref(&entry("url")), Some(SiteRef::Mal(Medium::Anime, 1)));
        assert_eq!(site_ref(&entry("anime")), None);
    }

    #[test]
    fn active_episodes_go_over_clamped_to_the_site_total() {
        let row = progress(ProgressStatus::Active, ProgressUnit::Episode, 7.5);
        let want = want(&row, Medium::Anime, Totals::default(), None);
        assert_eq!((want.status, want.progress), (ListStatus::Current, Some(7)));
        let clamped = super::want(
            &progress(ProgressStatus::Active, ProgressUnit::Episode, 30.0),
            Medium::Anime,
            Totals { units: Some(24), volumes: None },
            None,
        );
        assert_eq!(clamped.progress, Some(24));
    }

    #[test]
    fn completed_takes_the_site_total_then_its_own() {
        let mut row = progress(ProgressStatus::Completed, ProgressUnit::Episode, 12.0);
        row.total = Some(12.0);
        let site = Totals { units: Some(13), volumes: None };
        assert_eq!(want(&row, Medium::Anime, site, None).progress, Some(13));
        assert_eq!(want(&row, Medium::Anime, Totals::default(), None).progress, Some(12));
    }

    #[test]
    fn minutes_never_become_counts() {
        let mut row = progress(ProgressStatus::Active, ProgressUnit::Minute, 90.0);
        row.total = Some(900.0);
        let want = want(&row, Medium::Manga, Totals::default(), None);
        assert_eq!((want.progress, want.volumes), (None, None));
        row.status = ProgressStatus::Completed;
        let site = Totals { units: Some(40), volumes: Some(5) };
        let done = super::want(&row, Medium::Manga, site, None);
        assert_eq!((done.progress, done.volumes), (Some(40), Some(5)));
        assert_eq!(super::want(&row, Medium::Manga, Totals::default(), None).progress, None);
    }

    #[test]
    fn rewatches_keep_the_last_finished_viewing() {
        let mut row = progress(ProgressStatus::Active, ProgressUnit::Episode, 3.0);
        row.rewatch = 2;
        row.started = Some(days(date!(2026 - 10 - 01)));
        let under_way = want(&row, Medium::Anime, Totals::default(), None);
        assert_eq!(
            (under_way.status, under_way.rewatching, under_way.repeats),
            (ListStatus::Current, true, 1)
        );
        assert_eq!(under_way.started, None, "a rewatch's dates aren't the first viewing's");

        row.status = ProgressStatus::Dropped;
        let dropped = want(&row, Medium::Anime, Totals { units: Some(12), volumes: None }, None);
        assert_eq!(
            (dropped.status, dropped.rewatching, dropped.repeats, dropped.progress),
            (ListStatus::Completed, false, 1, Some(12))
        );

        row.status = ProgressStatus::Completed;
        assert_eq!(want(&row, Medium::Anime, Totals::default(), None).repeats, 2);
    }

    #[test]
    fn dates_go_over_on_a_first_viewing() {
        let mut row = progress(ProgressStatus::Completed, ProgressUnit::Episode, 12.0);
        row.started = Some(days(date!(2026 - 01 - 05)));
        row.finished = Some(days(date!(2026 - 03 - 30)));
        let want = want(&row, Medium::Anime, Totals::default(), None);
        assert_eq!(want.started, Some(date!(2026 - 01 - 05)));
        assert_eq!(want.finished, Some(date!(2026 - 03 - 30)));
    }

    #[test]
    fn a_new_entry_sends_what_nrs_knows() {
        let mut want = entry(ListStatus::Current, Some(3));
        want.started = Some(date!(2026 - 10 - 01));
        let patch = diff(&want, None).unwrap();
        assert_eq!(patch.status, Some((ListStatus::Current, false)));
        assert_eq!(patch.progress, Some(3));
        assert_eq!(patch.repeats, None);
        assert_eq!(patch.started, Some(date!(2026 - 10 - 01)));
    }

    #[test]
    fn only_differing_fields_are_sent() {
        let want = entry(ListStatus::Completed, Some(12));
        let mut have = entry(ListStatus::Current, Some(12));
        have.started = Some(date!(2020 - 01 - 01));
        let patch = diff(&want, Some(&have)).unwrap();
        assert_eq!(
            patch,
            Patch {
                status: Some((ListStatus::Completed, false)),
                ..Patch::default()
            }
        );
        have.status = ListStatus::Completed;
        assert_eq!(diff(&want, Some(&have)), None, "unknown fields stay as the site has them");
    }

    #[test]
    fn rewatching_alone_is_a_status_change() {
        let mut want = entry(ListStatus::Current, None);
        want.rewatching = true;
        let have = entry(ListStatus::Current, Some(5));
        assert_eq!(
            diff(&want, Some(&have)).unwrap().status,
            Some((ListStatus::Current, true))
        );
    }

    #[test]
    fn scores_go_over_except_for_planned_entries() {
        let row = progress(ProgressStatus::Dropped, ProgressUnit::Episode, 2.0);
        assert_eq!(want(&row, Medium::Anime, Totals::default(), Some(4)).score, Some(4));
        let planned = progress(ProgressStatus::Planned, ProgressUnit::Episode, 0.0);
        assert_eq!(want(&planned, Medium::Anime, Totals::default(), Some(4)).score, None);
        let mut want = entry(ListStatus::Completed, None);
        want.score = Some(80);
        let mut have = entry(ListStatus::Completed, Some(12));
        have.score = Some(75);
        assert_eq!(diff(&want, Some(&have)).unwrap().score, Some(80));
        have.score = Some(80);
        assert_eq!(diff(&want, Some(&have)), None);
    }

    #[test]
    fn describe_shows_from_and_to() {
        let want = entry(ListStatus::Completed, Some(12));
        let have = entry(ListStatus::Current, Some(3));
        let patch = diff(&want, Some(&have)).unwrap();
        let text = Describe { medium: Medium::Anime, patch: &patch, have: Some(&have) };
        assert_eq!(text.to_string(), "status current→completed, episodes 3→12");
        let added = diff(&want, None).unwrap();
        let text = Describe { medium: Medium::Anime, patch: &added, have: None };
        assert_eq!(text.to_string(), "status completed, episodes 12");
    }
}
