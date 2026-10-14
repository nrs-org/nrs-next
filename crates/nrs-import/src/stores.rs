//! Store-owned facts for stripped XML (`<bind url>` entries).
//!
//! Stripped XML leaves out what the type stores know: titles, and the
//! album -> track containment of the music store. `load_store_facts` reads
//! those for every bound URL through the Store Protocol (musiclib by
//! `lookup`, which never fetches; the anime store by `ingest`), and
//! `apply_store_facts` folds
//! them into a parsed document before `build_snapshot`, so the snapshot is
//! the same shape the legacy XML produced.
//!
//! Progress rule, also shared with `strip/strip_xml.py`: a
//! `<musicConsumedProgress>` without `length` takes the bound track's
//! musiclib length (`track_length`), and an `<animeConsumedProgress>`
//! without `episodes` takes the anime store's episode count. Without
//! `episodeDuration` it takes NRS's 20-minute default (a TV episode minus
//! OP/ED), unless the store's per-episode length is outside the TV range
//! (`is_tv_episode`): movies, specials and shorts take the store's length.
//!
//! Containment rule, shared with `strip/strip_xml.py`: a musiclib
//! `release -> track` edge becomes an NRS `contains` edge only when both
//! ends map one-to-one onto NRS entries (no other NRS entry binds the same
//! musiclib entry), the parent doesn't `<unlink>` it, and the XML doesn't
//! already declare it.

use nrs_entry_store::{EntryStore, HttpEntryStore};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::xml::{ImpactKind, ParsedDocument, ParsedRelation};

/// What the stores say about the URLs a document binds.
#[derive(Debug, Default, Clone)]
pub struct StoreFacts {
    /// Bind URL -> display title.
    pub titles: HashMap<String, String>,
    /// Bind URL -> musiclib store ID.
    pub music_entry: HashMap<String, String>,
    /// musiclib `release -> track` edges among `music_entry` values.
    pub music_contains: HashSet<(String, String)>,
    /// Bind URL -> track length in minutes (musiclib).
    pub lengths: HashMap<String, f32>,
    /// Bind URL -> episode count (anime store).
    pub episodes: HashMap<String, f32>,
    /// Bind URL -> per-episode length in minutes (anime store).
    pub episode_minutes: HashMap<String, f32>,
}

/// Minutes NRS counts per TV episode: OP/ED and previews excluded.
pub const DEFAULT_EPISODE_MINUTES: f32 = 20.0;

/// A store per-episode length (OP/ED included) in the usual TV range, which
/// NRS counts as `DEFAULT_EPISODE_MINUTES` instead.
pub fn is_tv_episode(minutes: f32) -> bool {
    (15.0..30.0).contains(&minutes)
}

