//! Parser for the legacy `nrsml` XML format (schema:
//! `https://raw.githubusercontent.com/btmxh/nrsml/master/xml/schema.xsd`).
//!
//! This produces an intermediate representation (`ParsedDocument`), not
//! `nrs-core` types directly — `snapshot.rs` does that conversion using
//! `legacy.rs`'s formulas, and `roles.rs` handles `<role>`/`<musicvar>`
//! music-credit weighting. Nothing is silently dropped: anything this
//! parser can't turn into a concrete value (an unresolved `<def>`
//! template variable, a `<script>`/`<def>`/`<ref>` escape hatch) is
//! recorded in `ParsedDocument::warnings` rather than guessed at
//! silently.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use roxmltree::Node;
use time::Date;

#[derive(Debug)]
pub enum ImportError {
    Io(PathBuf, std::io::Error),
    Xml(PathBuf, roxmltree::Error),
    Missing {
        path: PathBuf,
        context: String,
        attribute: &'static str,
    },
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Io(p, e) => write!(f, "{}: {e}", p.display()),
            ImportError::Xml(p, e) => write!(f, "{}: {e}", p.display()),
            ImportError::Missing {
                path,
                context,
                attribute,
            } => {
                write!(
                    f,
                    "{}: {context}: missing required attribute '{attribute}'",
                    path.display()
                )
            }
        }
    }
}
impl std::error::Error for ImportError {}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedEntry {
    pub id: String,
    pub title: String,
    /// Best-effort external id (INFRA.md's `(store, store_id)`),
    /// picked from whichever `<source>` sub-element is present, in
    /// priority order. `store == "none"` if the entry has no
    /// recognisable source at all (common for auto-generated track
    /// ids and bare artist entries).
    pub store: String,
    pub store_id: String,
    pub best_girl: Option<String>,
    pub impacts: Vec<ParsedImpact>,
    pub relations: Vec<ParsedRelation>,
    /// Per-contributor role credit (`roles.rs` converts this into
    /// auto-generated `Contains` relations toward each credited id).
    pub roles: Vec<ParsedRole>,
    pub music_var: Option<ParsedMusicVar>,
}