/// Fill titles, store bindings and store-asserted containment into `doc`.
/// Returns warnings (stale `<unlink>`s, bound entries with no title).
pub fn apply_store_facts(doc: &mut ParsedDocument, facts: &StoreFacts) -> Vec<String> {
    let mut warnings = Vec::new();

    for entry in &mut doc.entries {
        if entry.binds.is_empty() {
            continue;
        }
        if entry.store == "none" {
            entry.store = "url".into();
            entry.store_id = entry.binds[0].clone();
        }
        if entry.title.is_empty() {
            match entry.binds.iter().find_map(|url| facts.titles.get(url)) {
                Some(title) => {
                    entry.title = title.clone();
                    entry.title_from_store = true;
                }
                None => warnings.push(format!(
                    "entry {}: no store title for {}",
                    entry.id,
                    entry.binds.join(", ")
                )),
            }
        }
        let length = entry.binds.iter().find_map(|url| facts.lengths.get(url));
        let episodes = entry.binds.iter().find_map(|url| facts.episodes.get(url));
        let episode_minutes = entry
            .binds
            .iter()
            .find_map(|url| facts.episode_minutes.get(url))
            .filter(|&&minutes| !is_tv_episode(minutes));
        for impact in &mut entry.impacts {
            match &mut impact.kind {
                ImpactKind::MusicConsumedProgress { duration_minutes }
                    if duration_minutes.is_none() =>
                {
                    *duration_minutes = length.copied();
                }
                ImpactKind::AnimeConsumedProgress {
                    episodes: count,
                    episode_duration_minutes: minutes,
                    ..
                } => {
                    if count.is_none() {
                        *count = episodes.copied();
                    }
                    if minutes.is_none() {
                        *minutes = episode_minutes.copied();
                    }
                }
                _ => {}
            }
        }
    }

    // NRS entry ids per musiclib entry (an id declared twice counts once).
    let mut per_music: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for entry in &doc.entries {
        for url in &entry.binds {
            if let Some(music) = facts.music_entry.get(url) {
                per_music
                    .entry(music.as_str())
                    .or_default()
                    .insert(entry.id.as_str());
            }
        }
    }
    let nrs_of: HashMap<&str, String> = per_music
        .into_iter()
        .filter(|(_, ids)| ids.len() == 1)
        .map(|(music, ids)| (music, ids.into_iter().next().unwrap().to_string()))
        .collect();

    let mut declared: HashSet<(String, String)> = HashSet::new();
    let mut unlinked: HashSet<(String, String)> = HashSet::new();
    for entry in &doc.entries {
        for relation in &entry.relations {
            if let ParsedRelation::Contains { to, .. } = relation {
                declared.insert((entry.id.clone(), to.clone()));
            }
        }
        for target in &entry.unlinks {
            unlinked.insert((entry.id.clone(), target.clone()));
        }
    }

    let mut derived: BTreeSet<(String, String)> = BTreeSet::new();
    for (parent, child) in &facts.music_contains {
        let (Some(parent), Some(child)) = (nrs_of.get(parent.as_str()), nrs_of.get(child.as_str()))
        else {
            continue;
        };
        if parent != child {
            derived.insert((parent.clone(), child.clone()));
        }
    }
    for pair in &unlinked {
        if !derived.contains(pair) {
            warnings.push(format!(
                "entry {}: <unlink id=\"{}\"> matches no store edge",
                pair.0, pair.1
            ));
        }
    }

    let mut first_declaration: HashMap<String, usize> = HashMap::new();
    for (index, entry) in doc.entries.iter().enumerate() {
        first_declaration.entry(entry.id.clone()).or_insert(index);
    }
    for pair in derived {
        if declared.contains(&pair) || unlinked.contains(&pair) {
            continue;
        }
        let index = first_declaration[&pair.0];
        doc.entries[index].relations.push(ParsedRelation::Contains {
            to: pair.1,
            factor: 1.0,
        });
    }
    warnings
}

/// Where to read store facts from. Either may be absent; bound entries the
/// available stores can't answer for keep an empty title (and a warning).
#[derive(Debug, Default, Clone)]
pub struct StoreSources<'a> {
    pub music_store_url: Option<&'a str>,
    pub anime_store_url: Option<&'a str>,
}

/// Where musiclib's server listens by default.
pub const DEFAULT_MUSIC_STORE_URL: &str = "http://127.0.0.1:4600";

impl<'a> StoreSources<'a> {
    /// `NRS_MUSIC_STORE_URL` / `NRS_ANIME_STORE_URL`, for callers without flags.
    pub fn from_env(music: &'a Option<String>, anime: &'a Option<String>) -> Self {
        StoreSources {
            music_store_url: music.as_deref(),
            anime_store_url: anime.as_deref(),
        }
    }
}

/// Load and apply store facts when `doc` binds anything (a legacy document
/// with no `<bind>` is left untouched). Returns warnings.
pub fn resolve_binds(
    doc: &mut ParsedDocument,
    sources: &StoreSources,
) -> Result<Vec<String>, String> {
    let bound = doc.entries.iter().filter(|e| !e.binds.is_empty()).count();
    if bound == 0 {
        return Ok(Vec::new());
    }
    let mut warnings = Vec::new();
    if sources.music_store_url.is_none() && sources.anime_store_url.is_none() {
        warnings.push(format!(
            "{bound} entries use <bind> but no store was given (--music-store / --anime-store): \
             their titles and store-asserted containment are missing"
        ));
    }
    let (facts, load_warnings) = load_store_facts(doc, sources)?;
    warnings.extend(load_warnings);
    warnings.extend(apply_store_facts(doc, &facts));
    Ok(warnings)
}

/// What the stores fill into an entry's progress impacts, for one bind URL
/// (the web editor's defaults). `episode_minutes` is what the importer
/// uses: the store's length for movies and shorts, else the 20-minute
/// default; `store_episode_minutes` is the store's own figure.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ProgressBasis {
    pub length_minutes: Option<f32>,
    pub episodes: Option<f32>,
    pub episode_minutes: f32,
    pub store_episode_minutes: Option<f32>,
}

pub fn progress_basis(url: &str, sources: &StoreSources) -> Result<ProgressBasis, String> {
    let urls = BTreeSet::from([url]);
    let mut facts = StoreFacts::default();
    if let Some(base_url) = sources.music_store_url {
        load_music(base_url, &urls, &mut facts)?;
    }
    if let Some(base_url) = sources.anime_store_url {
        let mut warnings = Vec::new();
        load_anime(base_url, &urls, &mut facts, &mut warnings)?;
        if let Some(warning) = warnings.pop() {
            return Err(warning);
        }
    }
    let store_episode_minutes = facts.episode_minutes.get(url).copied();
    Ok(ProgressBasis {
        length_minutes: facts.lengths.get(url).copied(),
        episodes: facts.episodes.get(url).copied(),
        episode_minutes: store_episode_minutes
            .filter(|&minutes| !is_tv_episode(minutes))
            .unwrap_or(DEFAULT_EPISODE_MINUTES),
        store_episode_minutes,
    })
}

/// Read the facts for every URL `doc` binds.
pub fn load_store_facts(
    doc: &ParsedDocument,
    sources: &StoreSources,
) -> Result<(StoreFacts, Vec<String>), String> {
    let urls: BTreeSet<&str> = doc
        .entries
        .iter()
        .flat_map(|e| e.binds.iter().map(String::as_str))
        .collect();
    let mut facts = StoreFacts::default();
    let mut warnings = Vec::new();
    if let Some(base_url) = sources.music_store_url {
        load_music(base_url, &urls, &mut facts)?;
    }
    if let Some(base_url) = sources.anime_store_url {
        load_anime(base_url, &urls, &mut facts, &mut warnings)?;
    }
    Ok((facts, warnings))
}

/// musiclib through the Store Protocol. `lookup` maps a URL to its entry
/// without fetching; `resolve`'s `extra.sources` lists every pair of the
/// entry, so the pair the URL matched comes first for its title and length;
/// `suggest_relations` gives a release's tracks.
fn load_music(base_url: &str, urls: &BTreeSet<&str>, facts: &mut StoreFacts) -> Result<(), String> {
    let err = |e: nrs_entry_store::Error| format!("music store {base_url}: {e}");
    let store = HttpEntryStore::new(base_url)
        .map_err(err)?
        .expecting_store("musiclib");
    let mut releases = Vec::new();
    for &url in urls {
        let Some(found) = store.lookup(url).map_err(err)? else {
            continue;
        };
        let meta = store.resolve(&found.store_id).map_err(err)?;
        let bound = found.extra["identifier"].as_str().unwrap_or(url);
        let sources = meta.extra["sources"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        if let Some(title) = music_title(sources, bound).or(meta.extra["entry_title"].as_str()) {
            facts.titles.insert(url.to_string(), title.to_string());
        }
        if let Some(ms) = music_length_ms(sources, bound) {
            facts.lengths.insert(url.to_string(), ms as f32 / 60_000.0);
        }
        if meta.kind == "release" {
            releases.push(found.store_id.clone());
        }
        facts.music_entry.insert(url.to_string(), found.store_id);
    }

    let bound: HashSet<&str> = facts.music_entry.values().map(String::as_str).collect();
    for parent in releases {
        for suggestion in store.suggest_relations(&parent).map_err(err)? {
            if suggestion.kind == "contains" && bound.contains(suggestion.target_store_id.as_str())
            {
                facts
                    .music_contains
                    .insert((parent.clone(), suggestion.target_store_id));
            }
        }
    }
    Ok(())
}

/// The bound pair's own title.
fn music_title<'a>(sources: &'a [Value], bound: &str) -> Option<&'a str> {
    sources
        .iter()
        .filter(|s| s["identifier"] == bound)
        .find_map(|s| s["title"].as_str())
}