/// `<musicvar>`: overrides to `roles::MusicVars`'s defaults. `None`
/// fields fall back to the computed default.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ParsedMusicVar {
    pub vocallyrics: Option<f32>,
    pub lyricsmusic: Option<f32>,
    pub emolyrics: Option<f32>,
    pub arrange: Option<f32>,
    pub instperform: Option<f32>,
    pub feat: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedImpact {
    pub kind: ImpactKind,
    /// `(entry id, factor)`; defaults to `[(self, 1.0)]` when no
    /// explicit `<contributor>` children are present.
    pub contributors: Vec<(String, f32)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImpactKind {
    Cry {
        blend: Vec<(String, f32)>,
    },
    Nei {
        base: f32,
        blend: Vec<(String, f32)>,
    },
    Aei {
        base: f32,
        blend: Vec<(String, f32)>,
    },
    Pads {
        days: f32,
        blend: Vec<(String, f32)>,
    },
    Waifu {
        days: f32,
    },
    Ehi,
    Epi {
        factor: f32,
    },
    Jumpscare,
    SleeplessNight,
    Politics,
    InterestField {
        new_field: bool,
    },
    Dropped,
    Meme {
        strength: f32,
        days: f32,
    },
    Music {
        base: f32,
    },
    Additional {
        value: f32,
    },
    Visual {
        visual_type: String,
        base: f32,
        unique: f32,
    },
    OsuSong {
        personal: f32,
        community: f32,
    },
    Writing {
        character: f32,
        story: f32,
        pacing: f32,
        originality: f32,
    },
    AnimeConsumedProgress {
        boredom: f32,
        episodes: f32,
        episode_duration_minutes: f32,
    },
    ConsumedProgress {
        boredom: f32,
        duration_minutes: f32,
    },
    MusicConsumedProgress {
        duration_minutes: f32,
    },
    /// `<regularImpact><score><component value=".." factor=".."/>...`:
    /// a raw, already-computed value directly at a named old factor
    /// (e.g. `"Emotion.AP"`, `"Art.Language"`) - no formula, unlike
    /// every other impact kind.
    Regular {
        components: Vec<(String, f32)>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParsedRelation {
    Remix {
        to: String,
    },
    FeatureMusic {
        to: String,
    },
    KilledBy {
        to: String,
        potential: f32,
        effect: f32,
    },
    Contains {
        to: String,
        factor: f32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedRole {
    pub id: String,
    pub roles_expr: String,
}

#[derive(Debug, Default)]
pub struct ParsedDocument {
    pub entries: Vec<ParsedEntry>,
    pub warnings: Vec<String>,
}

/// Parse a document rooted at `path`, following `<include>` elements
/// recursively (relative to the including file's directory). `today`
/// pins what the `"today"`/`"today+N"` date forms resolve to, so
/// parsing is deterministic given a fixed reference date.
pub fn parse_file(path: &Path, today: Date) -> Result<ParsedDocument, ImportError> {
    let mut doc = ParsedDocument::default();
    parse_file_into(path, today, &mut doc)?;
    Ok(doc)
}

/// Parse XML text directly, with no filesystem access - `<include>`
/// isn't resolvable in this mode (it errors as a missing file).
/// Convenient for tests and for embedded/one-off snippets.
pub fn parse_str(xml: &str, today: Date) -> Result<ParsedDocument, ImportError> {
    let mut doc = ParsedDocument::default();
    parse_text_into(xml, Path::new("<string>"), today, &mut doc)?;
    Ok(doc)
}

fn parse_file_into(path: &Path, today: Date, out: &mut ParsedDocument) -> Result<(), ImportError> {
    let text = fs::read_to_string(path).map_err(|e| ImportError::Io(path.to_path_buf(), e))?;
    parse_text_into(&text, path, today, out)
}

fn parse_text_into(
    text: &str,
    path: &Path,
    today: Date,
    out: &mut ParsedDocument,
) -> Result<(), ImportError> {
    let xml =
        roxmltree::Document::parse(text).map_err(|e| ImportError::Xml(path.to_path_buf(), e))?;
    let root = xml.root_element();
    let dir = path.parent().unwrap_or_else(|| Path::new("."));

    for child in root.children().filter(Node::is_element) {
        match child.tag_name().name() {
            "include" => {
                let src = child.attribute("src").ok_or_else(|| ImportError::Missing {
                    path: path.to_path_buf(),
                    context: "include".into(),
                    attribute: "src",
                })?;
                parse_file_into(&dir.join(src), today, out)?;
            }
            "entry" => parse_entry(child, path, today, &mut out.entries, &mut out.warnings)?,
            other => out.warnings.push(format!(
                "{}: unexpected top-level element <{other}>, skipped",
                path.display()
            )),
        }
    }
    Ok(())
}

fn parse_entry(
    node: Node,
    path: &Path,
    today: Date,
    entries: &mut Vec<ParsedEntry>,
    warnings: &mut Vec<String>,
) -> Result<(), ImportError> {
    let id = node
        .attribute("id")
        .ok_or_else(|| ImportError::Missing {
            path: path.to_path_buf(),
            context: "entry".into(),
            attribute: "id",
        })?
        .to_string();
    let title = node.attribute("title").unwrap_or("").to_string();
    let context_prefix = format!("entry {id}");

    let mut entry = ParsedEntry {
        id: id.clone(),
        title,
        store: "none".into(),
        store_id: id.clone(),
        best_girl: None,
        impacts: Vec::new(),
        relations: Vec::new(),
        roles: Vec::new(),
        music_var: None,
    };

    for child in node.children().filter(Node::is_element) {
        let tag = child.tag_name().name();
        match tag {
            "entry" => {
                let child_id = child
                    .attribute("id")
                    .ok_or_else(|| ImportError::Missing {
                        path: path.to_path_buf(),
                        context: context_prefix.clone(),
                        attribute: "id",
                    })?
                    .to_string();
                entry.relations.push(ParsedRelation::Contains {
                    to: child_id,
                    factor: 1.0,
                });
                parse_entry(child, path, today, entries, warnings)?;
            }
            "source" => parse_source(child, &mut entry),
            "bestGirl" => entry.best_girl = child.attribute("name").map(String::from),
            "contains" => {
                for (to, factor) in parse_contains(child, 1.0) {
                    entry
                        .relations
                        .push(ParsedRelation::Contains { to, factor });
                }
            }
            "role" => {
                if let Some(role_id) = child.attribute("id") {
                    let roles_expr = child.attribute("roles").unwrap_or("").to_string();
                    entry.roles.push(ParsedRole {
                        id: role_id.to_string(),
                        roles_expr,
                    });
                } else {
                    warnings.push(format!(
                        "{context_prefix}: <role> missing required attribute 'id', skipped"
                    ));
                }
            }
            "remix" | "featureMusic" | "killedBy" => {
                if let Some(relation) = parse_relation(child, tag, &context_prefix, warnings) {
                    entry.relations.push(relation);
                }
            }
            "musicvar" => {
                entry.music_var = Some(ParsedMusicVar {
                    vocallyrics: child.attribute("vocallyrics").and_then(|s| s.parse().ok()),
                    lyricsmusic: child.attribute("lyricsmusic").and_then(|s| s.parse().ok()),
                    emolyrics: child.attribute("emolyrics").and_then(|s| s.parse().ok()),
                    arrange: child.attribute("arrange").and_then(|s| s.parse().ok()),
                    instperform: child.attribute("instperform").and_then(|s| s.parse().ok()),
                    feat: child.attribute("feat").map(|s| s == "true"),
                });
            }
            "queue" | "validatorSuppress" => {} // pure metadata
            "script" | "def" | "ref" => warnings.push(format!(
                "{context_prefix}: unsupported <{tag}> element, skipped (not interpretable)"
            )),
            _ => {
                if let Some(kind) = parse_impact_kind(child, tag, today, &context_prefix, warnings)
                {
                    let contributors = parse_contributors(child, &id);
                    entry.impacts.push(ParsedImpact { kind, contributors });
                }
            }
        }
    }

    entries.push(entry);
    Ok(())
}

fn parse_source(node: Node, entry: &mut ParsedEntry) {
    // Priority order for a single (store, store_id): the dedicated
    // provider tags first, then a generic <urls><url .../></urls>
    // fallback (the common case for music entries, which mostly don't
    // have a dedicated tag).
    for provider in ["mal", "al", "adb", "ks", "vndb"] {
        if let Some(child) = node
            .children()
            .find(|c| c.is_element() && c.tag_name().name() == provider)
        {
            if let Some(id) = child.attribute("id") {
                entry.store = provider.to_string();
                entry.store_id = id.to_string();
                return;
            }
        }
    }
    if let Some(vgmdb) = node
        .children()
        .find(|c| c.is_element() && c.tag_name().name() == "vgmdb")
    {
        for key in ["product", "album", "artist", "track", "disc"] {
            if let Some(id) = vgmdb.attribute(key) {
                entry.store = format!("vgmdb-{key}");
                entry.store_id = id.to_string();
                return;
            }
        }
    }
    if let Some(urls) = node
        .children()
        .find(|c| c.is_element() && c.tag_name().name() == "urls")
    {
        if let Some(url) = urls
            .children()
            .find(|c| c.is_element() && c.tag_name().name() == "url")
        {
            if let (Some(name), Some(src)) = (url.attribute("name"), url.attribute("src")) {
                entry.store = format!("url-{name}");
                entry.store_id = src.to_string();
            }
        }
    }
}

/// `<contains>` may nest further `<contains>` children, each
/// multiplying its own `factor` (default `1.0`) into the parent's -
/// e.g. `<contains factor="0.5"><contains id="X"/></contains>`.
fn parse_contains(node: Node, inherited_factor: f32) -> Vec<(String, f32)> {
    let factor = node.attribute("factor").map(parse_fraction).unwrap_or(1.0) * inherited_factor;
    let mut result = Vec::new();
    if let Some(id) = node.attribute("id") {
        result.push((id.to_string(), factor));
    }
    for child in node
        .children()
        .filter(|c| c.is_element() && c.tag_name().name() == "contains")
    {
        result.extend(parse_contains(child, factor));
    }
    result
}

fn parse_fraction(s: &str) -> f32 {
    if let Some((num, den)) = s.split_once('/') {
        let n: f32 = num.trim().parse().unwrap_or(1.0);
        let d: f32 = den.trim().parse().unwrap_or(1.0);
        n / d
    } else {
        s.trim().parse().unwrap_or(1.0)
    }
}

fn parse_contributors(node: Node, self_id: &str) -> Vec<(String, f32)> {
    let contributors: Vec<(String, f32)> = node
        .children()
        .filter(|c| c.is_element() && c.tag_name().name() == "contributor")
        .filter_map(|c| {
            let id = c.attribute("id")?.to_string();
            let factor = c.attribute("factor").map(parse_fraction).unwrap_or(1.0);
            Some((id, factor))
        })
        .collect();
    if contributors.is_empty() {
        vec![(self_id.to_string(), 1.0)]
    } else {
        contributors
    }
}

fn parse_relation(
    node: Node,
    tag: &str,
    context: &str,
    warnings: &mut Vec<String>,
) -> Option<ParsedRelation> {
    let to = match node.attribute("id") {
        Some(id) => id.to_string(),
        None => {
            warnings.push(format!(
                "{context}: <{tag}> missing required attribute 'id', skipped"
            ));
            return None;
        }
    };
    match tag {
        "remix" => Some(ParsedRelation::Remix { to }),
        "featureMusic" => Some(ParsedRelation::FeatureMusic { to }),
        "killedBy" => {
            let potential = req_f32(node, "potential", context, warnings)?;
            let effect = req_f32(node, "effect", context, warnings)?;
            Some(ParsedRelation::KilledBy {
                to,
                potential,
                effect,
            })
        }
        _ => unreachable!(),
    }
}

fn req_f32(node: Node, name: &str, context: &str, warnings: &mut Vec<String>) -> Option<f32> {
    match node.attribute(name).and_then(|s| s.parse().ok()) {
        Some(v) => Some(v),
        None => {
            warnings.push(format!(
                "{context}: <{}> missing/unparseable required attribute '{name}', skipped",
                node.tag_name().name()
            ));
            None
        }
    }
}

fn opt_f32(node: Node, name: &str, default: f32) -> f32 {
    node.attribute(name)
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// `EIEmotions`: either a bare code (`"MU"`, implicit weight 1.0) or a
/// colon-separated list of `CODE-weight` pairs (`"CU-0.5:CP-0.5"`).
fn parse_emotions(s: &str) -> Vec<(String, f32)> {
    if !s.contains(':') && !s.contains('-') {
        return vec![(s.to_string(), 1.0)];
    }
    s.split(':')
        .filter_map(|part| {
            let (code, w) = part.rsplit_once('-')?;
            Some((code.to_string(), w.parse().ok()?))
        })
        .collect()
}

/// `Duration` (`"(\d+:)?\d\d?:\d{2}"`, i.e. `MM:SS` or `HH:MM:SS`) to
/// total minutes.
fn parse_duration_minutes(s: &str) -> Option<f32> {
    let parts: Vec<f32> = s
        .split(':')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    match parts.as_slice() {
        [m, sec] => Some(m + sec / 60.0),
        [h, m, sec] => Some(h * 60.0 + m + sec / 60.0),
        _ => None,
    }
}

/// `Date`: `"today"`, `"today+N"`, `"epoch"`, `"epoch+N"`, or an ISO
/// 8601 date.
fn parse_date(s: &str, today: Date) -> Option<Date> {
    if s == "today" {
        return Some(today);
    }
    if let Some(rest) = s.strip_prefix("today+") {
        return Some(today + time::Duration::days(rest.parse().ok()?));
    }
    const EPOCH: Date = time::macros::date!(1970 - 01 - 01);
    if s == "epoch" {
        return Some(EPOCH);
    }
    if let Some(rest) = s.strip_prefix("epoch+") {
        return Some(EPOCH + time::Duration::days(rest.parse().ok()?));
    }
    Date::parse(s, &time::format_description::well_known::Iso8601::DATE).ok()
}

/// Total days for a `length`/`from`/`to` period, on `node` itself plus
/// any nested `<periods>` children, all summed (INFRA's
/// `DatePeriod[]`). Returns `0.0` (with a warning) if nothing usable
/// is present, rather than failing the whole entry.
fn period_days(node: Node, today: Date, context: &str, warnings: &mut Vec<String>) -> f32 {
    let one = |n: Node| -> Option<f32> {
        if let Some(len) = n.attribute("length") {
            return len.parse().ok();
        }
        let from = n.attribute("from").and_then(|s| parse_date(s, today))?;
        let to = n
            .attribute("to")
            .and_then(|s| parse_date(s, today))
            .unwrap_or(today);
        Some((to - from).whole_days() as f32)
    };

    let mut total = 0.0;
    let mut found = false;
    if let Some(d) = one(node) {
        total += d;
        found = true;
    }
    for child in node
        .children()
        .filter(|c| c.is_element() && c.tag_name().name() == "periods")
    {
        if let Some(d) = one(child) {
            total += d;
            found = true;
        }
    }
    if !found {
        warnings.push(format!(
            "{context}: <{}> has no usable length/from/to (possibly an unresolved template value), treated as 0 days",
            node.tag_name().name()
        ));
    }
    total
}

fn parse_impact_kind(
    node: Node,
    tag: &str,
    today: Date,
    context: &str,
    warnings: &mut Vec<String>,
) -> Option<ImpactKind> {
    let emotions = |n: Node| {
        n.attribute("emotions")
            .map(parse_emotions)
            .unwrap_or_default()
    };

    match tag {
        "cry" => Some(ImpactKind::Cry {
            blend: emotions(node),
        }),
        "nei" => Some(ImpactKind::Nei {
            base: req_f32(node, "base", context, warnings)?,
            blend: emotions(node),
        }),
        "aei" => Some(ImpactKind::Aei {
            base: req_f32(node, "base", context, warnings)?,
            blend: emotions(node),
        }),
        "pads" | "maxAEIPADS" | "cryPADS" => Some(ImpactKind::Pads {
            days: period_days(node, today, context, warnings),
            blend: emotions(node),
        }),
        "waifu" => Some(ImpactKind::Waifu {
            days: period_days(node, today, context, warnings),
        }),
        "ehi" => Some(ImpactKind::Ehi),
        "epi" => Some(ImpactKind::Epi {
            factor: req_f32(node, "base", context, warnings)?,
        }),
        "jumpscare" => Some(ImpactKind::Jumpscare),
        "sleeplessNight" => Some(ImpactKind::SleeplessNight),
        "politics" => Some(ImpactKind::Politics),
        "interestField" => Some(ImpactKind::InterestField {
            new_field: node.attribute("new").map(|s| s == "true").unwrap_or(false),
        }),
        "dropped" => Some(ImpactKind::Dropped),
        "meme" => Some(ImpactKind::Meme {
            strength: req_f32(node, "strength", context, warnings)?,
            days: period_days(node, today, context, warnings),
        }),
        "music" => Some(ImpactKind::Music {
            base: req_f32(node, "base", context, warnings)?,
        }),
        "additional" => Some(ImpactKind::Additional {
            value: req_f32(node, "value", context, warnings)?,
        }),
        "visual" => Some(ImpactKind::Visual {
            visual_type: node.attribute("type").unwrap_or("").to_string(),
            base: req_f32(node, "base", context, warnings)?,
            unique: req_f32(node, "unique", context, warnings)?,
        }),
        "osuSong" => Some(ImpactKind::OsuSong {
            personal: opt_f32(node, "personal", 0.0),
            community: opt_f32(node, "community", 0.0),
        }),
        "writing" => Some(ImpactKind::Writing {
            character: req_f32(node, "character", context, warnings)?,
            story: req_f32(node, "story", context, warnings)?,
            pacing: req_f32(node, "pacing", context, warnings)?,
            originality: req_f32(node, "originality", context, warnings)?,
        }),
        "animeConsumedProgress" => Some(ImpactKind::AnimeConsumedProgress {
            boredom: req_f32(node, "boredom", context, warnings)?,
            episodes: req_f32(node, "episodes", context, warnings)?,
            episode_duration_minutes: node
                .attribute("episodeDuration")
                .and_then(parse_duration_minutes)
                .unwrap_or(20.0),
        }),
        "consumedProgress" => Some(ImpactKind::ConsumedProgress {
            boredom: req_f32(node, "boredom", context, warnings)?,
            duration_minutes: node
                .attribute("duration")
                .and_then(parse_duration_minutes)
                .or_else(|| {
                    warnings.push(format!(
                        "{context}: <consumedProgress> unparseable 'duration'"
                    ));
                    None
                })?,
        }),
        "musicConsumedProgress" => Some(ImpactKind::MusicConsumedProgress {
            duration_minutes: node
                .attribute("length")
                .and_then(parse_duration_minutes)
                .or_else(|| {
                    warnings.push(format!(
                        "{context}: <musicConsumedProgress> unparseable 'length'"
                    ));
                    None
                })?,
        }),
        // `<consumed>`'s duration attribute is (inconsistently, but
        // per-schema) named "length", unlike consumedProgress's "duration".
        "consumed" => Some(ImpactKind::ConsumedProgress {
            boredom: req_f32(node, "boredom", context, warnings)?,
            duration_minutes: node
                .attribute("length")
                .and_then(parse_duration_minutes)
                .or_else(|| {
                    warnings.push(format!("{context}: <consumed> unparseable 'length'"));
                    None
                })?,
        }),
        "regularImpact" => {
            let score = node
                .children()
                .find(|c| c.is_element() && c.tag_name().name() == "score");
            let components: Vec<(String, f32)> = score
                .into_iter()
                .flat_map(|s| s.children())
                .filter(|c| c.is_element() && c.tag_name().name() == "component")
                .filter_map(|c| {
                    let factor = c.attribute("factor")?.to_string();
                    let value: f32 = c.attribute("value")?.parse().ok()?;
                    Some((factor, value))
                })
                .collect();
            if components.is_empty() {
                warnings.push(format!(
                    "{context}: <regularImpact> has no usable <component>s, skipped"
                ));
                None
            } else {
                Some(ImpactKind::Regular { components })
            }
        }
        other => {
            warnings.push(format!("{context}: unknown element <{other}>, skipped"));
            None
        }
    }
}

/// All ids referenced anywhere (contributors, relation targets) - used
/// by `snapshot.rs` to tell a real dangling/null-entry reference apart
/// from an id that just hasn't been parsed yet.
pub fn referenced_ids(doc: &ParsedDocument) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for entry in &doc.entries {
        for impact in &entry.impacts {
            for (id, _) in &impact.contributors {
                ids.insert(id.clone());
            }
        }
        for relation in &entry.relations {
            let to = match relation {
                ParsedRelation::Remix { to }
                | ParsedRelation::FeatureMusic { to }
                | ParsedRelation::KilledBy { to, .. }
                | ParsedRelation::Contains { to, .. } => to,
            };
            ids.insert(to.clone());
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> Date {
        time::macros::date!(2020 - 01 - 11)
    }

    fn entry<'a>(doc: &'a ParsedDocument, id: &str) -> &'a ParsedEntry {
        doc.entries
            .iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| panic!("no entry {id}"))
    }

    #[test]
    fn parses_source_and_cry() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="Test">
                    <source><mal id="123" /></source>
                    <cry emotions="MU" />
                </entry>
            </document>"#,
            today(),
        )
        .unwrap();
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        let e = entry(&doc, "A-1");
        assert_eq!(e.store, "mal");
        assert_eq!(e.store_id, "123");
        assert_eq!(e.impacts.len(), 1);
        assert_eq!(
            e.impacts[0].kind,
            ImpactKind::Cry {
                blend: vec![("MU".into(), 1.0)]
            }
        );
        assert_eq!(e.impacts[0].contributors, vec![("A-1".into(), 1.0)]);
    }

    #[test]
    fn nested_entry_becomes_flat_plus_contains() {
        let doc = parse_str(
            r#"<document>
                <entry id="F-1" title="Franchise">
                    <entry id="A-1" title="Child" />
                </entry>
            </document>"#,
            today(),
        )
        .unwrap();
        assert_eq!(doc.entries.len(), 2);
        let f = entry(&doc, "F-1");
        assert_eq!(
            f.relations,
            vec![ParsedRelation::Contains {
                to: "A-1".into(),
                factor: 1.0
            }]
        );
    }

    #[test]
    fn explicit_contains_parses_fraction_factor() {
        let doc = parse_str(
            r#"<document>
                <entry id="C-1" title="Container">
                    <contains id="M-1" factor="1/5" />
                </entry>
                <entry id="M-1" title="Member" />
            </document>"#,
            today(),
        )
        .unwrap();
        let c = entry(&doc, "C-1");
        assert_eq!(
            c.relations,
            vec![ParsedRelation::Contains {
                to: "M-1".into(),
                factor: 0.2
            }]
        );
    }

    #[test]
    fn nested_contains_multiplies_factor() {
        let doc = parse_str(
            r#"<document>
                <entry id="C-1" title="Container">
                    <contains factor="0.5">
                        <contains id="M-1" />
                    </contains>
                </entry>
                <entry id="M-1" title="Member" />
            </document>"#,
            today(),
        )
        .unwrap();
        let c = entry(&doc, "C-1");
        assert_eq!(
            c.relations,
            vec![ParsedRelation::Contains {
                to: "M-1".into(),
                factor: 0.5
            }]
        );
    }

    #[test]
    fn relations_parse_correctly() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="A">
                    <remix id="B-1" />
                    <featureMusic id="C-1" />
                    <killedBy id="D-1" potential="0.3" effect="1.0" />
                </entry>
            </document>"#,
            today(),
        )
        .unwrap();
        let a = entry(&doc, "A-1");
        assert_eq!(a.relations.len(), 3);
        assert!(a
            .relations
            .contains(&ParsedRelation::Remix { to: "B-1".into() }));
        assert!(a
            .relations
            .contains(&ParsedRelation::FeatureMusic { to: "C-1".into() }));
        assert!(a.relations.contains(&ParsedRelation::KilledBy {
            to: "D-1".into(),
            potential: 0.3,
            effect: 1.0
        }));
    }

    #[test]
    fn contributor_split_including_null_entry() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="A">
                    <cry emotions="MU">
                        <contributor id="A-1" factor="0.7" />
                        <contributor id="Progressive Crisis [null entry]" factor="0.3" />
                    </cry>
                </entry>
            </document>"#,
            today(),
        )
        .unwrap();
        let a = entry(&doc, "A-1");
        assert_eq!(
            a.impacts[0].contributors,
            vec![
                ("A-1".into(), 0.7),
                ("Progressive Crisis [null entry]".into(), 0.3)
            ]
        );
    }

    #[test]
    fn meme_date_range_computes_days() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="A">
                    <meme strength="0.5" from="2020-01-01" to="2020-01-11" />
                </entry>
            </document>"#,
            today(),
        )
        .unwrap();
        assert_eq!(
            entry(&doc, "A-1").impacts[0].kind,
            ImpactKind::Meme {
                strength: 0.5,
                days: 10.0
            }
        );
    }

    #[test]
    fn meme_today_reference_resolves_against_fixed_date() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="A">
                    <meme strength="0.5" from="2020-01-01" to="today" />
                </entry>
            </document>"#,
            today(), // fixed at 2020-01-11
        )
        .unwrap();
        assert_eq!(
            entry(&doc, "A-1").impacts[0].kind,
            ImpactKind::Meme {
                strength: 0.5,
                days: 10.0
            }
        );
    }

    #[test]
    fn missing_period_warns_and_defaults_to_zero_days() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="A"><waifu waifu="X" /></entry>
            </document>"#,
            today(),
        )
        .unwrap();
        assert_eq!(
            entry(&doc, "A-1").impacts[0].kind,
            ImpactKind::Waifu { days: 0.0 }
        );
        assert!(doc
            .warnings
            .iter()
            .any(|w| w.contains("no usable length/from/to")));
    }

    #[test]
    fn role_is_carried() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="A"><role id="M-2" roles="vocal+image" /></entry>
            </document>"#,
            today(),
        )
        .unwrap();
        let a = entry(&doc, "A-1");
        assert_eq!(
            a.roles,
            vec![ParsedRole {
                id: "M-2".into(),
                roles_expr: "vocal+image".into()
            }]
        );
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    }

    #[test]
    fn musicvar_is_parsed() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="A"><musicvar feat="true" arrange="0.75" /></entry>
            </document>"#,
            today(),
        )
        .unwrap();
        let mv = entry(&doc, "A-1").music_var.unwrap();
        assert_eq!(mv.feat, Some(true));
        assert_eq!(mv.arrange, Some(0.75));
        assert_eq!(mv.vocallyrics, None);
    }

    #[test]
    fn script_element_warns_but_does_not_fail_parse() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="A"><script src="x.js" /><cry emotions="MU" /></entry>
            </document>"#,
            today(),
        )
        .unwrap();
        assert_eq!(entry(&doc, "A-1").impacts.len(), 1);
        assert!(doc.warnings.iter().any(|w| w.contains("<script>")));
    }

    #[test]
    fn emotions_bare_code_defaults_to_full_weight() {
        assert_eq!(parse_emotions("MU"), vec![("MU".to_string(), 1.0)]);
    }

    #[test]
    fn emotions_split_parses_weighted_pairs() {
        assert_eq!(
            parse_emotions("CU-0.5:CP-0.5"),
            vec![("CU".to_string(), 0.5), ("CP".to_string(), 0.5)]
        );
    }

    #[test]
    fn duration_minutes_parses_mm_ss_and_hh_mm_ss() {
        assert_eq!(parse_duration_minutes("5:42"), Some(5.0 + 42.0 / 60.0));
        assert_eq!(parse_duration_minutes("1:05:42"), Some(65.0 + 42.0 / 60.0));
    }
}