/// The bound pair's own length first, then the entry's other pairs:
/// MusicBrainz, then Spotify, then the rest.
fn music_length_ms(sources: &[Value], bound: &str) -> Option<i64> {
    sources
        .iter()
        .filter_map(|s| {
            let ms = s["duration_ms"].as_i64().filter(|&ms| ms > 0)?;
            let source = s["source"].as_str().unwrap_or_default();
            let identifier = s["identifier"].as_str().unwrap_or_default();
            let rank = match source {
                "musicbrainz" => 0,
                "spotify" => 1,
                _ => 2,
            };
            Some(((identifier != bound, rank, source, identifier), ms))
        })
        .min_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, ms)| ms)
}

/// An anime-store entry URL in the shape `<bind url>`s pin
/// (`https://myanimelist.net/anime/5114`, `https://vndb.org/v17`): no
/// `www.`, slug, query or fragment. `None` when `url` isn't one.
pub fn canonical_anime_url(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let mut parts = rest.split('/');
    let host = parts.next()?;
    let host = host.strip_prefix("www.").unwrap_or(host);
    let numeric = |id: &str| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit());
    match host {
        "myanimelist.net" | "anilist.co" | "kitsu.app" => {
            let kind = parts
                .next()
                .filter(|kind| matches!(*kind, "anime" | "manga"))?;
            // Kitsu also addresses entries by slug.
            let id = parts
                .next()
                .filter(|id| numeric(id) || (host == "kitsu.app" && !id.is_empty()))?;
            Some(format!("https://{host}/{kind}/{id}"))
        }
        "vndb.org" => {
            let id = parts
                .next()
                .filter(|id| id.strip_prefix('v').is_some_and(numeric))?;
            Some(format!("https://vndb.org/{id}"))
        }
        _ => None,
    }
}

/// URLs the anime store can ingest (anime, manga/novels, visual novels).
fn is_anime_store_url(url: &str) -> bool {
    [
        "https://myanimelist.net/",
        "https://anilist.co/",
        "https://vndb.org/",
        "https://kitsu.app/",
    ]
    .iter()
    .any(|prefix| url.starts_with(prefix))
}

fn load_anime(
    base_url: &str,
    urls: &BTreeSet<&str>,
    facts: &mut StoreFacts,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let store =
        HttpEntryStore::new(base_url).map_err(|e| format!("anime store {base_url}: {e}"))?;
    for &url in urls.iter().filter(|url| is_anime_store_url(url)) {
        // Providers rate-limit bursts (MAL answers with a redirect to
        // error.json), so retry a failed lookup with a short backoff.
        let mut attempt = 0;
        loop {
            match store.ingest(url).and_then(|id| store.resolve(&id)) {
                Ok(meta) => {
                    let basis = &meta.extra["progress_basis"];
                    if let Some(count) = basis["episode_count"].as_f64() {
                        facts.episodes.insert(url.to_string(), count as f32);
                    }
                    if let Some(seconds) = basis["episode_duration_seconds"].as_f64() {
                        facts
                            .episode_minutes
                            .insert(url.to_string(), seconds as f32 / 60.0);
                    }
                    facts.titles.insert(url.to_string(), meta.title);
                    break;
                }
                Err(e) if attempt >= 3 => {
                    warnings.push(format!("anime store: {url}: {e}"));
                    break;
                }
                Err(_) => {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_secs(2 << attempt));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::parse_str;

    #[test]
    fn canonical_anime_url_strips_to_the_bind_shape() {
        let canonical = |url| canonical_anime_url(url);
        assert_eq!(
            canonical("https://myanimelist.net/anime/5114/Fullmetal_Alchemist__Brotherhood?q=1")
                .as_deref(),
            Some("https://myanimelist.net/anime/5114")
        );
        assert_eq!(
            canonical("http://www.myanimelist.net/manga/2/").as_deref(),
            Some("https://myanimelist.net/manga/2")
        );
        assert_eq!(
            canonical("https://anilist.co/anime/1535/Death-Note/").as_deref(),
            Some("https://anilist.co/anime/1535")
        );
        assert_eq!(
            canonical("https://vndb.org/v17#main").as_deref(),
            Some("https://vndb.org/v17")
        );
        assert_eq!(
            canonical("https://kitsu.app/anime/death-note").as_deref(),
            Some("https://kitsu.app/anime/death-note")
        );
        assert_eq!(canonical("https://myanimelist.net/people/1"), None);
        assert_eq!(canonical("https://vndb.org/r17"), None);
        assert_eq!(canonical("https://vgmdb.net/album/104418"), None);
        assert_eq!(canonical("mal:5114"), None);
    }

    fn doc(xml: &str) -> ParsedDocument {
        let doc = parse_str(xml, time::macros::date!(2020 - 01 - 01)).unwrap();
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        doc
    }

    fn contains(doc: &ParsedDocument, id: &str) -> Vec<String> {
        let mut out: Vec<String> = doc
            .entries
            .iter()
            .filter(|e| e.id == id)
            .flat_map(|e| &e.relations)
            .filter_map(|r| match r {
                ParsedRelation::Contains { to, .. } => Some(to.clone()),
                _ => None,
            })
            .collect();
        out.sort();
        out
    }

    fn facts(entries: &[(&str, &str, &str)], edges: &[(&str, &str)]) -> StoreFacts {
        StoreFacts {
            titles: entries
                .iter()
                .map(|(u, _, t)| (u.to_string(), t.to_string()))
                .collect(),
            music_entry: entries
                .iter()
                .map(|(u, e, _)| (u.to_string(), e.to_string()))
                .collect(),
            music_contains: edges
                .iter()
                .map(|(p, c)| (p.to_string(), c.to_string()))
                .collect(),
            ..StoreFacts::default()
        }
    }

    #[test]
    fn fills_missing_progress_from_the_stores() {
        let mut d = doc(r#"<document>
            <entry id="M-1"><bind url="u:track" /><musicConsumedProgress /></entry>
            <entry id="M-2"><bind url="u:track2" /><musicConsumedProgress length="3:00" /></entry>
            <entry id="A-1"><bind url="u:tv" />
                <animeConsumedProgress status="Completed" boredom="1" /></entry>
            <entry id="A-2"><bind url="u:movie" />
                <animeConsumedProgress status="Completed" boredom="1" /></entry>
            <entry id="A-3"><bind url="u:tv" />
                <animeConsumedProgress status="Dropped" boredom="1" episodes="3" episodeDuration="10:00" /></entry>
        </document>"#);
        let mut f = StoreFacts::default();
        f.lengths.insert("u:track".into(), 4.5);
        f.lengths.insert("u:track2".into(), 4.5);
        f.episodes.insert("u:tv".into(), 12.0);
        f.episode_minutes.insert("u:tv".into(), 24.0);
        f.episodes.insert("u:movie".into(), 1.0);
        f.episode_minutes.insert("u:movie".into(), 110.0);
        apply_store_facts(&mut d, &f);
        let kinds: Vec<&ImpactKind> = d.entries.iter().map(|e| &e.impacts[0].kind).collect();
        assert_eq!(
            kinds[0],
            &ImpactKind::MusicConsumedProgress {
                duration_minutes: Some(4.5)
            }
        );
        // A written value wins over the store.
        assert_eq!(
            kinds[1],
            &ImpactKind::MusicConsumedProgress {
                duration_minutes: Some(3.0)
            }
        );
        // A TV-length episode keeps NRS's default (left `None` here).
        assert_eq!(
            kinds[2],
            &ImpactKind::AnimeConsumedProgress {
                boredom: 1.0,
                episodes: Some(12.0),
                episode_duration_minutes: None
            }
        );
        assert_eq!(
            kinds[3],
            &ImpactKind::AnimeConsumedProgress {
                boredom: 1.0,
                episodes: Some(1.0),
                episode_duration_minutes: Some(110.0)
            }
        );
        assert_eq!(
            kinds[4],
            &ImpactKind::AnimeConsumedProgress {
                boredom: 1.0,
                episodes: Some(3.0),
                episode_duration_minutes: Some(10.0)
            }
        );
    }

    #[test]
    fn fills_titles_and_binding() {
        let mut d = doc(r#"<document>
            <entry id="M-1"><bind url="u:album" /></entry>
            <entry id="F-1" title="Mine" />
        </document>"#);
        let w = apply_store_facts(&mut d, &facts(&[("u:album", "1", "Album")], &[]));
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(d.entries[0].title, "Album");
        assert_eq!(
            (d.entries[0].store.as_str(), d.entries[0].store_id.as_str()),
            ("url", "u:album")
        );
        assert_eq!(d.entries[1].title, "Mine");
        assert_eq!(d.entries[1].store, "none");
    }

    #[test]
    fn derives_store_edges_but_respects_declared_and_unlinked() {
        let mut d = doc(r#"<document>
            <entry id="A"><bind url="u:a" /><unlink id="T3" /><contains id="T2" factor="0.5" /></entry>
            <entry id="T1"><bind url="u:t1" /></entry>
            <entry id="T2"><bind url="u:t2" /></entry>
            <entry id="T3"><bind url="u:t3" /></entry>
        </document>"#);
        let f = facts(
            &[
                ("u:a", "1", "A"),
                ("u:t1", "2", "t1"),
                ("u:t2", "3", "t2"),
                ("u:t3", "4", "t3"),
            ],
            &[("1", "2"), ("1", "3"), ("1", "4")],
        );
        let w = apply_store_facts(&mut d, &f);
        assert!(w.is_empty(), "{w:?}");
        // T1 derived; T2 declared (factor kept, not duplicated); T3 unlinked.
        assert_eq!(contains(&d, "A"), ["T1", "T2"]);
    }

    #[test]
    fn skips_musiclib_entries_bound_twice() {
        let mut d = doc(r#"<document>
            <entry id="A"><bind url="u:a" /></entry>
            <entry id="T1"><bind url="u:t" /></entry>
            <entry id="T2"><bind url="u:t" /></entry>
        </document>"#);
        apply_store_facts(
            &mut d,
            &facts(&[("u:a", "1", "A"), ("u:t", "2", "t")], &[("1", "2")]),
        );
        assert!(contains(&d, "A").is_empty());
    }

    #[test]
    fn warns_on_stale_unlink_and_missing_title() {
        let mut d = doc(r#"<document>
            <entry id="A"><bind url="u:a" /><unlink id="T1" /></entry>
            <entry id="T1"><bind url="u:gone" /></entry>
        </document>"#);
        let w = apply_store_facts(&mut d, &facts(&[("u:a", "1", "A")], &[]));
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w.iter().any(|m| m.contains("no store title")));
        assert!(w.iter().any(|m| m.contains("matches no store edge")));
    }
}
